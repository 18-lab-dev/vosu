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
}
