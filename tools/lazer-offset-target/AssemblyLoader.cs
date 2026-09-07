using System.Reflection;
using System.Globalization;
using System.Runtime.Loader;

namespace LazerOffsetTarget;

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
