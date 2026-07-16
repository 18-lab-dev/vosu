from __future__ import annotations

import bisect
import base64
import argparse
import hashlib
import json
import os
import math
import queue
from pathlib import Path
import shutil
import secrets
import socket
import signal
import struct
import subprocess
import sys
import tempfile
import time
import threading
import http.client
import urllib.error
import urllib.request
import tomllib
from typing import Any
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))
from live_parity import (SCHEMAS, PINNED_VOSU_VERSION, PINNED_SOURCE_REVISION, VOSU_BUILD_LABEL,
    SOURCE_BUILD_LABEL, SUPPORTED_MATRIX, Sample, align_active, barrier_of,
    beatmap_time, compare_pair, dump_capture, load_capture, monotonic_violations,
    paused_of, state_of)
from parity_domains import (
    cadence_evidence, confirmed_retry_boundaries, gamebase_path_family,
    infer_reference_capabilities, segment_at,
)
CONNECTION_OWNERSHIP_TIMEOUT_SECONDS = 2.0
LINUX_INODE_OWNER_CACHE_NS = 5_000_000
MIN_ACTIVE_BEFORE_PAUSE_NS = 1_000_000_000
_linux_inode_owner_cache_lock = threading.Lock()
_linux_inode_owner_cache_ns = 0
_linux_inode_owner_cache: dict[str, set[int]] = {}
INJECTION_ENV_PREFIXES = (
    "NODE_", "LD_", "DYLD_", "PYTHON", "RUST", "CARGO", "BUN_", "DENO_",
    "NPM_", "PNPM_", "CCACHE_", "SCCACHE_", "DISTCC_", "ICECC_", "GIT_",
    "CMAKE_", "MESON_", "DOTNET_", "CORECLR_", "COMPLUS_", "COR_", "MONO_",
)
INJECTION_ENV_NAMES = {
    "SSLKEYLOGFILE", "CC", "CXX", "CPP", "LD", "AR", "AS", "NM", "OBJCOPY",
    "OBJDUMP", "RANLIB", "STRIP", "LINKER", "CFLAGS", "CPPFLAGS", "CXXFLAGS",
    "LDFLAGS", "MAKEFLAGS", "BASH_ENV", "ENV", "ZDOTDIR", "PERL5OPT",
    "PERL5LIB", "RUBYOPT", "RUBYLIB", "LUA_PATH", "LUA_CPATH", "CLASSPATH",
    "JAVA_TOOL_OPTIONS", "JDK_JAVA_OPTIONS", "_JAVA_OPTIONS",
    "DOTNET_STARTUP_HOOKS", "DOTNET_ADDITIONAL_DEPS", "ELECTRON_RUN_AS_NODE",
}


def _is_injection_environment(name: str) -> bool:
    upper = name.upper()
    return upper in INJECTION_ENV_NAMES or upper.startswith(INJECTION_ENV_PREFIXES)


def _valid_environment_keys(
    value: Any, allowed_injection: set[str] | None = None
) -> bool:
    allowed = allowed_injection or set()
    return (
        isinstance(value, list)
        and all(isinstance(key, str) for key in value)
        and value == sorted(set(value))
        and all(
            not _is_injection_environment(key) or key in allowed
            for key in value)
    )


def _process_environment_keys(pid: int) -> list[str]:
    if not sys.platform.startswith("linux"):
        raise ValueError("external client environment attestation requires Linux")
    entries = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
    keys: set[str] = set()
    for entry in entries:
        if not entry:
            continue
        name, separator, _value = entry.partition(b"=")
        if not separator or not name:
            raise ValueError("external client has malformed process environment")
        keys.add(name.decode("utf-8", errors="surrogateescape"))
    return sorted(keys)


def _scrubbed_environment(
        extra: dict[str, Any] | None = None, *, trusted_internal: bool = False
) -> dict[str, str]:
    keep = {
        "PATH", "HOME", "USER", "LOGNAME", "LANG", "USERPROFILE",
        "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "APPDATA", "LOCALAPPDATA",
        "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR",
        "XDG_SESSION_TYPE", "XDG_DATA_DIRS", "XDG_CURRENT_DESKTOP",
        "XDG_BACKEND", "XDG_SEAT", "XDG_SESSION_ID", "XDG_VTNR",
        "WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS",
    }
    environment = {
        key: value for key, value in os.environ.items()
        if (key.upper() in keep or key.upper().startswith("LC_"))
        and not _is_injection_environment(key)
    }
    trusted = {"CARGO_TARGET_DIR", "CARGO_HOME", "RUSTC", "PYTHON", "HOME",
               "USERPROFILE", "XDG_CONFIG_HOME"}
    for key, value in (extra or {}).items():
        if _is_injection_environment(key) and not (
                trusted_internal and key.upper() in trusted):
            raise ValueError(f"code-loading environment override is forbidden: {key}")
        environment[str(key)] = str(value)
    return environment
