from __future__ import annotations

import ctypes
from ctypes import wintypes
import subprocess
import sys
import time
from typing import Any

_CREATE_SUSPENDED = 0x00000004
_SYSTEM_PROCESS_INFORMATION = 5
_STATUS_INFO_LENGTH_MISMATCH = ctypes.c_long(0xC0000004).value
_JOB_OBJECT_ASSOCIATE_COMPLETION_PORT_INFORMATION = 7
_JOB_OBJECT_EXTENDED_LIMIT_INFORMATION = 9
_JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000
_JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO = 4
_JOB_OBJECT_MSG_NEW_PROCESS = 6
_JOB_OBJECT_MSG_EXIT_PROCESS = 7
_JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS = 8
_TH32CS_SNAPTHREAD = 0x00000004
_THREAD_SUSPEND_RESUME = 0x0002
_INVALID_DWORD = 0xFFFFFFFF
_WAIT_TIMEOUT = 258


class _IoCounters(ctypes.Structure):
    _fields_ = [
        ("read_operation_count", ctypes.c_uint64),
        ("write_operation_count", ctypes.c_uint64),
        ("other_operation_count", ctypes.c_uint64),
        ("read_transfer_count", ctypes.c_uint64),
        ("write_transfer_count", ctypes.c_uint64),
        ("other_transfer_count", ctypes.c_uint64),
    ]


class _BasicLimitInformation(ctypes.Structure):
    _fields_ = [
        ("per_process_user_time_limit", ctypes.c_int64),
        ("per_job_user_time_limit", ctypes.c_int64),
        ("limit_flags", wintypes.DWORD),
        ("minimum_working_set_size", ctypes.c_size_t),
        ("maximum_working_set_size", ctypes.c_size_t),
        ("active_process_limit", wintypes.DWORD),
        ("affinity", ctypes.c_size_t),
        ("priority_class", wintypes.DWORD),
        ("scheduling_class", wintypes.DWORD),
    ]


class _ExtendedLimitInformation(ctypes.Structure):
    _fields_ = [
        ("basic_limit_information", _BasicLimitInformation),
        ("io_info", _IoCounters),
        ("process_memory_limit", ctypes.c_size_t),
        ("job_memory_limit", ctypes.c_size_t),
        ("peak_process_memory_used", ctypes.c_size_t),
        ("peak_job_memory_used", ctypes.c_size_t),
    ]


class _UnicodeString(ctypes.Structure):
    _fields_ = [
        ("length", wintypes.USHORT),
        ("maximum_length", wintypes.USHORT),
        ("buffer", wintypes.LPWSTR),
    ]


class _SystemProcessInformation(ctypes.Structure):
    _fields_ = [
        ("next_entry_offset", wintypes.ULONG),
        ("number_of_threads", wintypes.ULONG),
        ("working_set_private_size", ctypes.c_int64),
        ("hard_fault_count", wintypes.ULONG),
        ("number_of_threads_high_watermark", wintypes.ULONG),
        ("cycle_time", ctypes.c_uint64),
        ("create_time", ctypes.c_int64),
        ("user_time", ctypes.c_int64),
        ("kernel_time", ctypes.c_int64),
        ("image_name", _UnicodeString),
        ("base_priority", wintypes.LONG),
        ("unique_process_id", wintypes.HANDLE),
        ("parent_process_id", wintypes.HANDLE),
    ]


class _ThreadEntry(ctypes.Structure):
    _fields_ = [
        ("dwSize", wintypes.DWORD),
        ("cntUsage", wintypes.DWORD),
        ("th32ThreadID", wintypes.DWORD),
        ("th32OwnerProcessID", wintypes.DWORD),
        ("tpBasePri", wintypes.LONG),
        ("tpDeltaPri", wintypes.LONG),
        ("dwFlags", wintypes.DWORD),
    ]


class _CompletionPortAssociation(ctypes.Structure):
    _fields_ = [
        ("completion_key", ctypes.c_void_p),
        ("completion_port", wintypes.HANDLE),
    ]


