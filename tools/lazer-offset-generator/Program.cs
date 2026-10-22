using Microsoft.Win32.SafeHandles;
using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Encodings.Web;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using Microsoft.Diagnostics.Runtime;

namespace LazerOffsetGenerator;

internal static partial class Program
{
    private const int MaximumReadyLineLength = 256;
    private const int MaximumTargetStderrLength = 4096;
    private static readonly TimeSpan TargetStartupTimeout = TimeSpan.FromSeconds(30);
    private static readonly TimeSpan TargetDiagnosticDrainTimeout = TimeSpan.FromSeconds(2);

    private const string Usage = """
        Usage: lazer-offset-generator --target-host <executable> --assembly-dir <path> \
            --dac <matching libmscordaccore.so> --template <path> [--output <path>]
        Usage: lazer-offset-generator --pid <lazer-process-id> \
            --dac <matching libmscordaccore.so> --template <path> [--output <path>]
        Usage: lazer-offset-generator --assemblies-only --assembly-dir <path> \
            --template <path> [--output <path>]

        The first form launches the target host as a direct child. The second form
        attaches to the already-running lazer process. Both forms replace every
        offset in the ordered template. The first form requires the pinned Satori
        DAC markers; the live-process form requires a DAC matching the target CLR.
        The third form measures managed field offsets from on-disk assemblies without
        a live DAC attach (preferred on Satori-based official AppImages).
        JSON is written to stdout unless --output is supplied.
        """;