def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def file_contains(path: Path, needle: bytes) -> bool:
    overlap = b""
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            data = overlap + chunk
            if needle in data:
                return True
            overlap = data[-max(0, len(needle) - 1):]
    return False


def _resolved_command(value: str) -> Path:
    found = shutil.which(value)
    path = Path(found if found else value).absolute()
    if not path.exists():
        raise FileNotFoundError(path)
    return path


def _windows_path_has_any_access(path: Path, rights: tuple[int, ...]) -> bool:
    """Check effective token rights without relying on os.access' mode-bit shim."""
    import ctypes
    from ctypes import wintypes

    create_file = ctypes.windll.kernel32.CreateFileW
    create_file.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    create_file.restype = wintypes.HANDLE

    share_all = 0x00000001 | 0x00000002 | 0x00000004
    open_existing = 3
    backup_semantics = 0x02000000
    invalid_handle = ctypes.c_void_p(-1).value

    for access in rights:
        handle = create_file(
            str(path),
            access,
            share_all,
            None,
            open_existing,
            backup_semantics,
            None,
        )
        if handle != invalid_handle:
            ctypes.windll.kernel32.CloseHandle(handle)
            return True

    return False


def _windows_path_is_writable(path: Path) -> bool:
    file_rights = (0x00000002, 0x00000004, 0x00010000, 0x00040000, 0x00080000)
    directory_rights = (
        0x00000002,
        0x00000004,
        0x00000040,
        0x00010000,
        0x00040000,
        0x00080000,
    )
    return _windows_path_has_any_access(
        path, directory_rights if path.is_dir() else file_rights)


def _windows_command_is_writable(path: Path, resolved: Path) -> bool:
    replace_child_rights = (0x00000040, 0x00010000, 0x00040000, 0x00080000)
    return (
        _windows_path_is_writable(path)
        or _windows_path_is_writable(resolved)
        or any(
            _windows_path_has_any_access(parent, replace_child_rights)
            for candidate in (path, resolved)
            for parent in candidate.parents
        )
    )


def _resolved_trusted_command(value: str) -> Path:
    path = _resolved_command(value)
    resolved = path.resolve(strict=True)
    locations = {
        ancestor
        for candidate in (path, resolved)
        for ancestor in (candidate, *candidate.parents)
    }
    if os.name == "posix":
        for location in locations:
            stat = location.stat()
            if stat.st_uid != 0 or stat.st_mode & 0o022:
                raise ValueError(
                    f"build tool is not rooted in a protected system path: {path}")
    elif _windows_command_is_writable(path, resolved):
        raise ValueError(
            f"build tool is writable by the capture user: {path}")
    return path


def _process_executable(pid: int) -> Path:
    if pid <= 0:
        raise ValueError("process PID must be positive")
    if sys.platform.startswith("linux"):
        return Path(f"/proc/{pid}/exe").resolve(strict=True)
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes
        process = ctypes.windll.kernel32.OpenProcess(0x1000, False, pid)
        if not process:
            raise ValueError(f"cannot open process {pid}")
        try:
            size = wintypes.DWORD(32768)
            buffer = ctypes.create_unicode_buffer(size.value)
            if not ctypes.windll.kernel32.QueryFullProcessImageNameW(process, 0, buffer, ctypes.byref(size)):
                raise ValueError(f"cannot resolve process {pid} executable")
            return Path(buffer.value).resolve(strict=True)
        finally:
            ctypes.windll.kernel32.CloseHandle(process)
    raise ValueError("live parity supports process identity only on Windows and Linux")


def _process_start_identity(pid: int) -> str:
    if sys.platform.startswith("linux"):
        # The field after the final ')' begins at proc stat field 3; starttime is field 22.
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
        return stat[stat.rfind(")") + 2:].split()[19]
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes
        process = ctypes.windll.kernel32.OpenProcess(0x1000, False, pid)
        if not process:
            raise ValueError(f"cannot open process {pid}")
        try:
            created = wintypes.FILETIME()
            exited, kernel, user = wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME()
            if not ctypes.windll.kernel32.GetProcessTimes(
                    process, ctypes.byref(created), ctypes.byref(exited),
                    ctypes.byref(kernel), ctypes.byref(user)):
                raise ValueError(f"cannot read process {pid} start identity")
            return str((created.dwHighDateTime << 32) | created.dwLowDateTime)
        finally:
            ctypes.windll.kernel32.CloseHandle(process)
    raise ValueError("unsupported host")


