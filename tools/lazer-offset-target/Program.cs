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

        Load a verified managed assembly set, construct every CLR type named by the
        ordered offsets template, and remain alive for ClrMD inspection.
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

internal sealed record RetainedHostState(
    DirectoryAssemblyLoader Loader,
    LoadedAssembly[] Assemblies,
    ResolvedType[] Types,
    RuntimeTypeHandle[] TypeHandles);

internal sealed record LoadedAssembly(string Path, Assembly Assembly);

internal sealed record ResolvedType(string Name, Type Type, string AssemblyPath);

internal sealed class TargetAssemblyLoadContext : AssemblyLoadContext
{
    public TargetAssemblyLoadContext()
        : base("lazer-offset-target", isCollectible: false)
    {
    }

    protected override Assembly? Load(AssemblyName assemblyName) => null;
}

internal sealed class DirectoryAssemblyLoader
{
    private readonly AssemblyLoadContext _loadContext = new TargetAssemblyLoadContext();
    private readonly ManagedCandidate[] _candidates;
    private readonly Dictionary<string, List<ManagedCandidate>> _candidatesBySimpleName;
    private readonly Dictionary<string, Assembly> _loadedByPath = new(StringComparer.Ordinal);
    private readonly object _loadGate = new();

    private DirectoryAssemblyLoader(string directoryPath, ManagedCandidate[] candidates)
    {
        DirectoryPath = directoryPath;
        _candidates = candidates;
        _candidatesBySimpleName = new Dictionary<string, List<ManagedCandidate>>(
            StringComparer.OrdinalIgnoreCase);

        foreach (ManagedCandidate candidate in candidates)
        {
            string simpleName = candidate.Identity.Name!;
            if (!_candidatesBySimpleName.TryGetValue(simpleName, out List<ManagedCandidate>? matches))
            {
                matches = [];
                _candidatesBySimpleName.Add(simpleName, matches);
            }

            matches.Add(candidate);
        }

        _loadContext.Resolving += ResolveFromDirectory;
    }

    public string DirectoryPath { get; }

    public static DirectoryAssemblyLoader Create(string directoryPath)
    {
        string fullDirectoryPath;
        try
        {
            fullDirectoryPath = Path.GetFullPath(directoryPath);
        }
        catch (Exception error) when (error is ArgumentException or
                                             NotSupportedException or
                                             PathTooLongException)
        {
            throw new TargetHostException(
                $"assembly loading failed: assembly directory path '{directoryPath}' is invalid: " +
                error.Message,
                error);
        }

        if (!Directory.Exists(fullDirectoryPath))
        {
            throw new TargetHostException(
                $"assembly loading failed: assembly directory '{fullDirectoryPath}' does not exist");
        }

        string[] dllPaths;
        try
        {
            dllPaths = Directory.GetFiles(
                fullDirectoryPath,
                "*.dll",
                SearchOption.TopDirectoryOnly);
            Array.Sort(dllPaths, StringComparer.Ordinal);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            throw new TargetHostException(
                $"assembly loading failed: could not enumerate DLL candidates in " +
                $"'{fullDirectoryPath}': {error.Message}",
                error);
        }

        List<ManagedCandidate> managedCandidates = [];
        int nonManagedCandidateCount = 0;
        foreach (string dllPath in dllPaths)
        {
            AssemblyName identity;
            try
            {
                identity = AssemblyName.GetAssemblyName(dllPath);
            }
            catch (BadImageFormatException)
            {
                nonManagedCandidateCount++;
                continue;
            }
            catch (Exception error) when (error is IOException or
                                                 UnauthorizedAccessException or
                                                 ArgumentException)
            {
                throw new TargetHostException(
                    $"assembly loading failed while inspecting DLL candidate " +
                    $"'{dllPath}': {error.Message}",
                    error);
            }

            if (string.IsNullOrWhiteSpace(identity.Name))
            {
                throw new TargetHostException(
                    $"assembly loading failed: managed DLL candidate '{dllPath}' has no assembly name");
            }

            managedCandidates.Add(new ManagedCandidate(dllPath, identity));
        }

        if (managedCandidates.Count == 0)
        {
            throw new TargetHostException(
                $"assembly loading failed: directory '{fullDirectoryPath}' contains no managed " +
                $"DLL candidates ({nonManagedCandidateCount.ToString(CultureInfo.InvariantCulture)} " +
                "non-managed DLL candidates were ignored)");
        }

        return new DirectoryAssemblyLoader(fullDirectoryPath, managedCandidates.ToArray());
    }

