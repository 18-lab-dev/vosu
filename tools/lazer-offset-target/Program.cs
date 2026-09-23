using System.Diagnostics;
using System.ComponentModel;
using System.Globalization;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Runtime.Loader;
using System.Security.Cryptography;
using System.Text.Json;

namespace LazerOffsetTarget;

internal static class Program
{
    private const string Usage = """
        Usage: lazer-offset-target --template <offsets.json> --assembly-dir <managed assembly directory>
        Usage: lazer-offset-target --game-base-signature --assembly-dir <managed assembly directory>

        Historical template mode loads every named CLR type and remains alive for ClrMD.
        Signature mode loads the exact osu.Desktop.OsuGameDesktop type and emits one JSON result.
        """;

    private const string ExpectedFrameworkDescription = ".NET 8.0.16";
    private const string ExpectedCoreLibAssemblyName = "System.Private.CoreLib.dll";
    private const string ExpectedCoreLibSha256 =
        "73c6ae7ff2ed535a249539f78731fc4412cf6c263ed9692fcf035f2cd01f96fa";
    private const string ExpectedJitModuleName = "libclrjit.so";
    private const string ExpectedJitSha256 =
        "3d58119a829d10d20f9fc31f2144f9202af19c93b412d5a5eeeeeba4b35ebb3e";
    private const string ExpectedCoreClrModuleName = "libcoreclr.so";
    private const string ExpectedCoreClrSha256 =
        "dcc631eda8fdb34d06956697e9709b8d828306eb73ac3187b5058180e1af615e";
    private const int LinuxStatBufferSize = 256;
    private const string RuntimeIdentityEnvironmentVariable =
        "VOSU_LAZER_OFFSET_RUNTIME_IDENTITY";

    [DllImport("libc", EntryPoint = "fstat", SetLastError = true)]
    private static extern int FStat(int fileDescriptor, IntPtr status);

    private static RetainedHostState? s_retainedState;

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

            if (options.GameBaseSignature)
                return EmitGameBaseSignature(options.AssemblyDirectory!);

            RuntimeIdentityProof runtimeIdentity = RuntimeIdentityProof.LoadRequired();
            VerifyRuntimeIdentity(runtimeIdentity);

            TemplateSchema template = TemplateSchema.Load(options.TemplatePath!);
            DirectoryAssemblyLoader loader = DirectoryAssemblyLoader.Create(options.AssemblyDirectory!);
            LoadedAssembly[] assemblies = loader.LoadAll();
            ResolvedType[] resolvedTypes = ResolveTypes(template.TypeNames, assemblies);
            RuntimeTypeHandle[] typeHandles = ActivateTypes(resolvedTypes);

            s_retainedState = new RetainedHostState(loader, assemblies, resolvedTypes, typeHandles);
            Console.Out.WriteLine(string.Create(
                CultureInfo.InvariantCulture,
                $"ready pid={Environment.ProcessId} types={resolvedTypes.Length} template_sha256={template.SourceSha256}"));
            Console.Out.Flush();