    public static int Main(string[] args)
    {
        try
        {
            CommandLine options = CommandLine.Parse(args);
            if (options.ShowHelp)
            {
                Console.Out.WriteLine(Usage);
                return 0;
            }

            TemplateSchema template = TemplateSchema.Load(options.TemplatePath!);
            byte[] json;
            if (options.AssembliesOnly)
            {
                json = AssemblyOffsets.Generate(
                    options.AssemblyDirectory!,
                    template,
                    osuVersion: null);
            }
            else
            {
                using ValidatedDac dac = options.ProcessId is null
                    ? OpenValidatedDac(options.DacPath!)
                    : OpenLiveDac(options.DacPath!);

                if (options.ProcessId is int processId)
                {
                    json = Serialize(
                        template,
                        Generate(processId, dac.LoaderPath, template));
                }
                else
                {
                    using PinnedRuntimeFiles runtimeFiles =
                        PinnedRuntimeFiles.Open(options.TargetHostPath!);
                    OwnedTargetHost targetHost = OwnedTargetHost.Start(
                        options.TargetHostPath!,
                        options.TemplatePath!,
                        options.AssemblyDirectory!,
                        runtimeFiles.IdentityToken,
                        template.ClrTypeCount,
                        template.SourceSha256);
                    try
                    {
                        try
                        {
                            json = Serialize(
                                template,
                                Generate(targetHost.ProcessId, dac.LoaderPath, template));
                        }
                        catch (GeneratorException error)
                        {
                            string? exitDiagnostic = targetHost.TryDescribeExit();
                            if (exitDiagnostic is null)
                                throw;

                            throw new GeneratorException($"{error.Message}; {exitDiagnostic}", error);
                        }
                    }
                    finally
                    {
                        targetHost.TerminateAndReap();
                    }
                    runtimeFiles.VerifyUnchanged();
                }
            }

            if (options.OutputPath is null)
                WriteStdout(json);
            else
                WriteAtomically(options.OutputPath, json);

            return 0;
        }
        catch (UsageException error)
        {
            Console.Error.WriteLine($"error: {error.Message}");
            Console.Error.WriteLine(Usage);
            return 2;
        }
        catch (GeneratorException error)
        {
            Console.Error.WriteLine($"error: {error.Message}");
            return 1;
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"error: unexpected {error.GetType().Name}: {error.Message}");
            return 1;
        }
    }


    private static GenerationResult Generate(
        int processId,
        string dacPath,
        TemplateSchema template)
    {
        DataTarget target;
        try
        {
            target = DataTarget.AttachToProcess(processId, suspend: true, new DataTargetOptions());
        }
        catch (Exception error)
        {
            throw new GeneratorException(
                $"could not attach to live process {processId} with ClrMD: {error.Message}",
                error);
        }

        try
        {
            using (target)
            {
                if (target.DataReader.PointerSize != 8)
                {
                    throw new GeneratorException(
                        $"process {processId} has unsupported {target.DataReader.PointerSize}-byte pointers; " +
                        "vosu lazer offsets require a 64-bit target");
                }

                if (target.ClrVersions.Length == 0)
                    throw new GeneratorException($"process {processId} has no loaded CLR runtime");

                if (target.ClrVersions.Length != 1)
                {
                    throw new GeneratorException(
                        $"process {processId} has {target.ClrVersions.Length} loaded CLR runtimes; " +
                        "runtime selection would be ambiguous");
                }

                using ClrRuntime runtime = CreateRuntime(
                    target.ClrVersions[0],
                    processId,
                    dacPath);
                ClrModule[] modules = runtime.EnumerateModules().ToArray();
                if (modules.Length == 0)
                    throw new GeneratorException($"the CLR in process {processId} exposes no loaded modules");

                Dictionary<string, List<TypeCandidate>> typeIndex =
                    IndexModuleTypes(template, runtime, modules);

                Dictionary<string, ResolvedType> resolvedTypes = new(StringComparer.Ordinal);
                foreach (TemplateEntry entry in template.Entries)
                {
                    if (entry.Kind != TemplateEntryKind.ClrType)
                        continue;

                    ClrType type = ResolveType(entry.Name, typeIndex[entry.Name]);
                    Dictionary<string, int> fields = new(StringComparer.Ordinal);
                    foreach (string fieldName in entry.Fields)
                    {
                        fields.Add(
                            fieldName,
                            type.IsEnum ? type.AsEnum().GetEnumValue<int>(fieldName)
                                : ResolveObjectFieldOffset(type, entry.Name, fieldName, target.DataReader.PointerSize));
                    }

                    resolvedTypes.Add(entry.Name, new ResolvedType(type, fields));
                }

                ResolvedType gameType = resolvedTypes[TemplateSchema.GameVtableTypeName];
                ulong methodTable = gameType.Type.MethodTable;
                if (methodTable == 0)
                {
                    throw new GeneratorException(
                        $"resolved type '{TemplateSchema.GameVtableTypeName}' has a zero method table");
                }

                if (!target.DataReader.ReadPointer(methodTable, out ulong gameBaseVtable))
                {
                    throw new GeneratorException(
                        $"could not read the 8-byte method-table word at 0x{methodTable:x} for " +
                        $"'{TemplateSchema.GameVtableTypeName}'");
                }

                if (gameBaseVtable == 0)
                {
                    throw new GeneratorException(
                        $"the method-table word at 0x{methodTable:x} for " +
                        $"'{TemplateSchema.GameVtableTypeName}' is zero");
                }

                string osuVersion = ResolveOsuVersion(modules, gameType.Type.Module);
                return new GenerationResult(osuVersion, gameBaseVtable, resolvedTypes);
            }
        }
        catch (GeneratorException)
        {
            throw;
        }
        catch (Exception error)
        {
            throw new GeneratorException($"failed while inspecting process {processId}: {error.Message}", error);
        }
    }

    private static ClrRuntime CreateRuntime(
        ClrInfo clrInfo,
        int processId,
        string dacPath)
    {
        try
        {
            return clrInfo.CreateRuntime(Path.GetFullPath(dacPath));
        }
        catch (Exception error)
        {
            throw new GeneratorException(
                $"could not create the CLR runtime for process {processId} with its matching DAC: " +
                $"{error.Message}",
                error);
        }
    }





    private static Dictionary<string, List<TypeCandidate>> IndexModuleTypes(
        TemplateSchema template,
        ClrRuntime runtime,
        IReadOnlyList<ClrModule> modules)
    {
        Dictionary<string, List<TypeCandidate>> index = CreateEmptyTypeIndex(template);

        foreach (ClrModule module in modules)
        {
            foreach ((ulong methodTable, int token) in module.EnumerateTypeDefToMethodTableMap())
            {
                if (methodTable == 0)
                    continue;

                ClrType? candidate = runtime.GetTypeByMethodTable(methodTable);
                string? name = candidate?.Name;
                if (candidate is null || name is null ||
                    !index.TryGetValue(name, out List<TypeCandidate>? matches))
                {
                    continue;
                }

                if (candidate.MethodTable != methodTable)
                {
                    throw new GeneratorException(
                        $"required typedef token 0x{token:x8} in '{DescribeModule(module)}' resolved invalid " +
                        $"method table 0x{candidate.MethodTable:x}; expected 0x{methodTable:x}");
                }

                if (candidate.Module.Address != module.Address)
                {
                    throw new GeneratorException(
                        $"required type '{name}' at method table 0x{methodTable:x} in " +
                        $"'{DescribeModule(module)}' resolved from unexpected module " +
                        $"'{DescribeModule(candidate.Module)}'");
                }

                if (matches.All(match =>
                        match.Type.MethodTable != methodTable ||
                        match.Module.Address != module.Address ||
                        match.Token != token))
                {
                    matches.Add(new TypeCandidate(candidate, module, token));
                }
            }
        }

        return index;
    }


    private static Dictionary<string, List<TypeCandidate>> CreateEmptyTypeIndex(
        TemplateSchema template)
    {
        Dictionary<string, List<TypeCandidate>> index = new(StringComparer.Ordinal);
        foreach (TemplateEntry entry in template.Entries)
        {
            if (entry.Kind == TemplateEntryKind.ClrType)
                index.Add(entry.Name, []);
        }

        return index;
    }

    private static ClrType ResolveType(string typeName, IReadOnlyList<TypeCandidate> matches)
    {
        if (matches.Count == 0)
        {
            throw new GeneratorException(
                $"could not resolve constructed CLR type '{typeName}' through loaded module typedef maps");
        }

        if (matches.Count != 1)
        {
            string locations = string.Join(
                ", ",
                matches
                    .Select(match =>
                        $"{DescribeModule(match.Module)} token 0x{match.Token:x8} " +
                        $"(method table 0x{match.Type.MethodTable:x})")
                    .Order(StringComparer.Ordinal));
            throw new GeneratorException(
                $"CLR type '{typeName}' is ambiguous across {matches.Count} live definitions: {locations}");
        }

        return matches[0].Type;
    }

    private static int ResolveObjectFieldOffset(
        ClrType type,
        string typeName,
        string fieldName,
        int pointerSize)
    {
        HashSet<ulong> visited = [];
        for (ClrType? current = type; current is not null; current = current.BaseType)
        {
            if (current.MethodTable == 0)
            {
                throw new GeneratorException(
                    $"ClrMD returned zero method table while resolving '{typeName}.{fieldName}'");
            }

            if (!visited.Add(current.MethodTable))
            {
                throw new GeneratorException(
                    $"base-type cycle encountered while resolving '{typeName}.{fieldName}'");
            }

            ClrInstanceField[] instanceMatches = current.Fields
                .Where(field =>
                    field.ContainingType.MethodTable == current.MethodTable &&
                    string.Equals(field.Name, fieldName, StringComparison.Ordinal))
                .ToArray();
            ClrStaticField[] staticMatches = current.StaticFields
                .Where(field =>
                    field.ContainingType.MethodTable == current.MethodTable &&
                    string.Equals(field.Name, fieldName, StringComparison.Ordinal))
                .ToArray();
            ClrThreadStaticField[] threadStaticMatches = current.ThreadStaticFields
                .Where(field =>
                    field.ContainingType.MethodTable == current.MethodTable &&
                    string.Equals(field.Name, fieldName, StringComparison.Ordinal))
                .ToArray();

            int totalMatches =
                instanceMatches.Length + staticMatches.Length + threadStaticMatches.Length;
            if (totalMatches == 0)
                continue;

            if (totalMatches != 1)
            {
                throw new GeneratorException(
                    $"field '{typeName}.{fieldName}' is ambiguous on declaring type " +
                    $"'{current.Name}': found {instanceMatches.Length} instance, " +
                    $"{staticMatches.Length} static, and " +
                    $"{threadStaticMatches.Length} thread-static matches");
            }

            if (instanceMatches.Length == 0)
            {
                string kind = staticMatches.Length == 1 ? "static" : "thread-static";
                throw new GeneratorException(
                    $"field '{typeName}.{fieldName}' is {kind} on declaring type " +
                    $"'{current.Name}'; the template may contain only instance fields");
            }

            int metadataOffset = instanceMatches[0].Offset;
            if (metadataOffset < 0)
            {
                throw new GeneratorException(
                    $"ClrMD returned invalid offset {metadataOffset} for " +
                    $"field '{typeName}.{fieldName}'");
            }

            try
            {
                // ClrMD's instance offset is relative to the managed object's data. vosu adds
                // its stored offset directly to ClrObject.Address, which starts at the MT word.
                return checked(metadataOffset + pointerSize);
            }
            catch (OverflowException error)
            {
                throw new GeneratorException(
                    $"offset overflow for field '{typeName}.{fieldName}'",
                    error);
            }
        }

        throw new GeneratorException(
            $"could not resolve field '{typeName}.{fieldName}' in the type or its base types");
    }

    private static string ResolveOsuVersion(IReadOnlyList<ClrModule> modules, ClrModule gameModule)
    {
        SortedSet<string> depsPaths = new(StringComparer.Ordinal);
        foreach (ClrModule module in modules)
        {
            string? modulePath = GetExistingModulePath(module);
            string? directory = modulePath is null ? null : Path.GetDirectoryName(modulePath);
            if (!string.IsNullOrEmpty(directory))
            {
                string depsPath = Path.Combine(directory, "osu!.deps.json");
                if (File.Exists(depsPath))
                    depsPaths.Add(Path.GetFullPath(depsPath));
            }
        }

        HashSet<string> depsVersions = new(StringComparer.Ordinal);
        foreach (string depsPath in depsPaths)
        {
            foreach (string version in ReadVersionsFromDepsFile(depsPath))
                depsVersions.Add(version);
        }

        if (depsVersions.Count > 1)
        {
            throw new GeneratorException(
                $"mounted osu!.deps.json files disagree on OsuVersion: " +
                string.Join(", ", depsVersions.Order(StringComparer.Ordinal)));
        }

        if (depsVersions.Count == 1)
            return depsVersions.Single();

        ClrModule[] mainModules = modules
            .Where(module =>
            {
                string? path = GetExistingModulePath(module);
                return path is not null &&
                       string.Equals(Path.GetFileName(path), "osu!.dll", StringComparison.OrdinalIgnoreCase);
            })
            .ToArray();

        if (mainModules.Length == 0)
            mainModules = [gameModule];

        HashSet<string> metadataVersions = new(StringComparer.Ordinal);
        foreach (ClrModule module in mainModules)
        {
            string? version = TryReadModuleVersion(module);
            if (version is not null)
                metadataVersions.Add(version);
        }

        if (metadataVersions.Count > 1)
        {
            throw new GeneratorException(
                $"loaded osu! module metadata disagrees on OsuVersion: " +
                string.Join(", ", metadataVersions.Order(StringComparer.Ordinal)));
        }

        if (metadataVersions.Count == 1)
            return metadataVersions.Single();

        string searched = depsPaths.Count == 0
            ? "no mounted osu!.deps.json was found beside the loaded modules"
            : $"no osu!/version entry was found in {string.Join(", ", depsPaths)}";
        throw new GeneratorException(
            $"could not derive canonical OsuVersion from loaded osu! module metadata; {searched}");
    }

    private static IEnumerable<string> ReadVersionsFromDepsFile(string path)
    {
        JsonDocument document;
        try
        {
            document = JsonDocument.Parse(File.ReadAllBytes(path));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException)
        {
            throw new GeneratorException($"could not read mounted deps file '{path}': {error.Message}", error);
        }

        using (document)
        {
            if (document.RootElement.ValueKind != JsonValueKind.Object)
                throw new GeneratorException($"mounted deps file '{path}' does not contain a JSON object");

            HashSet<string> versions = new(StringComparer.Ordinal);
            if (document.RootElement.TryGetProperty("libraries", out JsonElement libraries))
                CollectDepsVersions(libraries, path, versions);

            if (document.RootElement.TryGetProperty("targets", out JsonElement targets) &&
                targets.ValueKind == JsonValueKind.Object)
            {
                foreach (JsonProperty target in targets.EnumerateObject())
                    CollectDepsVersions(target.Value, path, versions);
            }

            return versions.ToArray();
        }
    }

    private static void CollectDepsVersions(
        JsonElement container,
        string path,
        ISet<string> versions)
    {
        if (container.ValueKind != JsonValueKind.Object)
            return;

        foreach (JsonProperty property in container.EnumerateObject())
        {
            const string prefix = "osu!/";
            if (!property.Name.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
                continue;

            string rawVersion = property.Name[prefix.Length..];
            string? canonical = CanonicalizeVersion(rawVersion);
            if (canonical is null)
            {
                throw new GeneratorException(
                    $"mounted deps file '{path}' has non-canonical osu! version '{rawVersion}'");
            }

            versions.Add(canonical);
        }
    }

    private static string? TryReadModuleVersion(ClrModule module)
    {
        string? path = GetExistingModulePath(module);
        if (path is not null)
        {
            try
            {
                FileVersionInfo info = FileVersionInfo.GetVersionInfo(path);
                string? productVersion = CanonicalizeVersion(info.ProductVersion);
                if (productVersion is not null)
                    return productVersion;

                string? fileVersion = CanonicalizeVersion(info.FileVersion);
                if (fileVersion is not null)
                    return fileVersion;
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                // Fall through to assembly metadata from the same live module.
            }

            try
            {
                string? assemblyVersion = CanonicalizeVersion(
                    AssemblyName.GetAssemblyName(path).Version?.ToString());
                if (assemblyVersion is not null)
                    return assemblyVersion;
            }
            catch (Exception error) when (
                error is IOException or UnauthorizedAccessException or BadImageFormatException)
            {
                // Fall through to ClrMD's assembly identity.
            }
        }

        return CanonicalizeVersion(module.AssemblyName);
    }

    private static string? CanonicalizeVersion(string? value)
    {
        if (string.IsNullOrWhiteSpace(value))
            return null;

        HashSet<string> versions = new(StringComparer.Ordinal);
        foreach (Match match in VersionPattern().Matches(value))
        {
            if (!ulong.TryParse(match.Groups["major"].Value, NumberStyles.None, CultureInfo.InvariantCulture, out ulong major) ||
                !ulong.TryParse(match.Groups["minor"].Value, NumberStyles.None, CultureInfo.InvariantCulture, out ulong minor) ||
                !ulong.TryParse(match.Groups["patch"].Value, NumberStyles.None, CultureInfo.InvariantCulture, out ulong patch))
            {
                continue;
            }

            versions.Add(string.Concat(
                major.ToString(CultureInfo.InvariantCulture), ".",
                minor.ToString(CultureInfo.InvariantCulture), ".",
                patch.ToString(CultureInfo.InvariantCulture)));
        }

        return versions.Count == 1 ? versions.Single() : null;
    }

    private static string? GetExistingModulePath(ClrModule module)
    {
        foreach (string? candidate in new[] { module.Name, module.AssemblyName })
        {
            if (string.IsNullOrWhiteSpace(candidate))
                continue;

            try
            {
                if (File.Exists(candidate))
                    return Path.GetFullPath(candidate);
            }
            catch (Exception error) when (error is ArgumentException or NotSupportedException)
            {
                // This candidate is an assembly identity rather than a file path.
            }
        }

        return null;
    }

    private static string DescribeModule(ClrModule module) =>
        module.Name ?? module.AssemblyName ?? $"module@0x{module.Address:x}";

    private static byte[] Serialize(TemplateSchema template, GenerationResult result)
    {
        using MemoryStream stream = new();
        using (Utf8JsonWriter writer = new(
                   stream,
                   new JsonWriterOptions
                   {
                       Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
                       Indented = false,
                       SkipValidation = false,
                   }))
        {
            writer.WriteStartObject();
            foreach (TemplateEntry entry in template.Entries)
            {
                switch (entry.Kind)
                {
                    case TemplateEntryKind.OsuVersion:
                        writer.WriteString(entry.Name, result.OsuVersion);
                        break;
                    case TemplateEntryKind.GameBaseVtable:
                        writer.WriteNumber(entry.Name, result.GameBaseVtable);
                        break;
                    case TemplateEntryKind.ClrType:
                        writer.WritePropertyName(entry.Name);
                        writer.WriteStartObject();
                        IReadOnlyDictionary<string, int> fields = result.Types[entry.Name].Fields;
                        foreach (string fieldName in entry.Fields)
                            writer.WriteNumber(fieldName, fields[fieldName]);
                        writer.WriteEndObject();
                        break;
                    default:
                        throw new InvalidOperationException($"unknown template entry kind {entry.Kind}");
                }
            }

            writer.WriteEndObject();
        }
        stream.WriteByte((byte)'\n');

        return stream.ToArray();
    }

    private static void WriteStdout(ReadOnlySpan<byte> json)
    {
        try
        {
            Stream stdout = Console.OpenStandardOutput();
            stdout.Write(json);
            stdout.Flush();
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            throw new GeneratorException($"could not write generated JSON to stdout: {error.Message}", error);
        }
    }

    private static void WriteAtomically(string outputPath, ReadOnlySpan<byte> json)
    {
        string fullPath;
        try
        {
            fullPath = Path.GetFullPath(outputPath);
        }
        catch (Exception error) when (error is ArgumentException or NotSupportedException)
        {
            throw new GeneratorException($"invalid output path '{outputPath}': {error.Message}", error);
        }

        string? directory = Path.GetDirectoryName(fullPath);
        if (string.IsNullOrEmpty(directory) || !Directory.Exists(directory))
            throw new GeneratorException($"output directory does not exist for '{fullPath}'");

        string tempPath = Path.Combine(
            directory,
            $".{Path.GetFileName(fullPath)}.{Environment.ProcessId}.{Guid.NewGuid():N}.tmp");

        try
        {
            using (FileStream stream = new(
                       tempPath,
                       FileMode.CreateNew,
                       FileAccess.Write,
                       FileShare.None,
                       bufferSize: 4096,
                       FileOptions.WriteThrough))
            {
                stream.Write(json);
                stream.Flush(flushToDisk: true);
            }

            File.Move(tempPath, fullPath, overwrite: true);
        }
        catch (Exception error) when (
            error is IOException or UnauthorizedAccessException or NotSupportedException)
        {
            throw new GeneratorException(
                $"could not atomically replace output '{fullPath}': {error.Message}",
                error);
        }
        finally
        {
            try
            {
                File.Delete(tempPath);
            }
            catch
            {
                // Preserve the original generation error if cleanup itself fails.
            }
        }
    }

    [GeneratedRegex(
        @"(?<![\d.])(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?:\.\d+)?(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?(?![\d.])",
        RegexOptions.CultureInvariant)]
    private static partial Regex VersionPattern();

    private sealed record ResolvedType(ClrType Type, IReadOnlyDictionary<string, int> Fields);

    private sealed record TypeCandidate(ClrType Type, ClrModule Module, int Token);

    private sealed record GenerationResult(
        string OsuVersion,
        ulong GameBaseVtable,
        IReadOnlyDictionary<string, ResolvedType> Types);
}