    public LoadedAssembly[] LoadAll()
    {
        List<LoadedAssembly> loadedAssemblies = new(_candidates.Length);

        foreach (ManagedCandidate candidate in _candidates)
        {
            Assembly assembly = LoadCandidate(candidate);
            bool alreadyRecorded = false;
            foreach (LoadedAssembly loaded in loadedAssemblies)
            {
                if (ReferenceEquals(loaded.Assembly, assembly))
                {
                    alreadyRecorded = true;
                    break;
                }
            }

            if (!alreadyRecorded)
                loadedAssemblies.Add(new LoadedAssembly(candidate.Path, assembly));
        }

        return loadedAssemblies.ToArray();
    }

    private Assembly? ResolveFromDirectory(
        AssemblyLoadContext loadContext,
        AssemblyName requestedIdentity)
    {
        if (!ReferenceEquals(loadContext, _loadContext) ||
            string.IsNullOrWhiteSpace(requestedIdentity.Name) ||
            !_candidatesBySimpleName.TryGetValue(
                requestedIdentity.Name,
                out List<ManagedCandidate>? sameNameCandidates))
        {
            return null;
        }

        ManagedCandidate? selected = null;
        foreach (ManagedCandidate candidate in sameNameCandidates)
        {
            if (!IdentitySatisfiesRequest(candidate.Identity, requestedIdentity))
                continue;

            if (selected is not null)
            {
                throw new TargetHostException(
                    $"assembly loading failed: dependency '{requestedIdentity.FullName}' is " +
                    $"ambiguous in assembly directory '{DirectoryPath}'; matching candidates are " +
                    $"'{selected.Path}' and '{candidate.Path}'");
            }

            selected = candidate;
        }

        if (selected is not null)
            return LoadCandidate(selected);

        if (sameNameCandidates.Count == 1)
            return LoadCandidate(sameNameCandidates[0]);

        List<string> candidateDescriptions = new(sameNameCandidates.Count);
        foreach (ManagedCandidate candidate in sameNameCandidates)
        {
            candidateDescriptions.Add(
                $"'{candidate.Path}' ({candidate.Identity.FullName ?? candidate.Identity.Name})");
        }

        throw new TargetHostException(
            $"assembly loading failed: dependency '{requestedIdentity.FullName}' cannot be " +
            "resolved uniquely from same-name candidates " +
            string.Join(", ", candidateDescriptions));
    }

    private Assembly LoadCandidate(ManagedCandidate candidate)
    {
        lock (_loadGate)
        {
            if (_loadedByPath.TryGetValue(candidate.Path, out Assembly? loaded))
                return loaded;

            Assembly assembly;
            try
            {
                assembly = _loadContext.LoadFromAssemblyPath(candidate.Path);
            }
            catch (Exception error) when (error is IOException or
                                                 UnauthorizedAccessException or
                                                 BadImageFormatException or
                                                 ArgumentException)
            {
                throw new TargetHostException(
                    $"assembly loading failed for managed DLL '{candidate.Path}' " +
                    $"({candidate.Identity.FullName ?? candidate.Identity.Name}): {error.Message}",
                    error);
            }

            AssemblyName loadedIdentity = assembly.GetName();
            if (!IdentityEquals(candidate.Identity, loadedIdentity))
            {
                throw new TargetHostException(
                    $"assembly loading failed for managed DLL '{candidate.Path}': requested " +
                    $"identity '{candidate.Identity.FullName}' but AssemblyLoadContext.Default " +
                    $"returned '{loadedIdentity.FullName}'");
            }

            _loadedByPath.Add(candidate.Path, assembly);
            return assembly;
        }
    }