            Thread.Sleep(Timeout.Infinite);
            return 0;
        }
        catch (UsageException error)
        {
            Console.Error.WriteLine($"error: command line: {error.Message}");
            Console.Error.WriteLine(Usage);
            return 2;
        }
        catch (TargetHostException error)
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

    private static int EmitGameBaseSignature(string assemblyDirectory)
    {
        if (RuntimeInformation.ProcessArchitecture != Architecture.X64)
        {
            throw new TargetHostException(
                "game-base signature mode requires an X64 process architecture");
        }

        DirectoryAssemblyLoader loader = DirectoryAssemblyLoader.Create(assemblyDirectory);
        // Load only the game entry assembly. The loader resolves its dependencies;
        // bundled framework assemblies such as CoreLib must stay owned by the CLR.
        LoadedAssembly[] assemblies = [loader.Load("osu!")];
        ResolvedType resolved = ResolveTypes(
            ["osu.Desktop.OsuGameDesktop"],
            assemblies)[0];

        RuntimeTypeHandle handle;
        try
        {
            handle = resolved.Type.TypeHandle;
        }
        catch (Exception error) when (error is TypeLoadException or
                                             FileNotFoundException or
                                             FileLoadException or
                                             BadImageFormatException or
                                             NotSupportedException or
                                             InvalidOperationException)
        {
            throw new TargetHostException(
                $"game-base signature failed to obtain type handle for '{resolved.Name}' " +
                $"from '{resolved.AssemblyPath}': {error.Message}",
                error);
        }

        if (handle.Value == IntPtr.Zero)
            throw new TargetHostException(
                $"game-base signature failed for '{resolved.Name}': RuntimeTypeHandle.Value is zero");

        long methodTableWord = Marshal.ReadInt64(handle.Value);
        if (methodTableWord <= 0)
        {
            throw new TargetHostException(
                $"game-base signature failed for '{resolved.Name}': first type-handle word " +
                $"was {methodTableWord.ToString(CultureInfo.InvariantCulture)}");
        }

        string version = ReadVersionFromDeps(assemblyDirectory);
        Console.Out.WriteLine(JsonSerializer.Serialize(new
        {
            OsuVersion = version,
            GameBaseVtable = unchecked((ulong)methodTableWord),
        }));
        return 0;
    }

    private static string ReadVersionFromDeps(string assemblyDirectory)
    {
        string depsPath = Path.Combine(Path.GetFullPath(assemblyDirectory), "osu!.deps.json");
        using FileStream file = File.OpenRead(depsPath);
        if (file.Length > 4 * 1024 * 1024)
            throw new TargetHostException($"dependencies file '{depsPath}' exceeds 4 MiB");
        byte[] bytes = new byte[checked((int)file.Length)];
        file.ReadExactly(bytes);
        using JsonDocument document = JsonDocument.Parse(bytes);
        if (!document.RootElement.TryGetProperty("libraries", out JsonElement libraries) ||
            libraries.ValueKind != JsonValueKind.Object)
            throw new TargetHostException($"dependencies file '{depsPath}' has no object 'libraries'");

        foreach (JsonProperty library in libraries.EnumerateObject())
        {
            const string prefix = "osu!/";
            if (!library.Name.StartsWith(prefix, StringComparison.Ordinal))
                continue;
            string rest = library.Name[prefix.Length..];
            int separator = rest.IndexOf('-');
            if (separator <= 0 || separator == rest.Length - 1)
                continue;
            string version = rest[..separator];
            if (IsCanonicalVersion(version))
                return version;
            throw new TargetHostException($"invalid osu! version '{version}' in '{depsPath}'");
        }
        throw new TargetHostException($"dependencies file '{depsPath}' contains no canonical osu! version");
    }

    private static bool IsCanonicalVersion(string version)
    {
        string[] parts = version.Split('.');
        if (parts.Length != 3)
            return false;
        foreach (string part in parts)
        {
            if (part.Length == 0 ||
                (part.Length > 1 && part[0] == '0') ||
                !part.All(char.IsAsciiDigit) ||
                !ulong.TryParse(part, NumberStyles.None, CultureInfo.InvariantCulture, out _))
            {
                return false;
            }
        }

        return true;
    }

    private static ResolvedType[] ResolveTypes(
        IReadOnlyList<string> typeNames,
        IReadOnlyList<LoadedAssembly> assemblies)
    {
        ResolvedType[] resolved = new ResolvedType[typeNames.Count];

        for (int typeIndex = 0; typeIndex < typeNames.Count; typeIndex++)
        {
            string requestedName = typeNames[typeIndex];
            List<TypeMatch> matches = [];

            foreach (LoadedAssembly loaded in assemblies)
            {
                Type? candidate;
                try
                {
                    candidate = loaded.Assembly.GetType(
                        requestedName,
                        throwOnError: false,
                        ignoreCase: false);
                }
                catch (Exception error) when (error is ArgumentException or
                                                     TypeLoadException or
                                                     FileNotFoundException or
                                                     FileLoadException or
                                                     BadImageFormatException)
                {
                    throw new TargetHostException(
                        $"type resolution failed for '{requestedName}' while inspecting " +
                        $"assembly '{DescribeAssembly(loaded.Assembly)}' loaded from " +
                        $"'{loaded.Path}': {error.Message}",
                        error);
                }

                if (candidate is null)
                    continue;

                if (!string.Equals(candidate.FullName, requestedName, StringComparison.Ordinal))
                {
                    throw new TargetHostException(
                        $"type resolution failed for '{requestedName}': assembly " +
                        $"'{DescribeAssembly(loaded.Assembly)}' returned non-exact type " +
                        $"'{candidate.FullName ?? candidate.Name}'");
                }

                bool alreadyMatched = false;
                foreach (TypeMatch match in matches)
                {
                    if (match.Type == candidate)
                    {
                        alreadyMatched = true;
                        break;
                    }
                }

                if (!alreadyMatched)
                    matches.Add(new TypeMatch(candidate, loaded));
            }

            if (matches.Count == 0)
            {
                throw new TargetHostException(
                    $"type resolution failed: requested type '{requestedName}' was not found " +
                    $"in any of the {assemblies.Count.ToString(CultureInfo.InvariantCulture)} " +
                    "managed assemblies loaded from the assembly directory");
            }

            if (matches.Count > 1)
            {
                List<string> locations = new(matches.Count);
                foreach (TypeMatch match in matches)
                {
                    locations.Add(
                        $"'{DescribeAssembly(match.Type.Assembly)}' from '{match.Source.Path}'");
                }

                throw new TargetHostException(
                    $"type resolution is ambiguous for requested type '{requestedName}'; " +
                    $"exact definitions were found in {string.Join(", ", locations)}");
            }

            TypeMatch uniqueMatch = matches[0];
            string definingPath = FindAssemblyPath(uniqueMatch.Type.Assembly, assemblies) ??
                                  uniqueMatch.Source.Path;
            resolved[typeIndex] = new ResolvedType(requestedName, uniqueMatch.Type, definingPath);
        }

        return resolved;
    }

    private static RuntimeTypeHandle[] ActivateTypes(IReadOnlyList<ResolvedType> resolvedTypes)
    {
        RuntimeTypeHandle[] handles = new RuntimeTypeHandle[resolvedTypes.Count];

        for (int index = 0; index < resolvedTypes.Count; index++)
        {
            ResolvedType resolved = resolvedTypes[index];
            RuntimeTypeHandle handle;
            try
            {
                handle = resolved.Type.TypeHandle;
            }
            catch (Exception error) when (error is TypeLoadException or
                                                 FileNotFoundException or
                                                 FileLoadException or
                                                 BadImageFormatException or
                                                 NotSupportedException or
                                                 InvalidOperationException)
            {
                throw new TargetHostException(
                    $"type-handle activation failed for '{resolved.Name}' from " +
                    $"'{resolved.AssemblyPath}': {error.Message}",
                    error);
            }

            if (handle.Value == IntPtr.Zero)
            {
                throw new TargetHostException(
                    $"type-handle activation failed for '{resolved.Name}' from " +
                    $"'{resolved.AssemblyPath}': RuntimeTypeHandle.Value is zero");
            }

            handles[index] = handle;
        }

        return handles;
    }

    private static void VerifyRuntimeIdentity(RuntimeIdentityProof runtimeIdentity)
    {
        if (!OperatingSystem.IsLinux())
        {
            throw new TargetHostException(
                "runtime identity check failed: the pinned target host supports Linux only");
        }

        if (RuntimeInformation.ProcessArchitecture != Architecture.X64)
        {
            throw new TargetHostException(
                "runtime identity check failed: expected process architecture X64, found " +
                RuntimeInformation.ProcessArchitecture);
        }

        string frameworkDescription = RuntimeInformation.FrameworkDescription;
        if (!string.Equals(
                frameworkDescription,
                ExpectedFrameworkDescription,
                StringComparison.Ordinal))
        {
            throw new TargetHostException(
                $"runtime identity check failed: expected '{ExpectedFrameworkDescription}', " +
                $"found '{frameworkDescription}'");
        }

        string coreLibPath = typeof(object).Assembly.Location;
        if (!string.Equals(
                Path.GetFileName(coreLibPath),
                ExpectedCoreLibAssemblyName,
                StringComparison.Ordinal))
        {
            throw new TargetHostException(
                "runtime identity check failed: expected the loaded core library path to end " +
                $"in '{ExpectedCoreLibAssemblyName}', found '{coreLibPath}'");
        }
        VerifyMappedFileHash(
            coreLibPath,
            ExpectedCoreLibSha256,
            "loaded core library",
            runtimeIdentity.CoreLib);

        string[] loadedModulePaths;
        try
        {
            using Process process = Process.GetCurrentProcess();
            loadedModulePaths = process.Modules
                .Cast<ProcessModule>()
                .Select(module => module.FileName)
                .Distinct(StringComparer.Ordinal)
                .ToArray();
        }
        catch (Exception error) when (error is InvalidOperationException or
                                             NotSupportedException or
                                             Win32Exception or
                                             IOException or
                                             UnauthorizedAccessException)
        {
            throw new TargetHostException(
                "runtime identity check failed: could not enumerate loaded runtime modules: " +
                error.Message,
                error);
        }

        VerifyLoadedModule(
            loadedModulePaths,
            ExpectedJitModuleName,
            ExpectedJitSha256,
            runtimeIdentity.Jit);
        VerifyLoadedModule(
            loadedModulePaths,
            ExpectedCoreClrModuleName,
            ExpectedCoreClrSha256,
            runtimeIdentity.CoreClr);
    }

    private static void VerifyLoadedModule(
        IReadOnlyList<string> loadedModulePaths,
        string expectedName,
        string expectedSha256,
        RuntimeFileStamp expectedStamp)
    {
        string[] paths = loadedModulePaths
            .Where(path => string.Equals(
                Path.GetFileName(path),
                expectedName,
                StringComparison.Ordinal))
            .ToArray();
        if (paths.Length != 1)
        {
            string discoveredPaths = paths.Length == 0
                ? "none"
                : string.Join(", ", paths.Select(path => $"'{path}'"));
            throw new TargetHostException(
                "runtime identity check failed: expected exactly one loaded " +
                $"'{expectedName}' module, found {paths.Length}: {discoveredPaths}");
        }

        VerifyMappedFileHash(
            paths[0],
            expectedSha256,
            $"loaded '{expectedName}' module",
            expectedStamp);
    }

    private static void VerifyMappedFileHash(
        string path,
        string expectedSha256,
        string description,
        RuntimeFileStamp expectedStamp)
    {
        string fullPath;
        string actualSha256;
        try
        {
            fullPath = Path.GetFullPath(path);
            using FileStream stream = new(
                fullPath,
                FileMode.Open,
                FileAccess.Read,
                FileShare.Read);
            RuntimeFileStamp stamp = ReadRuntimeFileStamp(stream);
            if (stamp != expectedStamp)
            {
                throw new TargetHostException(
                    $"runtime identity check failed: {description} at '{fullPath}' has " +
                    $"identity {stamp}, expected the generator-validated {expectedStamp}");
            }

            FileIdentity identity = stamp.Identity;
            List<FileIdentity> mappedIdentities = [];
            foreach (string line in File.ReadLines("/proc/self/maps"))
            {
                if (TryReadMappingIdentity(line, fullPath, out FileIdentity mappedIdentity))
                    mappedIdentities.Add(mappedIdentity);
            }

            if (!mappedIdentities.Any(mappedIdentity =>
                    IsSameMappedFile(identity, mappedIdentity)))
            {
                string discovered = mappedIdentities.Count == 0
                    ? "none"
                    : string.Join(", ", mappedIdentities);
                throw new TargetHostException(
                    $"runtime identity check failed: {description} at '{fullPath}' was opened " +
                    $"as {identity}, but its loaded mappings use {discovered} (the path may " +
                    "have been replaced after startup)");
            }

            stream.Position = 0;
            actualSha256 = Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
            if (ReadRuntimeFileStamp(stream) != expectedStamp)
            {
                throw new TargetHostException(
                    $"runtime identity check failed: {description} at '{fullPath}' changed " +
                    "while its SHA-256 was being verified");
            }
        }
        catch (TargetHostException)
        {
            throw;
        }
        catch (Exception error) when (error is IOException or
                                             UnauthorizedAccessException or
                                             ArgumentException or
                                             NotSupportedException or
                                             Win32Exception or
                                             OverflowException)
        {
            throw new TargetHostException(
                $"runtime identity check failed: could not bind and hash {description} " +
                $"'{path}': {error.Message}",
                error);
        }

        if (!string.Equals(actualSha256, expectedSha256, StringComparison.Ordinal))
        {
            throw new TargetHostException(
                $"runtime identity check failed: {description} '{fullPath}' has SHA-256 " +
                $"'{actualSha256}', expected '{expectedSha256}'");
        }
    }

    private static bool IsSameMappedFile(FileIdentity opened, FileIdentity mapped) =>
        opened.DeviceMajor == mapped.DeviceMajor &&
        opened.DeviceMinor == mapped.DeviceMinor &&
        opened.Inode == mapped.Inode;

    private static bool TryReadMappingIdentity(
        string line,
        string expectedPath,
        out FileIdentity identity)
    {
        identity = default;
        string[] fields = line.Split(
            ' ',
            6,
            StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        if (fields.Length != 6 ||
            !string.Equals(fields[5], expectedPath, StringComparison.Ordinal))
        {
            return false;
        }

        string[] device = fields[3].Split(':', 2);
        if (device.Length != 2 ||
            !ulong.TryParse(
                device[0],
                NumberStyles.AllowHexSpecifier,
                CultureInfo.InvariantCulture,
                out ulong deviceMajor) ||
            !ulong.TryParse(
                device[1],
                NumberStyles.AllowHexSpecifier,
                CultureInfo.InvariantCulture,
                out ulong deviceMinor) ||
            !ulong.TryParse(
                fields[4],
                NumberStyles.None,
                CultureInfo.InvariantCulture,
                out ulong inode))
        {
            return false;
        }

        identity = new FileIdentity(deviceMajor, deviceMinor, inode);
        return true;
    }

    private static RuntimeFileStamp ReadRuntimeFileStamp(FileStream stream)
    {
        IntPtr status = Marshal.AllocHGlobal(LinuxStatBufferSize);
        try
        {
            int descriptor = checked(
                (int)stream.SafeFileHandle.DangerousGetHandle().ToInt64());
            if (FStat(descriptor, status) != 0)
                throw new Win32Exception(Marshal.GetLastPInvokeError());

            ulong device = unchecked((ulong)Marshal.ReadInt64(status, 0));
            ulong inode = unchecked((ulong)Marshal.ReadInt64(status, sizeof(long)));
            long size = Marshal.ReadInt64(status, 48);
            long changeSeconds = Marshal.ReadInt64(status, 104);
            long changeNanoseconds = Marshal.ReadInt64(status, 112);
            ulong deviceMajor =
                ((device >> 8) & 0xfffUL) | ((device >> 32) & ~0xfffUL);
            ulong deviceMinor =
                (device & 0xffUL) | ((device >> 12) & ~0xffUL);
            return new RuntimeFileStamp(
                deviceMajor,
                deviceMinor,
                inode,
                size,
                changeSeconds,
                changeNanoseconds);
        }
        finally
        {
            Marshal.FreeHGlobal(status);
        }
    }

    private readonly record struct FileIdentity(
        ulong DeviceMajor,
        ulong DeviceMinor,
        ulong Inode);

    private readonly record struct RuntimeFileStamp(
        ulong DeviceMajor,
        ulong DeviceMinor,
        ulong Inode,
        long Size,
        long ChangeSeconds,
        long ChangeNanoseconds)
    {
        public FileIdentity Identity => new(DeviceMajor, DeviceMinor, Inode);

        public static RuntimeFileStamp Parse(string value, string name)
        {
            string[] fields = value.Split(':', StringSplitOptions.TrimEntries);
            if (fields.Length != 6 ||
                !ulong.TryParse(fields[0], NumberStyles.None, CultureInfo.InvariantCulture, out ulong major) ||
                !ulong.TryParse(fields[1], NumberStyles.None, CultureInfo.InvariantCulture, out ulong minor) ||
                !ulong.TryParse(fields[2], NumberStyles.None, CultureInfo.InvariantCulture, out ulong inode) ||
                !long.TryParse(fields[3], NumberStyles.None, CultureInfo.InvariantCulture, out long size) ||
                !long.TryParse(fields[4], NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out long changeSeconds) ||
                !long.TryParse(fields[5], NumberStyles.None, CultureInfo.InvariantCulture, out long changeNanoseconds) ||
                size < 0 ||
                changeNanoseconds is < 0 or >= 1_000_000_000)
            {
                throw new TargetHostException(
                    $"runtime identity check failed: invalid generator identity for '{name}'");
            }

            return new RuntimeFileStamp(
                major,
                minor,
                inode,
                size,
                changeSeconds,
                changeNanoseconds);
        }
    }

    private sealed record RuntimeIdentityProof(
        RuntimeFileStamp CoreLib,
        RuntimeFileStamp Jit,
        RuntimeFileStamp CoreClr)
    {
        public static RuntimeIdentityProof LoadRequired()
        {
            string? token = Environment.GetEnvironmentVariable(
                RuntimeIdentityEnvironmentVariable);
            Environment.SetEnvironmentVariable(RuntimeIdentityEnvironmentVariable, null);
            if (string.IsNullOrWhiteSpace(token))
            {
                throw new TargetHostException(
                    "runtime identity check failed: the generator did not supply its " +
                    "pre-launch runtime identity");
            }

            Dictionary<string, RuntimeFileStamp> stamps =
                new(StringComparer.Ordinal);
            foreach (string entry in token.Split(
                         ';',
                         StringSplitOptions.RemoveEmptyEntries |
                         StringSplitOptions.TrimEntries))
            {
                int separator = entry.IndexOf('=');
                if (separator <= 0 || separator == entry.Length - 1)
                {
                    throw new TargetHostException(
                        "runtime identity check failed: invalid generator runtime identity");
                }

                string name = entry[..separator];
                if (!stamps.TryAdd(
                        name,
                        RuntimeFileStamp.Parse(entry[(separator + 1)..], name)))
                {
                    throw new TargetHostException(
                        $"runtime identity check failed: duplicate generator identity for '{name}'");
                }
            }

            if (stamps.Count != 3 ||
                !stamps.TryGetValue(ExpectedCoreLibAssemblyName, out RuntimeFileStamp coreLib) ||
                !stamps.TryGetValue(ExpectedJitModuleName, out RuntimeFileStamp jit) ||
                !stamps.TryGetValue(ExpectedCoreClrModuleName, out RuntimeFileStamp coreClr))
            {
                throw new TargetHostException(
                    "runtime identity check failed: generator identities must name exactly " +
                    $"'{ExpectedCoreLibAssemblyName}', '{ExpectedJitModuleName}', and " +
                    $"'{ExpectedCoreClrModuleName}'");
            }

            return new RuntimeIdentityProof(coreLib, jit, coreClr);
        }
    }

    private static string? FindAssemblyPath(
        Assembly requested,
        IReadOnlyList<LoadedAssembly> assemblies)
    {
        foreach (LoadedAssembly loaded in assemblies)
        {
            if (ReferenceEquals(requested, loaded.Assembly))
                return loaded.Path;
        }

        return null;
    }

    private static string DescribeAssembly(Assembly assembly) =>
        assembly.FullName ?? assembly.GetName().Name ?? "<unnamed assembly>";

    private sealed record TypeMatch(Type Type, LoadedAssembly Source);
}