def windows_osu_process_snapshot() -> dict[int, dict[str, Any]]:
    """Atomically preserve PID, parent, creation time, and image name."""
    if sys.platform != "win32":
        raise ValueError("Windows process snapshots require Windows")
    ntdll = ctypes.WinDLL("ntdll")
    query = ntdll.NtQuerySystemInformation
    query.argtypes = [
        wintypes.ULONG, ctypes.c_void_p, wintypes.ULONG,
        ctypes.POINTER(wintypes.ULONG),
    ]
    query.restype = ctypes.c_long
    size = 1 << 20
    while True:
        buffer = ctypes.create_string_buffer(size)
        required = wintypes.ULONG()
        status = query(
            _SYSTEM_PROCESS_INFORMATION,
            buffer,
            size,
            ctypes.byref(required),
        )
        if status == 0:
            break
        if status != _STATUS_INFO_LENGTH_MISMATCH:
            raise OSError(status, "cannot snapshot Windows processes")
        size = max(size * 2, int(required.value) + 65536)
        if size > 64 << 20:
            raise ValueError("Windows process snapshot exceeded size bound")

    processes: dict[int, dict[str, Any]] = {}
    offset = 0
    while True:
        entry = _SystemProcessInformation.from_buffer(buffer, offset)
        image = ""
        if entry.image_name.length and entry.image_name.buffer:
            image = ctypes.wstring_at(
                entry.image_name.buffer, entry.image_name.length // 2)
        pid = int(entry.unique_process_id or 0)
        if pid > 0 and image.lower() in {"osu!.exe", "osulazer.exe"}:
            processes[pid] = {
                "pid": pid,
                "parent_pid": int(entry.parent_process_id or 0),
                "start_identity": str(int(entry.create_time)),
                "image_name": image,
            }
        if entry.next_entry_offset == 0:
            break
        offset += int(entry.next_entry_offset)
        if offset >= size:
            raise ValueError("Windows process snapshot is malformed")
    return processes


class WindowsClientJob:
    """Kernel-backed launch and cleanup for a Windows client tree."""

    def __init__(self, process_handle: int, process_id: int) -> None:
        if sys.platform != "win32":
            raise ValueError("Windows client jobs require Windows")
        self._kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        self._configure_prototypes()
        self._observed: dict[int, dict[str, Any]] = {}
        self._job = self._kernel32.CreateJobObjectW(None, None)
        if not self._job:
            self._raise_last_error("cannot create client job")
        self._port = self._kernel32.CreateIoCompletionPort(
            wintypes.HANDLE(-1), None, 0, 1)
        if not self._port:
            self.close()
            self._raise_last_error("cannot create client completion port")
        try:
            limits = _ExtendedLimitInformation()
            limits.basic_limit_information.limit_flags = (
                _JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
            self._set_information(
                _JOB_OBJECT_EXTENDED_LIMIT_INFORMATION, limits)
            association = _CompletionPortAssociation(
                ctypes.c_void_p(1), self._port)
            self._set_information(
                _JOB_OBJECT_ASSOCIATE_COMPLETION_PORT_INFORMATION,
                association,
            )
            if not self._kernel32.AssignProcessToJobObject(
                    self._job, wintypes.HANDLE(process_handle)):
                self._raise_last_error("cannot assign client to private job")
            self._resume_process(process_id)
        except Exception:
            self.close()
            raise
        self.drain()

    @classmethod
    def launch(
        cls,
        command: list[str],
        *,
        cwd: str,
        env: dict[str, str],
    ) -> tuple[subprocess.Popen[bytes], WindowsClientJob]:
        if sys.platform != "win32":
            raise ValueError("supervised client launch requires Windows")
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdout=None,
            stderr=None,
            start_new_session=True,
            creationflags=_CREATE_SUSPENDED,
        )
        try:
            handle = getattr(process, "_handle", None)
            if not isinstance(handle, int):
                raise ValueError(
                    "launched osu client lacks a Windows process handle")
            return process, cls(handle, process.pid)
        except Exception:
            process.kill()
            raise

    def _configure_prototypes(self) -> None:
        api = self._kernel32
        api.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        api.CreateJobObjectW.restype = wintypes.HANDLE
        api.SetInformationJobObject.argtypes = [
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        api.SetInformationJobObject.restype = wintypes.BOOL
        api.AssignProcessToJobObject.argtypes = [
            wintypes.HANDLE, wintypes.HANDLE]
        api.AssignProcessToJobObject.restype = wintypes.BOOL
        api.CreateIoCompletionPort.argtypes = [
            wintypes.HANDLE, wintypes.HANDLE, ctypes.c_size_t, wintypes.DWORD]
        api.CreateIoCompletionPort.restype = wintypes.HANDLE
        api.GetQueuedCompletionStatus.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(wintypes.DWORD),
            ctypes.POINTER(ctypes.c_size_t),
            ctypes.POINTER(ctypes.c_void_p),
            wintypes.DWORD,
        ]
        api.GetQueuedCompletionStatus.restype = wintypes.BOOL
        api.CreateToolhelp32Snapshot.argtypes = [
            wintypes.DWORD, wintypes.DWORD]
        api.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
        api.Thread32First.argtypes = [
            wintypes.HANDLE, ctypes.POINTER(_ThreadEntry)]
        api.Thread32First.restype = wintypes.BOOL
        api.Thread32Next.argtypes = [
            wintypes.HANDLE, ctypes.POINTER(_ThreadEntry)]
        api.Thread32Next.restype = wintypes.BOOL
        api.OpenThread.argtypes = [
            wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        api.OpenThread.restype = wintypes.HANDLE
        api.ResumeThread.argtypes = [wintypes.HANDLE]
        api.ResumeThread.restype = wintypes.DWORD
        api.CloseHandle.argtypes = [wintypes.HANDLE]
        api.CloseHandle.restype = wintypes.BOOL

    def _set_information(self, info_class: int, value: ctypes.Structure) -> None:
        if not self._kernel32.SetInformationJobObject(
                self._job, info_class, ctypes.byref(value), ctypes.sizeof(value)):
            self._raise_last_error("cannot configure client job")

    @staticmethod
    def _raise_last_error(message: str) -> None:
        error = ctypes.get_last_error()
        raise OSError(error, message)

    def _resume_process(self, process_id: int) -> None:
        snapshot = self._kernel32.CreateToolhelp32Snapshot(
            _TH32CS_SNAPTHREAD, 0)
        if snapshot == ctypes.c_void_p(-1).value:
            self._raise_last_error("cannot enumerate suspended client threads")
        found = False
        entry = _ThreadEntry()
        entry.dwSize = ctypes.sizeof(entry)
        try:
            has_entry = self._kernel32.Thread32First(
                snapshot, ctypes.byref(entry))
            while has_entry:
                if int(entry.th32OwnerProcessID) == process_id:
                    found = True
                    thread = self._kernel32.OpenThread(
                        _THREAD_SUSPEND_RESUME, False,
                        int(entry.th32ThreadID))
                    if not thread:
                        self._raise_last_error(
                            "cannot open suspended client thread")
                    try:
                        previous = self._kernel32.ResumeThread(thread)
                        if previous != 1:
                            if previous == _INVALID_DWORD:
                                self._raise_last_error(
                                    "cannot resume suspended client thread")
                            raise ValueError(
                                "client thread had an unexpected suspend count")
                    finally:
                        self._kernel32.CloseHandle(thread)
                has_entry = self._kernel32.Thread32Next(
                    snapshot, ctypes.byref(entry))
        finally:
            self._kernel32.CloseHandle(snapshot)
        if not found:
            raise ValueError("suspended client has no primary thread")

    def drain(self) -> None:
        if not getattr(self, "_port", None):
            return
        while True:
            message = wintypes.DWORD()
            key = ctypes.c_size_t()
            process_id = ctypes.c_void_p()
            ok = self._kernel32.GetQueuedCompletionStatus(
                self._port,
                ctypes.byref(message),
                ctypes.byref(key),
                ctypes.byref(process_id),
                0,
            )
            if not ok:
                error = ctypes.get_last_error()
                if error == _WAIT_TIMEOUT:
                    return
                raise OSError(error, "cannot read client job notifications")
            pid = process_id.value
            if not isinstance(pid, int) or pid <= 0:
                if message.value == _JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO:
                    continue
                raise ValueError("client job returned an invalid process identity")
            if message.value == _JOB_OBJECT_MSG_NEW_PROCESS:
                self._observed[pid] = {
                    "pid": pid,
                    "observed_monotonic_ns": time.monotonic_ns(),
                    "exited": False,
                }
            elif message.value in {
                    _JOB_OBJECT_MSG_EXIT_PROCESS,
                    _JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS}:
                record = self._observed.setdefault(pid, {
                    "pid": pid,
                    "observed_monotonic_ns": time.monotonic_ns(),
                })
                record["exited"] = True

    def observed(self, pid: int) -> dict[str, Any] | None:
        self.drain()
        return self._observed.get(pid)

    def close(self) -> None:
        port = getattr(self, "_port", None)
        job = getattr(self, "_job", None)
        self._port = None
        self._job = None
        if port:
            self._kernel32.CloseHandle(port)
        if job:
            self._kernel32.CloseHandle(job)