    private static bool IdentitySatisfiesRequest(
        AssemblyName candidate,
        AssemblyName requested)
    {
        if (!string.Equals(candidate.Name, requested.Name, StringComparison.OrdinalIgnoreCase))
            return false;
        if (requested.Version is not null && candidate.Version != requested.Version)
            return false;
        if (!string.Equals(
                candidate.CultureName ?? string.Empty,
                requested.CultureName ?? string.Empty,
                StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        return PublicKeyTokensEqual(candidate.GetPublicKeyToken(), requested.GetPublicKeyToken());
    }

    private static bool IdentityEquals(AssemblyName left, AssemblyName right) =>
        string.Equals(left.Name, right.Name, StringComparison.OrdinalIgnoreCase) &&
        left.Version == right.Version &&
        string.Equals(
            left.CultureName ?? string.Empty,
            right.CultureName ?? string.Empty,
            StringComparison.OrdinalIgnoreCase) &&
        PublicKeyTokensEqual(left.GetPublicKeyToken(), right.GetPublicKeyToken());

    private static bool PublicKeyTokensEqual(byte[]? left, byte[]? right)
    {
        int leftLength = left?.Length ?? 0;
        int rightLength = right?.Length ?? 0;
        if (leftLength != rightLength)
            return false;

        for (int index = 0; index < leftLength; index++)
        {
            if (left![index] != right![index])
                return false;
        }

        return true;
    }

    private sealed record ManagedCandidate(string Path, AssemblyName Identity);
}

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

internal sealed record CommandLine(
    string? TemplatePath,
    string? AssemblyDirectory,
    bool ShowHelp)
{
    public static CommandLine Parse(string[] args)
    {
        if (args.Length == 1 && args[0] is "--help" or "-h")
            return new CommandLine(null, null, ShowHelp: true);

        string? templatePath = null;
        string? assemblyDirectory = null;

        for (int index = 0; index < args.Length; index++)
        {
            string argument = args[index];
            switch (argument)
            {
                case "--template":
                    if (templatePath is not null)
                        throw new UsageException("option '--template' may be supplied only once");
                    templatePath = ReadPath(args, ref index, argument);
                    break;
                case "--assembly-dir":
                    if (assemblyDirectory is not null)
                        throw new UsageException("option '--assembly-dir' may be supplied only once");
                    assemblyDirectory = ReadPath(args, ref index, argument);
                    break;
                case "--help":
                case "-h":
                    throw new UsageException($"option '{argument}' must be used alone");
                default:
                    throw new UsageException($"unknown argument '{argument}'");
            }
        }

        if (templatePath is null)
            throw new UsageException("missing required option '--template <offsets.json>'");
        if (assemblyDirectory is null)
        {
            throw new UsageException(
                "missing required option '--assembly-dir <Release output directory>'");
        }

        return new CommandLine(templatePath, assemblyDirectory, ShowHelp: false);
    }

    private static string ReadPath(string[] args, ref int index, string option)
    {
        if (index + 1 >= args.Length || IsOption(args[index + 1]))
            throw new UsageException($"option '{option}' requires a value");

        string value = args[++index];
        if (string.IsNullOrWhiteSpace(value))
            throw new UsageException($"option '{option}' requires a non-empty path");
        return value;
    }

    private static bool IsOption(string value) =>
        value is "--template" or "--assembly-dir" or "--help" or "-h";
}

internal sealed class UsageException : Exception
{
    public UsageException(string message)
        : base(message)
    {
    }
}

internal sealed class TargetHostException : Exception
{
    public TargetHostException(string message)
        : base(message)
    {
    }

    public TargetHostException(string message, Exception innerException)
        : base(message, innerException)
    {
    }
}
