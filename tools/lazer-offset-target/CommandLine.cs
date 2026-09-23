namespace LazerOffsetTarget;

internal sealed record CommandLine(
    string? TemplatePath,
    string? AssemblyDirectory,
    bool GameBaseSignature,
    bool ShowHelp)
{
    public static CommandLine Parse(string[] args)
    {
        if (args.Length == 1 && args[0] is "--help" or "-h")
            return new CommandLine(null, null, GameBaseSignature: false, ShowHelp: true);

        string? templatePath = null;
        string? assemblyDirectory = null;
        bool gameBaseSignature = false;

        for (int index = 0; index < args.Length; index++)
        {
            string argument = args[index];
            switch (argument)
            {
                case "--game-base-signature":
                    if (gameBaseSignature)
                        throw new UsageException(
                            "option '--game-base-signature' may be supplied only once");
                    gameBaseSignature = true;
                    break;
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

        if (assemblyDirectory is null)
        {
            throw new UsageException(
                "missing required option '--assembly-dir <Release output directory>'");
        }

        if (gameBaseSignature)
        {
            if (templatePath is not null)
                throw new UsageException(
                    "options '--game-base-signature' and '--template' are mutually exclusive");
        }
        else if (templatePath is null)
        {
            throw new UsageException("missing required option '--template <offsets.json>'");
        }

        return new CommandLine(
            templatePath,
            assemblyDirectory,
            gameBaseSignature,
            ShowHelp: false);
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
        value is "--game-base-signature" or "--template" or "--assembly-dir" or "--help" or "-h";
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
