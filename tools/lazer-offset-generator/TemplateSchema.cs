using System.Text.Json;
using System.Security.Cryptography;
using System.Text.RegularExpressions;

namespace LazerOffsetGenerator;

internal enum TemplateEntryKind
{
    OsuVersion,
    ClrType,
    GameBaseVtable,
}

internal sealed record TemplateEntry(
    string Name,
    TemplateEntryKind Kind,
    IReadOnlyList<string> Fields);

internal sealed record TemplateSchema(
    IReadOnlyList<TemplateEntry> Entries,
    int ClrTypeCount,
    string SourceSha256)
{
    public const string GameBaseTypeName = "osu.Game.OsuGameBase";
    public const string GameVtableTypeName = "osu.Desktop.OsuGameDesktop";

    public static TemplateSchema Load(string path)
    {
        byte[] json;
        try
        {
            json = File.ReadAllBytes(path);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            throw new GeneratorException($"could not read template '{path}': {error.Message}", error);
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
            throw new GeneratorException($"template '{path}' is not valid JSON: {error.Message}", error);
        }

        using (document)
        {
            if (document.RootElement.ValueKind != JsonValueKind.Object)
                throw new GeneratorException($"template '{path}' root must be a JSON object");

            List<TemplateEntry> entries = [];
            HashSet<string> topLevelNames = new(StringComparer.Ordinal);
            bool hasVersion = false;
            bool hasVtable = false;
            bool hasGameBaseType = false;
            bool hasGameVtableType = false;

            foreach (JsonProperty property in document.RootElement.EnumerateObject())
            {
                if (string.IsNullOrEmpty(property.Name))
                    throw new GeneratorException($"template '{path}' contains an empty top-level key");

                if (!topLevelNames.Add(property.Name))
                    throw new GeneratorException($"template '{path}' repeats top-level key '{property.Name}'");

                if (property.Name == "OsuVersion")
                {
                    if (property.Value.ValueKind != JsonValueKind.String ||
                        ProgramVersion(property.Value.GetString()) is null)
                    {
                        throw new GeneratorException(
                            $"template '{path}' property 'OsuVersion' must be a canonical version string");
                    }

                    hasVersion = true;
                    entries.Add(new TemplateEntry(property.Name, TemplateEntryKind.OsuVersion, []));
                    continue;
                }

                if (property.Name == "GameBaseVtable")
                {
                    RequireIntegerPlaceholder(path, property.Name, property.Value);
                    hasVtable = true;
                    entries.Add(new TemplateEntry(property.Name, TemplateEntryKind.GameBaseVtable, []));
                    continue;
                }

                if (property.Value.ValueKind != JsonValueKind.Object)
                {
                    throw new GeneratorException(
                        $"template '{path}' property '{property.Name}' must be an object of numeric field placeholders");
                }

                List<string> fields = [];
                HashSet<string> fieldNames = new(StringComparer.Ordinal);
                foreach (JsonProperty field in property.Value.EnumerateObject())
                {
                    if (string.IsNullOrEmpty(field.Name))
                        throw new GeneratorException($"template type '{property.Name}' contains an empty field key");

                    if (!fieldNames.Add(field.Name))
                    {
                        throw new GeneratorException(
                            $"template type '{property.Name}' repeats field key '{field.Name}'");
                    }

                    RequireIntegerPlaceholder(path, $"{property.Name}.{field.Name}", field.Value);
                    fields.Add(field.Name);
                }

                if (fields.Count == 0)
                    throw new GeneratorException($"template type '{property.Name}' contains no fields");

                if (property.Name == GameBaseTypeName)
                    hasGameBaseType = true;
                if (property.Name == GameVtableTypeName)
                    hasGameVtableType = true;

                entries.Add(new TemplateEntry(property.Name, TemplateEntryKind.ClrType, fields));
            }

            if (!hasVersion)
                throw new GeneratorException($"template '{path}' is missing required property 'OsuVersion'");
            if (!hasGameBaseType)
                throw new GeneratorException($"template '{path}' is missing required type '{GameBaseTypeName}'");
            if (!hasGameVtableType)
                throw new GeneratorException($"template '{path}' is missing required type '{GameVtableTypeName}'");
            if (!hasVtable)
                throw new GeneratorException($"template '{path}' is missing required property 'GameBaseVtable'");

            int clrTypeCount = entries.Count(entry => entry.Kind == TemplateEntryKind.ClrType);
            string sourceSha256 = Convert.ToHexString(SHA256.HashData(json)).ToLowerInvariant();
            return new TemplateSchema(entries, clrTypeCount, sourceSha256);
        }
    }

    private static void RequireIntegerPlaceholder(string path, string key, JsonElement value)
    {
        if (value.ValueKind != JsonValueKind.Number ||
            (!value.TryGetInt64(out _) && !value.TryGetUInt64(out _)))
        {
            throw new GeneratorException(
                $"template '{path}' property '{key}' must be an integer numeric placeholder");
        }
    }

    private static string? ProgramVersion(string? value)
    {
        if (string.IsNullOrWhiteSpace(value))
            return null;

        Match match = Regex.Match(
            value,
            @"\A[0-9]+\.[0-9]+\.[0-9]+\z",
            RegexOptions.CultureInvariant,
            TimeSpan.FromSeconds(1));
        return match.Success ? value : null;
    }
}
