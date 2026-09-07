using System.Globalization;

namespace LazerOffsetGenerator;

internal sealed record CommandLine(
    string? TargetHostPath,
    int? ProcessId,
    string? DacPath,
    string? AssemblyDirectory,
    string? TemplatePath,
    string? OutputPath,
    bool AssembliesOnly,
    bool ShowHelp)
{
    public static CommandLine Parse(string[] args)
    {
        if (args.Length == 1 && args[0] is "--help" or "-h")
            return new CommandLine(null, null, null, null, null, null, AssembliesOnly: false, ShowHelp: true);

        string? targetHostPath = null;
        int? processId = null;
        string? dacPath = null;
        string? assemblyDirectory = null;
        string? templatePath = null;
        string? outputPath = null;
        bool assembliesOnly = false;

        for (int index = 0; index < args.Length; index++)
        {
            string argument = args[index];
            switch (argument)
            {
                case "--target-host":
                    if (targetHostPath is not null)
                        throw new UsageException("option '--target-host' may be supplied only once");
                    targetHostPath = ReadPath(args, ref index, argument);
                    break;
                case "--pid":
                    if (processId is not null)
                        throw new UsageException("option '--pid' may be supplied only once");
                    processId = ReadProcessId(args, ref index, argument);
                    break;
                case "--dac":
                    if (dacPath is not null)
                        throw new UsageException("option '--dac' may be supplied only once");
                    dacPath = ReadPath(args, ref index, argument);
                    break;
                case "--assembly-dir":
                    if (assemblyDirectory is not null)
                        throw new UsageException("option '--assembly-dir' may be supplied only once");
                    assemblyDirectory = ReadPath(args, ref index, argument);
                    break;
                case "--template":
                    if (templatePath is not null)
                        throw new UsageException("option '--template' may be supplied only once");
                    templatePath = ReadPath(args, ref index, argument);
                    break;
                case "--output":
                    if (outputPath is not null)
                        throw new UsageException("option '--output' may be supplied only once");
                    outputPath = ReadPath(args, ref index, argument);
                    break;
                case "--assemblies-only":
                    if (assembliesOnly)
                        throw new UsageException("option '--assemblies-only' may be supplied only once");
                    assembliesOnly = true;
                    break;
                case "--help":
                case "-h":
                    throw new UsageException($"option '{argument}' must be used alone");
                default:
                    throw new UsageException($"unknown argument '{argument}'");
            }
        }

        if (templatePath is null)
            throw new UsageException("missing required option '--template <path>'");

        if (assembliesOnly)
        {
            if (processId is not null || targetHostPath is not null || dacPath is not null)
            {
                throw new UsageException(
                    "option '--assemblies-only' cannot be combined with '--pid', '--target-host', or '--dac'");
            }

            if (assemblyDirectory is null)
                throw new UsageException("missing required option '--assembly-dir <path>'");
        }
        else
        {
            if (dacPath is null)
                throw new UsageException("missing required option '--dac <matching libmscordaccore.so>'");
            if (processId is null)
            {
                if (targetHostPath is null)
                    throw new UsageException(
                        "missing required option '--target-host <executable>' or '--pid <lazer-process-id>'");
                if (assemblyDirectory is null)
                    throw new UsageException("missing required option '--assembly-dir <path>'");
            }
            else
            {
                if (targetHostPath is not null)
                    throw new UsageException("option '--target-host' cannot be combined with '--pid'");
                if (assemblyDirectory is not null)
                    throw new UsageException("option '--assembly-dir' cannot be combined with '--pid'");
            }
        }

        return new CommandLine(
            targetHostPath,
            processId,
            dacPath,
            assemblyDirectory,
            templatePath,
            outputPath,
            assembliesOnly,
            ShowHelp: false);
    }

    private static string ReadPath(string[] args, ref int index, string option)
    {
        string value = ReadValue(args, ref index, option);
        if (string.IsNullOrWhiteSpace(value))
            throw new UsageException($"option '{option}' requires a non-empty path");
        return value;
    }

    private static int ReadProcessId(string[] args, ref int index, string option)
    {
        string value = ReadValue(args, ref index, option);
        if (!int.TryParse(
                value,
                NumberStyles.None,
                CultureInfo.InvariantCulture,
                out int processId) ||
            processId <= 0)
        {
            throw new UsageException($"option '{option}' requires a positive process ID");
        }

        return processId;
    }

    private static string ReadValue(string[] args, ref int index, string option)
    {
        if (index + 1 >= args.Length)
            throw new UsageException($"option '{option}' requires a value");

        string value = args[++index];
        if (value is "--target-host" or "--pid" or "--dac" or "--assembly-dir" or
            "--template" or "--output" or "--assemblies-only" or "--help" or "-h")
        {
            throw new UsageException($"option '{option}' requires a value");
        }

        return value;
    }
}

internal sealed class UsageException(string message) : Exception(message);

internal sealed class GeneratorException : Exception
{
    public GeneratorException(string message)
        : base(message)
    {
    }

    public GeneratorException(string message, Exception innerException)
        : base(message, innerException)
    {
    }
}