def _process_parent_pid(pid: int) -> int:
    if pid <= 0:
        raise ValueError("process PID must be positive")
    if sys.platform.startswith("linux"):
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
        return int(stat[stat.rfind(")") + 2:].split()[1])
    if sys.platform == "win32":
        completed = subprocess.run(
            [str(_resolved_trusted_command("powershell.exe")),
             "-NoProfile", "-NonInteractive", "-Command",
             f"(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').ParentProcessId"],
            check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=_scrubbed_environment())
        value = completed.stdout.strip()
        if not value.isdigit():
            raise ValueError(f"cannot resolve process {pid} parent")
        return int(value)
    raise ValueError("unsupported host")


def _process_alive(pid: int, start_identity: str) -> bool:
    try:
        if sys.platform.startswith("linux"):
            stat = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
            fields = stat[stat.rfind(")") + 2:].split()
            return fields[0] != "Z" and fields[19] == start_identity
        if sys.platform == "win32":
            import ctypes
            from ctypes import wintypes
            process = ctypes.windll.kernel32.OpenProcess(0x1000, False, pid)
            if not process:
                return False
            try:
                exit_code = wintypes.DWORD()
                if (not ctypes.windll.kernel32.GetExitCodeProcess(
                        process, ctypes.byref(exit_code))
                        or exit_code.value != 259):
                    return False
                created = wintypes.FILETIME()
                exited = wintypes.FILETIME()
                kernel = wintypes.FILETIME()
                user = wintypes.FILETIME()
                if not ctypes.windll.kernel32.GetProcessTimes(
                        process, ctypes.byref(created), ctypes.byref(exited),
                        ctypes.byref(kernel), ctypes.byref(user)):
                    return False
                identity = str(
                    (created.dwHighDateTime << 32) | created.dwLowDateTime)
                return identity == start_identity
            finally:
                ctypes.windll.kernel32.CloseHandle(process)
        return _process_start_identity(pid) == start_identity
    except (OSError, ValueError, IndexError):
        return False


