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
    private const uint MemFdCloseOnExec = 0x0001;
    private const uint MemFdAllowSealing = 0x0002;
    private const int FileAddSeals = 1033;
    private const int FileSealAllWrites = 0x000f;
    private const string RuntimeIdentityEnvironmentVariable =
        "VOSU_LAZER_OFFSET_RUNTIME_IDENTITY";
    private const int LinuxStatBufferSize = 256;
    private const string ExpectedCoreLibSha256 =
        "73c6ae7ff2ed535a249539f78731fc4412cf6c263ed9692fcf035f2cd01f96fa";
    private const string ExpectedJitSha256 =
        "3d58119a829d10d20f9fc31f2144f9202af19c93b412d5a5eeeeeba4b35ebb3e";
    private const string ExpectedCoreClrSha256 =
        "dcc631eda8fdb34d06956697e9709b8d828306eb73ac3187b5058180e1af615e";
    private const string ExpectedDacVersionMarker = "@(#)Version 0.0";
    private const string ExpectedDacProvenanceMarker =
        "@Commit: 098d0ed17cf8f965d79be6e3d870a895a6dbbe04 " +
        "@Patch: e0aaf89fff93b05dad0a4fc272683cbc852a3f51fd8d5b27ac8d91bafd3a597a";

    [DllImport(
        "libc",
        EntryPoint = "memfd_create",
        SetLastError = true,
        CharSet = CharSet.Ansi)]
    private static extern int MemFdCreate(string name, uint flags);

    [DllImport("libc", EntryPoint = "fcntl", SetLastError = true)]
    private static extern int FileControl(int fileDescriptor, int command, int argument);

    [DllImport("libc", EntryPoint = "fstat", SetLastError = true)]
    private static extern int FStat(int fileDescriptor, IntPtr status);

    private const string Usage = """
        Usage: lazer-offset-generator --target-host <executable> --assembly-dir <path> \
            --dac <matching libmscordaccore.so> --template <path> [--output <path>]
        Usage: lazer-offset-generator --pid <lazer-process-id> \
            --dac <matching libmscordaccore.so> --template <path> [--output <path>]

        The first form launches the target host as a direct child. The second form
        attaches to the already-running lazer process. Both forms replace every
        offset in the ordered template. The first form requires the pinned Satori
        DAC markers; the live-process form requires a DAC matching the target CLR.
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
            using ValidatedDac dac = options.ProcessId is null
                ? OpenValidatedDac(options.DacPath!)
                : OpenLiveDac(options.DacPath!);

            byte[] json;
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

    [GeneratedRegex(
        @"\Aready pid=(?<pid>[1-9][0-9]*) types=(?<types>[1-9][0-9]*) template_sha256=(?<sha256>[0-9a-f]{64})\z",
        RegexOptions.CultureInvariant)]
    private static partial Regex TargetReadyPattern();

    private static ValidatedDac OpenValidatedDac(string path) =>
        OpenDac(path, validateMarkers: true);

    private static ValidatedDac OpenLiveDac(string path) =>
        OpenDac(path, validateMarkers: false);

    private static ValidatedDac OpenDac(string path, bool validateMarkers)
    {
        if (!OperatingSystem.IsLinux())
        {
            throw new GeneratorException(
                "DAC validation requires Linux because the validated file descriptor is " +
                "passed to ClrMD through /proc/self/fd");
        }

        FileStream? stream = null;
        try
        {
            using FileStream source = new(
                Path.GetFullPath(path),
                FileMode.Open,
                FileAccess.Read,
                FileShare.Read);
            stream = CreateSealedDacSnapshot(source);
            if (validateMarkers)
            {
                if (!ContainsAsciiMarker(stream, ExpectedDacVersionMarker))
                {
                    throw new GeneratorException(
                        $"DAC '{path}' does not carry expected version marker " +
                        $"'{ExpectedDacVersionMarker}'");
                }

                stream.Position = 0;
                if (!ContainsAsciiMarker(stream, ExpectedDacProvenanceMarker))
                {
                    throw new GeneratorException(
                        $"DAC '{path}' does not carry expected Satori provenance marker " +
                        $"'{ExpectedDacProvenanceMarker}'");
                }
            }

            long descriptor = stream.SafeFileHandle.DangerousGetHandle().ToInt64();
            if (descriptor < 0)
                throw new GeneratorException($"DAC '{path}' has an invalid file descriptor");

            return new ValidatedDac(stream, $"/proc/self/fd/{descriptor}");
        }
        catch (GeneratorException)
        {
            stream?.Dispose();
            throw;
        }
        catch (Exception error) when (error is IOException or
                                             UnauthorizedAccessException or
                                             ArgumentException or
                                             NotSupportedException or
                                             Win32Exception)
        {
            stream?.Dispose();
            throw new GeneratorException($"could not validate DAC '{path}': {error.Message}", error);
        }
    }

    private static FileStream CreateSealedDacSnapshot(FileStream source)
    {
        int descriptor = MemFdCreate(
            "vosu-satori-dac",
            MemFdCloseOnExec | MemFdAllowSealing);
        if (descriptor < 0)
            throw new Win32Exception(Marshal.GetLastPInvokeError());

        FileStream snapshot = new(
            new SafeFileHandle(new IntPtr(descriptor), ownsHandle: true),
            FileAccess.ReadWrite);
        try
        {
            source.CopyTo(snapshot);
            snapshot.Flush();
            if (FileControl(descriptor, FileAddSeals, FileSealAllWrites) < 0)
                throw new Win32Exception(Marshal.GetLastPInvokeError());

            snapshot.Position = 0;
            return snapshot;
        }
        catch
        {
            snapshot.Dispose();
            throw;
        }
    }

    private sealed class ValidatedDac(FileStream stream, string loaderPath) : IDisposable
    {
        public string LoaderPath { get; } = loaderPath;

        public void Dispose() => stream.Dispose();
    }

    private sealed class PinnedRuntimeFiles : IDisposable
    {
        private readonly PinnedRuntimeFile[] _files;

        private PinnedRuntimeFiles(PinnedRuntimeFile[] files)
        {
            _files = files;
            IdentityToken = string.Join(
                ';',
                files.Select(file => $"{file.Name}={file.InitialStamp.Serialize()}"));
        }

        public string IdentityToken { get; }

        public static PinnedRuntimeFiles Open(string targetHostPath)
        {
            if (!OperatingSystem.IsLinux() ||
                RuntimeInformation.ProcessArchitecture != Architecture.X64)
            {
                throw new GeneratorException(
                    "target runtime guarding requires Linux x64");
            }

            string? directory = Path.GetDirectoryName(Path.GetFullPath(targetHostPath));
            if (directory is null)
            {
                throw new GeneratorException(
                    $"could not determine target runtime directory for '{targetHostPath}'");
            }

            PinnedRuntimeFile[] files = new PinnedRuntimeFile[3];
            int opened = 0;
            try
            {
                files[opened++] = PinnedRuntimeFile.Open(
                    directory,
                    "System.Private.CoreLib.dll",
                    ExpectedCoreLibSha256);
                files[opened++] = PinnedRuntimeFile.Open(
                    directory,
                    "libclrjit.so",
                    ExpectedJitSha256);
                files[opened++] = PinnedRuntimeFile.Open(
                    directory,
                    "libcoreclr.so",
                    ExpectedCoreClrSha256);
                return new PinnedRuntimeFiles(files);
            }
            catch
            {
                for (int index = 0; index < opened; index++)
                    files[index].Dispose();
                throw;
            }
        }

        public void VerifyUnchanged()
        {
            foreach (PinnedRuntimeFile file in _files)
                file.VerifyUnchanged();
        }

        public void Dispose()
        {
            foreach (PinnedRuntimeFile file in _files)
                file.Dispose();
        }
    }

    private sealed class PinnedRuntimeFile(
        string name,
        string path,
        string expectedSha256,
        FileStream stream,
        RuntimeFileStamp initialStamp) : IDisposable
    {
        public string Name { get; } = name;

        public RuntimeFileStamp InitialStamp { get; } = initialStamp;

        public static PinnedRuntimeFile Open(
            string directory,
            string name,
            string expectedSha256)
        {
            string path = Path.Combine(directory, name);
            FileStream? stream = null;
            try
            {
                stream = new FileStream(
                    path,
                    FileMode.Open,
                    FileAccess.Read,
                    FileShare.Read);
                RuntimeFileStamp before = ReadRuntimeFileStamp(stream);
                string actualSha256 = HashStream(stream);
                RuntimeFileStamp after = ReadRuntimeFileStamp(stream);
                if (before != after)
                {
                    throw new GeneratorException(
                        $"target runtime file '{path}' changed while it was being validated");
                }

                if (!string.Equals(actualSha256, expectedSha256, StringComparison.Ordinal))
                {
                    throw new GeneratorException(
                        $"target runtime file '{path}' has SHA-256 '{actualSha256}', " +
                        $"expected '{expectedSha256}'");
                }

                return new PinnedRuntimeFile(name, path, expectedSha256, stream, after);
            }
            catch (GeneratorException)
            {
                stream?.Dispose();
                throw;
            }
            catch (Exception error) when (error is IOException or
                                                 UnauthorizedAccessException or
                                                 ArgumentException or
                                                 NotSupportedException or
                                                 Win32Exception or
                                                 OverflowException)
            {
                stream?.Dispose();
                throw new GeneratorException(
                    $"could not validate target runtime file '{path}': {error.Message}",
                    error);
            }
        }

        public void VerifyUnchanged()
        {
            RuntimeFileStamp before = ReadRuntimeFileStamp(stream);
            string actualSha256 = HashStream(stream);
            RuntimeFileStamp after = ReadRuntimeFileStamp(stream);
            if (before != InitialStamp || after != InitialStamp)
            {
                throw new GeneratorException(
                    $"target runtime file '{path}' changed while the target was running");
            }

            if (!string.Equals(actualSha256, expectedSha256, StringComparison.Ordinal))
            {
                throw new GeneratorException(
                    $"target runtime file '{path}' changed to SHA-256 '{actualSha256}' " +
                    $"while the target was running");
            }
        }

        public void Dispose() => stream.Dispose();
    }

    private static string HashStream(FileStream stream)
    {
        stream.Position = 0;
        return Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
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

    private readonly record struct RuntimeFileStamp(
        ulong DeviceMajor,
        ulong DeviceMinor,
        ulong Inode,
        long Size,
        long ChangeSeconds,
        long ChangeNanoseconds)
    {
        public string Serialize() => string.Create(
            CultureInfo.InvariantCulture,
            $"{DeviceMajor}:{DeviceMinor}:{Inode}:{Size}:{ChangeSeconds}:{ChangeNanoseconds}");
    }

    private static bool ContainsAsciiMarker(FileStream stream, string marker)
    {
        byte[] needle = Encoding.ASCII.GetBytes(marker);
        byte[] buffer = new byte[(64 * 1024) + needle.Length - 1];
        int carry = 0;

        while (true)
        {
            int read = stream.Read(buffer, carry, buffer.Length - carry);
            int length = carry + read;
            if (buffer.AsSpan(0, length).IndexOf(needle) >= 0)
                return true;
            if (read == 0)
                return false;

            carry = Math.Min(needle.Length - 1, length);
            buffer.AsSpan(length - carry, carry).CopyTo(buffer);
        }
    }

    private static async Task<string> ReadReadyLineAsync(TextReader reader)
    {
        char[] line = new char[MaximumReadyLineLength];
        char[] buffer = new char[64];
        int length = 0;

        while (true)
        {
            int read = await reader.ReadAsync(buffer.AsMemory()).ConfigureAwait(false);
            if (read == 0)
                throw new GeneratorException("target host closed stdout before completing its ready line");

            for (int index = 0; index < read; index++)
            {
                char character = buffer[index];
                if (character == '\n')
                {
                    int contentLength = length;
                    if (contentLength > 0 && line[contentLength - 1] == '\r')
                        contentLength--;
                    return new string(line, 0, contentLength);
                }

                if (length == line.Length)
                {
                    throw new GeneratorException(
                        $"target host ready line exceeds {MaximumReadyLineLength} characters");
                }

                line[length++] = character;
            }
        }
    }

    private static async Task DrainStandardErrorAsync(
        TextReader reader,
        BoundedText diagnostic)
    {
        char[] buffer = new char[512];
        try
        {
            while (true)
            {
                int read = await reader.ReadAsync(buffer.AsMemory()).ConfigureAwait(false);
                if (read == 0)
                    return;
                diagnostic.Append(buffer.AsSpan(0, read));
            }
        }
        catch (Exception error)
        {
            diagnostic.Append($"\n[stderr read failed: {error.Message}]");
        }
    }

    private sealed class OwnedTargetHost
    {
        private readonly Process _process;
        private readonly int _processId;
        private readonly BoundedText _stderr = new(MaximumTargetStderrLength);
        private readonly Task _stderrDrain;
        private bool _cleanedUp;

        private OwnedTargetHost(Process process)
        {
            _process = process;
            _processId = process.Id;
            _stderrDrain = DrainStandardErrorAsync(process.StandardError, _stderr);
        }

        public int ProcessId => _processId;

        public static OwnedTargetHost Start(
            string executablePath,
            string templatePath,
            string assemblyDirectory,
            string runtimeIdentityToken,
            int expectedTypeCount,
            string expectedTemplateSha256)
        {
            ProcessStartInfo startInfo = new()
            {
                FileName = executablePath,
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                CreateNoWindow = true,
            };
            startInfo.ArgumentList.Add("--template");
            startInfo.ArgumentList.Add(templatePath);
            startInfo.ArgumentList.Add("--assembly-dir");
            startInfo.ArgumentList.Add(assemblyDirectory);
            RemoveRuntimeOverrides(startInfo);
            startInfo.Environment[RuntimeIdentityEnvironmentVariable] = runtimeIdentityToken;

            Process process = new()
            {
                StartInfo = startInfo,
            };

            try
            {
                if (!process.Start())
                {
                    throw new GeneratorException(
                        $"could not launch target host executable '{executablePath}'");
                }
            }
            catch (Exception error)
            {
                process.Dispose();
                if (error is GeneratorException)
                    throw;

                throw new GeneratorException(
                    $"could not launch target host executable '{executablePath}': {error.Message}",
                    error);
            }

            static void RemoveRuntimeOverrides(ProcessStartInfo startInfo)
            {
                foreach (string key in startInfo.Environment.Keys.ToArray())
                {
                    if (key.StartsWith("DOTNET_", StringComparison.OrdinalIgnoreCase) ||
                        key.StartsWith("CORECLR_", StringComparison.OrdinalIgnoreCase) ||
                        key.StartsWith("COMPlus_", StringComparison.OrdinalIgnoreCase) ||
                        key.StartsWith("LD_", StringComparison.OrdinalIgnoreCase))
                    {
                        startInfo.Environment.Remove(key);
                    }
                }
            }

            OwnedTargetHost targetHost = new(process);
            try
            {
                targetHost.WaitUntilReady(expectedTypeCount, expectedTemplateSha256);
                return targetHost;
            }
            catch (Exception startupError)
            {
                try
                {
                    targetHost.TerminateAndReap();
                }
                catch (Exception cleanupError)
                {
                    throw new GeneratorException(
                        $"{startupError.Message}; target host {targetHost.ProcessId} cleanup also failed: " +
                        cleanupError.Message,
                        startupError);
                }

                if (startupError is GeneratorException)
                    throw;

                throw new GeneratorException(
                    $"target host {targetHost.ProcessId} failed during startup: {startupError.Message}",
                    startupError);
            }
        }

        public string? TryDescribeExit()
        {
            if (!_process.HasExited)
                return null;

            _process.WaitForExit();
            FinishStderrDrain();
            return DescribeExit("during ClrMD inspection");
        }

        public void TerminateAndReap()
        {
            if (_cleanedUp)
                return;
            _cleanedUp = true;

            Exception? cleanupError = null;
            try
            {
                if (!_process.HasExited)
                    _process.Kill();
            }
            catch (InvalidOperationException)
            {
                // The process exited between HasExited and Kill. WaitForExit below still reaps it.
            }
            catch (Exception error)
            {
                cleanupError = error;
            }

            try
            {
                _process.WaitForExit();
            }
            catch (Exception error)
            {
                cleanupError ??= error;
            }

            try
            {
                FinishStderrDrain();
            }
            catch (Exception error)
            {
                cleanupError ??= error;
            }

            try
            {
                _process.StandardOutput.Dispose();
            }
            catch (Exception error)
            {
                cleanupError ??= error;
            }

            try
            {
                _process.StandardError.Dispose();
            }
            catch (Exception error)
            {
                cleanupError ??= error;
            }

            try
            {
                _process.Dispose();
            }
            catch (Exception error)
            {
                cleanupError ??= error;
            }

            if (cleanupError is not null)
            {
                throw new GeneratorException(
                    $"could not terminate and reap target host {_processId}: {cleanupError.Message}",
                    cleanupError);
            }
        }

        private void WaitUntilReady(int expectedTypeCount, string expectedTemplateSha256)
        {
            Task<string> readyLineTask = ReadReadyLineAsync(_process.StandardOutput);
            Task exitTask = _process.WaitForExitAsync();
            Task completedTask;
            try
            {
                completedTask = Task.WhenAny(readyLineTask, exitTask)
                    .WaitAsync(TargetStartupTimeout)
                    .GetAwaiter()
                    .GetResult();
            }
            catch (TimeoutException error)
            {
                ObserveFault(readyLineTask);
                throw new GeneratorException(
                    $"target host {_processId} did not report readiness within " +
                    $"{TargetStartupTimeout.TotalSeconds:0} seconds; {DescribeStderr()}",
                    error);
            }

            if (completedTask == exitTask)
            {
                ObserveFault(readyLineTask);
                _process.WaitForExit();
                FinishStderrDrain();
                throw new GeneratorException(DescribeExit("before reporting readiness"));
            }

            string readyLine;
            try
            {
                readyLine = readyLineTask.GetAwaiter().GetResult();
            }
            catch (Exception error)
            {
                string suffix;
                if (_process.HasExited)
                {
                    _process.WaitForExit();
                    FinishStderrDrain();
                    suffix = DescribeExit("during startup");
                }
                else
                {
                    suffix = DescribeStderr();
                }

                string message = error is GeneratorException
                    ? error.Message
                    : $"could not read target host ready line: {error.Message}";
                throw new GeneratorException($"{message}; {suffix}", error);
            }

            Match match = TargetReadyPattern().Match(readyLine);
            if (!match.Success ||
                !int.TryParse(
                    match.Groups["pid"].Value,
                    NumberStyles.None,
                    CultureInfo.InvariantCulture,
                    out int reportedPid) ||
                !int.TryParse(
                    match.Groups["types"].Value,
                    NumberStyles.None,
                    CultureInfo.InvariantCulture,
                    out int reportedTypeCount))
            {
                throw new GeneratorException(
                    $"target host {_processId} produced invalid ready line " +
                    $"{JsonSerializer.Serialize(readyLine)}; expected " +
                    "'ready pid=<positive decimal> types=<positive decimal> " +
                    "template_sha256=<lowercase SHA-256>'; " +
                    DescribeStderr());
            }

            string reportedTemplateSha256 = match.Groups["sha256"].Value;
            if (reportedPid != _processId)
            {
                throw new GeneratorException(
                    $"target host reported PID {reportedPid}, but its direct child PID is " +
                    $"{_processId}; refusing to attach; {DescribeStderr()}");
            }

            if (reportedTypeCount != expectedTypeCount ||
                !string.Equals(
                    reportedTemplateSha256,
                    expectedTemplateSha256,
                    StringComparison.Ordinal))
            {
                throw new GeneratorException(
                    $"target host {_processId} activated {reportedTypeCount} types from template " +
                    $"SHA-256 {reportedTemplateSha256}, but the generator loaded " +
                    $"{expectedTypeCount} types from SHA-256 {expectedTemplateSha256}; " +
                    $"refusing to attach; {DescribeStderr()}");
            }

            if (_process.HasExited)
            {
                _process.WaitForExit();
                FinishStderrDrain();
                throw new GeneratorException(DescribeExit("immediately after reporting readiness"));
            }
        }

        private void FinishStderrDrain()
        {
            try
            {
                _stderrDrain
                    .WaitAsync(TargetDiagnosticDrainTimeout)
                    .GetAwaiter()
                    .GetResult();
            }
            catch (TimeoutException)
            {
                _stderr.Append(
                    $"\n[stderr drain did not finish within " +
                    $"{TargetDiagnosticDrainTimeout.TotalSeconds:0} seconds after target exit]");
                ObserveFault(_stderrDrain);
            }
        }

        private string DescribeExit(string timing) =>
            $"target host {_processId} exited with code {_process.ExitCode} {timing}; " +
            DescribeStderr();

        private string DescribeStderr()
        {
            string contents = _stderr.Snapshot(out bool truncated);
            if (contents.Length == 0)
                return "stderr was empty";

            return truncated
                ? $"stderr (first {MaximumTargetStderrLength} characters; truncated): {contents}"
                : $"stderr: {contents}";
        }

        private static void ObserveFault(Task task)
        {
            _ = task.ContinueWith(
                static completed => _ = completed.Exception,
                CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default);
        }
    }

    private sealed class BoundedText(int capacity)
    {
        private readonly object _gate = new();
        private readonly char[] _buffer = new char[capacity];
        private int _length;
        private bool _truncated;

        public void Append(ReadOnlySpan<char> value)
        {
            lock (_gate)
            {
                int count = Math.Min(value.Length, _buffer.Length - _length);
                value[..count].CopyTo(_buffer.AsSpan(_length));
                _length += count;
                _truncated |= count != value.Length;
            }
        }

        public string Snapshot(out bool truncated)
        {
            lock (_gate)
            {
                truncated = _truncated;
                return new string(_buffer, 0, _length).Trim();
            }
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
                            ResolveObjectFieldOffset(type, entry.Name, fieldName, target.DataReader.PointerSize));
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

internal sealed record CommandLine(
    string? TargetHostPath,
    int? ProcessId,
    string? DacPath,
    string? AssemblyDirectory,
    string? TemplatePath,
    string? OutputPath,
    bool ShowHelp)
{
    public static CommandLine Parse(string[] args)
    {
        if (args.Length == 1 && args[0] is "--help" or "-h")
            return new CommandLine(null, null, null, null, null, null, ShowHelp: true);

        string? targetHostPath = null;
        int? processId = null;
        string? dacPath = null;
        string? assemblyDirectory = null;
        string? templatePath = null;
        string? outputPath = null;

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
                case "--help":
                case "-h":
                    throw new UsageException($"option '{argument}' must be used alone");
                default:
                    throw new UsageException($"unknown argument '{argument}'");
            }
        }

        if (dacPath is null)
            throw new UsageException("missing required option '--dac <matching libmscordaccore.so>'");
        if (templatePath is null)
            throw new UsageException("missing required option '--template <path>'");
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

        return new CommandLine(
            targetHostPath,
            processId,
            dacPath,
            assemblyDirectory,
            templatePath,
            outputPath,
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
            "--template" or "--output" or "--help" or "-h")
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
