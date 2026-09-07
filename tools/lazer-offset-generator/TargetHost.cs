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
    [GeneratedRegex(
        @"\Aready pid=(?<pid>[1-9][0-9]*) types=(?<types>[1-9][0-9]*) template_sha256=(?<sha256>[0-9a-f]{64})\z",
        RegexOptions.CultureInvariant)]
    private static partial Regex TargetReadyPattern();


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

}