def _process_cmdline(pid: int) -> list[str]:
    if sys.platform.startswith("linux"):
        return [part.decode(errors="surrogateescape")
                for part in Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0") if part]
    if sys.platform == "win32":
        completed = subprocess.run(
            [str(_resolved_trusted_command("powershell.exe")),
             "-NoProfile", "-NonInteractive", "-Command",
             f"(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"],
            check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=_scrubbed_environment())
        raw = completed.stdout.strip()
        import ctypes
        argc = ctypes.c_int()
        parser = ctypes.windll.shell32.CommandLineToArgvW
        parser.restype = ctypes.POINTER(ctypes.c_wchar_p)
        argv = parser(raw, ctypes.byref(argc))
        if not argv:
            raise ValueError(f"cannot parse process {pid} command line")
        try:
            return [argv[index] for index in range(argc.value)]
        finally:
            ctypes.windll.kernel32.LocalFree(argv)
    raise ValueError("unsupported host")

def _linux_inode_owners(inodes: set[str]) -> set[int]:
    if not inodes:
        return set()

    global _linux_inode_owner_cache_ns, _linux_inode_owner_cache
    now = time.monotonic_ns()
    with _linux_inode_owner_cache_lock:
        if now - _linux_inode_owner_cache_ns > LINUX_INODE_OWNER_CACHE_NS:
            snapshot: dict[str, set[int]] = {}
            for proc in Path("/proc").iterdir():
                if not proc.name.isdigit():
                    continue
                try:
                    for fd in (proc / "fd").iterdir():
                        try:
                            target = os.readlink(fd)
                        except OSError:
                            continue
                        if target.startswith("socket:["):
                            snapshot.setdefault(target[8:-1], set()).add(int(proc.name))
                except OSError:
                    continue
            _linux_inode_owner_cache = snapshot
            _linux_inode_owner_cache_ns = time.monotonic_ns()

        owners: set[int] = set()
        for inode in inodes:
            owners.update(_linux_inode_owner_cache.get(inode, ()))
        return owners

def _linux_pid_socket_inodes(pid: int) -> set[str]:
    inodes: set[str] = set()
    try:
        descriptors = Path(f"/proc/{pid}/fd").iterdir()
        for descriptor in descriptors:
            try:
                target = os.readlink(descriptor)
            except OSError:
                continue
            if target.startswith("socket:["):
                inodes.add(target[8:-1])
    except OSError:
        return set()
    return inodes


def _linux_listening_socket_inodes(port: int) -> set[str]:
    inodes: set[str] = set()
    try:
        lines = Path("/proc/net/tcp").read_text(encoding="ascii").splitlines()[1:]
    except OSError:
        return inodes
    for line in lines:
        fields = line.split()
        if len(fields) <= 9:
            continue
        local_address, local_port = fields[1].rsplit(":", 1)
        if (
                local_address == "0100007F"
                and int(local_port, 16) == port
                and fields[3] == "0A"):
            inodes.add(fields[9])
    return inodes


def _linux_pid_owns_listening_socket(pid: int, port: int) -> bool:
    listeners = _linux_listening_socket_inodes(port)
    return bool(listeners) and listeners <= _linux_pid_socket_inodes(pid)



def _linux_socket_owners(port: int) -> set[int]:
    return _linux_inode_owners(_linux_listening_socket_inodes(port))


def _windows_socket_owners(port: int) -> set[int]:
    import ctypes
    from ctypes import wintypes
    size = wintypes.ULONG(0)
    api = ctypes.windll.iphlpapi.GetExtendedTcpTable
    api(None, ctypes.byref(size), False, socket.AF_INET, 3, 0)
    buffer = ctypes.create_string_buffer(size.value)
    if api(buffer, ctypes.byref(size), False, socket.AF_INET, 3, 0):
        raise OSError("GetExtendedTcpTable failed")
    count = struct.unpack_from("I", buffer.raw, 0)[0]
    expected_addr = struct.unpack("<I", socket.inet_aton("127.0.0.1"))[0]
    owners: set[int] = set()
    for index in range(count):
        _state, local_addr, local_port, _remote_addr, _remote_port, pid = (
            struct.unpack_from("IIIIII", buffer.raw, 4 + index * 24))
        if local_addr == expected_addr and socket.ntohs(local_port & 0xffff) == port:
            owners.add(pid)
    return owners


def _socket_owners(port: int) -> set[int]:
    if sys.platform.startswith("linux"):
        return _linux_socket_owners(port)
    if sys.platform == "win32":
        return _windows_socket_owners(port)
    return set()


def _socket_owner(port: int) -> int | None:
    owners = _socket_owners(port)
    return next(iter(owners)) if len(owners) == 1 else None


def _linux_connection_owners(server_port: int, client_port: int) -> set[int]:
    inodes: set[str] = set()
    try:
        lines = Path("/proc/net/tcp").read_text(encoding="ascii").splitlines()[1:]
    except OSError:
        lines = []
    for line in lines:
        fields = line.split()
        if len(fields) <= 9:
            continue
        local_address, local_port = fields[1].rsplit(":", 1)
        remote_address, remote_port = fields[2].rsplit(":", 1)
        if (
            local_address == "0100007F"
            and remote_address == "0100007F"
            and int(local_port, 16) == server_port
            and int(remote_port, 16) == client_port
            and fields[3] == "01"
        ):
            inodes.add(fields[9])
    return _linux_inode_owners(inodes)


def _windows_connection_owners(
    server_port: int, client_port: int
) -> set[int]:
    import ctypes
    from ctypes import wintypes
    size = wintypes.ULONG(0)
    api = ctypes.windll.iphlpapi.GetExtendedTcpTable
    api(None, ctypes.byref(size), False, socket.AF_INET, 5, 0)
    buffer = ctypes.create_string_buffer(size.value)
    if api(buffer, ctypes.byref(size), False, socket.AF_INET, 5, 0):
        raise OSError("GetExtendedTcpTable failed")
    count = struct.unpack_from("I", buffer.raw, 0)[0]
    loopback = struct.unpack("<I", socket.inet_aton("127.0.0.1"))[0]
    owners: set[int] = set()
    for index in range(count):
        state, local_addr, local_port, remote_addr, remote_port, pid = (
            struct.unpack_from("IIIIII", buffer.raw, 4 + index * 24))
        if (
            state == 5
            and local_addr == loopback
            and remote_addr == loopback
            and socket.ntohs(local_port & 0xffff) == server_port
            and socket.ntohs(remote_port & 0xffff) == client_port
        ):
            owners.add(pid)
    return owners


def _connection_owners(connection: socket.socket) -> set[int]:
    client_host, client_port = connection.getsockname()[:2]
    server_host, server_port = connection.getpeername()[:2]
    if client_host != "127.0.0.1" or server_host != "127.0.0.1":
        return set()
    if sys.platform.startswith("linux"):
        return _linux_connection_owners(server_port, client_port)
    if sys.platform == "win32":
        return _windows_connection_owners(server_port, client_port)
    return set()


def _connection_owner(connection: socket.socket) -> int | None:
    owners = _connection_owners(connection)
    return next(iter(owners)) if len(owners) == 1 else None


def _port_is_unused(port: int) -> bool:
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
        probe.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        probe.close()
