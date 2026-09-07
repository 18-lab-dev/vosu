using System.Text.Json;
using System.Security.Cryptography;

namespace LazerOffsetTarget;

internal sealed record TemplateSchema(string[] TypeNames, string SourceSha256)
{
    public const string GameBaseTypeName = "osu.Game.OsuGameBase";

    public static TemplateSchema Load(string path)
    {
        byte[] json;
        try
        {
            json = File.ReadAllBytes(path);
        }
        catch (Exception error) when (error is IOException or
                                             UnauthorizedAccessException or
                                             ArgumentException or
                                             NotSupportedException)
        {
            throw new TargetHostException(
                $"template error: could not read template '{path}': {error.Message}",
                error);
        }

        JsonDocument document;
        try
        {
            document = JsonDocument.Parse(
                json,
                new JsonDocumentOptions
                {
                    AllowTrailingCommas = false,
                    CommentHandling = JsonCommentHandling.Disallow,
                    MaxDepth = 8,
                });
        }
        catch (JsonException error)
        {
            throw new TargetHostException(
                $"template error: template '{path}' is not valid JSON: {error.Message}",
                error);
        }

        using (document)
        {
            if (document.RootElement.ValueKind != JsonValueKind.Object)
            {
                throw new TargetHostException(
                    $"template error: template '{path}' root must be a JSON object");
            }

            List<string> typeNames = [];
            HashSet<string> topLevelNames = new(StringComparer.Ordinal);
            bool hasVersion = false;
            bool hasVtable = false;
            bool hasGameBaseType = false;

            foreach (JsonProperty property in document.RootElement.EnumerateObject())
            {
                if (string.IsNullOrEmpty(property.Name))
                {
                    throw new TargetHostException(
                        $"template error: template '{path}' contains an empty top-level key");
                }

                if (!topLevelNames.Add(property.Name))
                {
                    throw new TargetHostException(
                        $"template error: template '{path}' repeats top-level key " +
                        $"'{property.Name}'");
                }

                if (property.Name == "OsuVersion")
                {
                    if (property.Value.ValueKind != JsonValueKind.String ||
                        !IsCanonicalVersion(property.Value.GetString()))
                    {
                        throw new TargetHostException(
                            $"template error: template '{path}' property 'OsuVersion' must be " +
                            "a canonical three-component numeric version string");
                    }

                    hasVersion = true;
                    continue;
                }

                if (property.Name == "GameBaseVtable")
                {
                    RequireIntegerPlaceholder(path, property.Name, property.Value);
                    hasVtable = true;
                    continue;
                }

                if (property.Value.ValueKind != JsonValueKind.Object)
                {
                    throw new TargetHostException(
                        $"template error: template '{path}' type '{property.Name}' must be an " +
                        "object of integer numeric field placeholders");
                }

                HashSet<string> fieldNames = new(StringComparer.Ordinal);
                int fieldCount = 0;
                foreach (JsonProperty field in property.Value.EnumerateObject())
                {
                    if (string.IsNullOrEmpty(field.Name))
                    {
                        throw new TargetHostException(
                            $"template error: template type '{property.Name}' contains an empty " +
                            "field key");
                    }

                    if (!fieldNames.Add(field.Name))
                    {
                        throw new TargetHostException(
                            $"template error: template type '{property.Name}' repeats field key " +
                            $"'{field.Name}'");
                    }

                    RequireIntegerPlaceholder(
                        path,
                        $"{property.Name}.{field.Name}",
                        field.Value);
                    fieldCount++;
                }

                if (fieldCount == 0)
                {
                    throw new TargetHostException(
                        $"template error: template type '{property.Name}' contains no fields");
                }

                if (property.Name == GameBaseTypeName)
                    hasGameBaseType = true;

                typeNames.Add(property.Name);
            }

            if (!hasVersion)
            {
                throw new TargetHostException(
                    $"template error: template '{path}' is missing required property 'OsuVersion'");
            }

            if (!hasGameBaseType)
            {
                throw new TargetHostException(
                    $"template error: template '{path}' is missing required type " +
                    $"'{GameBaseTypeName}'");
            }

            if (!hasVtable)
            {
                throw new TargetHostException(
                    $"template error: template '{path}' is missing required property " +
                    "'GameBaseVtable'");
            }

            string sourceSha256 = Convert.ToHexString(SHA256.HashData(json)).ToLowerInvariant();
            return new TemplateSchema(typeNames.ToArray(), sourceSha256);
        }
    }

    private static void RequireIntegerPlaceholder(
        string path,
        string propertyName,
        JsonElement value)
    {
        if (value.ValueKind != JsonValueKind.Number ||
            (!value.TryGetInt64(out _) && !value.TryGetUInt64(out _)))
        {
            throw new TargetHostException(
                $"template error: template '{path}' property '{propertyName}' must be an " +
                "integer numeric placeholder");
        }
    }

    private static bool IsCanonicalVersion(string? value)
    {
        if (string.IsNullOrEmpty(value))
            return false;

        int componentCount = 1;
        bool componentHasDigit = false;
        foreach (char character in value)
        {
            if (character is >= '0' and <= '9')
            {
                componentHasDigit = true;
                continue;
            }

            if (character != '.' || !componentHasDigit || componentCount == 3)
                return false;

            componentCount++;
            componentHasDigit = false;
        }

        return componentCount == 3 && componentHasDigit;
    }
}
