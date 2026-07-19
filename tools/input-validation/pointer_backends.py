from __future__ import annotations

import ctypes
import errno
import math
import os
import struct
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol


_PLAYFIELD_WIDTH = 512.0
_PLAYFIELD_HEIGHT = 384.0
_INT32_MIN = -(2**31)
_INT32_MAX = 2**31 - 1


class PointerBackendError(RuntimeError):
    """Base class for pointer backend setup and injection failures."""


class UnsupportedPointerBackendError(PointerBackendError):
    """Raised when a pointer backend is unavailable on this platform."""


@dataclass(frozen=True, slots=True)
class PlayfieldRect:
    x: int
    y: int
    width: int
    height: int

    def __post_init__(self) -> None:
        for name in ("x", "y", "width", "height"):
            value = getattr(self, name)
            if isinstance(value, bool) or not isinstance(value, int):
                raise TypeError(f"{name} must be an integer")
        if self.width < 2 or self.height < 2:
            raise ValueError("playfield rectangle width and height must be at least 2")
        if not (_INT32_MIN <= self.x <= _INT32_MAX and _INT32_MIN <= self.y <= _INT32_MAX):
            raise ValueError("playfield rectangle origin is outside the signed 32-bit range")
        if self.right > _INT32_MAX or self.bottom > _INT32_MAX:
            raise ValueError("playfield rectangle extent is outside the signed 32-bit range")

    @property
    def right(self) -> int:
        return self.x + self.width - 1

    @property
    def bottom(self) -> int:
        return self.y + self.height - 1

    @staticmethod
    def _transform_axis(value: float, origin: int, extent: int, source_extent: float) -> float:
        try:
            source = float(value)
        except (TypeError, ValueError) as error:
            raise TypeError("pointer coordinates must be real numbers") from error
        if not math.isfinite(source):
            raise ValueError("pointer coordinates must be finite")
        source = min(source_extent, max(0.0, source))
        return origin + source * (extent - 1) / source_extent

    def transform(self, x: float, y: float) -> tuple[float, float]:
        """Clamp osu! playfield coordinates and map them into this rectangle."""
        return (
            self._transform_axis(x, self.x, self.width, _PLAYFIELD_WIDTH),
            self._transform_axis(y, self.y, self.height, _PLAYFIELD_HEIGHT),
        )


class PointerBackend(Protocol):
    def move(self, x: float, y: float) -> None: ...

    def close(self) -> None: ...


class DryRunPointerBackend:
    """Deterministic backend recording transformed desktop pixel positions."""

    def __init__(self, rect: PlayfieldRect) -> None:
        if not isinstance(rect, PlayfieldRect):
            raise TypeError("rect must be a PlayfieldRect")
        self.rect = rect
        self.actions: list[tuple[float, float]] = []
        self.closed = False

    def move(self, x: float, y: float) -> None:
        if self.closed:
            raise PointerBackendError("pointer backend is closed")
        self.actions.append(self.rect.transform(x, y))

    def close(self) -> None:
        self.closed = True


class _MOUSEINPUT(ctypes.Structure):
    _fields_ = (
        ("dx", ctypes.c_long),
        ("dy", ctypes.c_long),
        ("mouseData", ctypes.c_uint32),
        ("dwFlags", ctypes.c_uint32),
        ("time", ctypes.c_uint32),
        ("dwExtraInfo", ctypes.c_size_t),
    )


class _INPUT_UNION(ctypes.Union):
    _fields_ = (("mi", _MOUSEINPUT),)


class _INPUT(ctypes.Structure):
    _anonymous_ = ("value",)
    _fields_ = (("type", ctypes.c_uint32), ("value", _INPUT_UNION))


class WindowsPointerBackend:
    """Global Windows pointer injection using SendInput absolute movement."""

    _SM_XVIRTUALSCREEN = 76
    _SM_YVIRTUALSCREEN = 77
    _SM_CXVIRTUALSCREEN = 78
    _SM_CYVIRTUALSCREEN = 79
    _INPUT_MOUSE = 0
    _MOUSEEVENTF_MOVE = 0x0001
    _MOUSEEVENTF_VIRTUALDESK = 0x4000
    _MOUSEEVENTF_ABSOLUTE = 0x8000

    def __init__(self, rect: PlayfieldRect) -> None:
        if not isinstance(rect, PlayfieldRect):
            raise TypeError("rect must be a PlayfieldRect")
        if sys.platform != "win32":
            raise UnsupportedPointerBackendError("the windows pointer backend requires Windows")
        self.rect = rect
        user32 = ctypes.WinDLL("user32", use_last_error=True)
        user32.GetSystemMetrics.argtypes = (ctypes.c_int,)
        user32.GetSystemMetrics.restype = ctypes.c_int
        user32.SendInput.argtypes = (ctypes.c_uint, ctypes.POINTER(_INPUT), ctypes.c_int)
        user32.SendInput.restype = ctypes.c_uint
        self._send_input = user32.SendInput
        self._virtual_x = user32.GetSystemMetrics(self._SM_XVIRTUALSCREEN)
        self._virtual_y = user32.GetSystemMetrics(self._SM_YVIRTUALSCREEN)
        self._virtual_width = user32.GetSystemMetrics(self._SM_CXVIRTUALSCREEN)
        self._virtual_height = user32.GetSystemMetrics(self._SM_CYVIRTUALSCREEN)
        if self._virtual_width <= 0 or self._virtual_height <= 0:
            raise PointerBackendError("Windows reported an invalid virtual desktop")
        self._input = _INPUT()
        self._input.type = self._INPUT_MOUSE
        self._input.mi.dwFlags = (
            self._MOUSEEVENTF_MOVE | self._MOUSEEVENTF_VIRTUALDESK | self._MOUSEEVENTF_ABSOLUTE
        )
        self._input_pointer = ctypes.pointer(self._input)
        self._closed = False

    @staticmethod
    def _normalize(pixel: float, origin: int, size: int) -> int:
        if size == 1:
            return 0
        normalized = round((pixel - origin) * 65535 / (size - 1))
        return min(65535, max(0, normalized))

    def move(self, x: float, y: float) -> None:
        if self._closed:
            raise PointerBackendError("pointer backend is closed")
        pixel_x = self.rect._transform_axis(x, self.rect.x, self.rect.width, _PLAYFIELD_WIDTH)
        pixel_y = self.rect._transform_axis(y, self.rect.y, self.rect.height, _PLAYFIELD_HEIGHT)
        self._input.mi.dx = self._normalize(pixel_x, self._virtual_x, self._virtual_width)
        self._input.mi.dy = self._normalize(pixel_y, self._virtual_y, self._virtual_height)
        if self._send_input(1, self._input_pointer, ctypes.sizeof(_INPUT)) != 1:
            error = ctypes.get_last_error()
            raise OSError(error, "SendInput failed")

    def close(self) -> None:
        self._closed = True


# Linux input/uinput constants and native structures.
_EV_SYN = 0x00
_EV_KEY = 0x01
_EV_ABS = 0x03
_SYN_REPORT = 0
_ABS_X = 0
_ABS_Y = 1
_BTN_LEFT = 0x110
_INPUT_PROP_POINTER = 0x00
_BUS_USB = 0x03

_IOC_WRITE = 1
_IOC_NRSHIFT = 0
_IOC_TYPESHIFT = 8
_IOC_SIZESHIFT = 16
_IOC_DIRSHIFT = 30


def _io(type_: str, number: int) -> int:
    return (ord(type_) << _IOC_TYPESHIFT) | (number << _IOC_NRSHIFT)


def _iow(type_: str, number: int, size: int) -> int:
    return (
        (_IOC_WRITE << _IOC_DIRSHIFT)
        | (size << _IOC_SIZESHIFT)
        | (ord(type_) << _IOC_TYPESHIFT)
        | (number << _IOC_NRSHIFT)
    )


_UI_DEV_CREATE = _io("U", 1)
_UI_DEV_DESTROY = _io("U", 2)
_UI_SET_EVBIT = _iow("U", 100, struct.calcsize("i"))
_UI_SET_KEYBIT = _iow("U", 101, struct.calcsize("i"))
_UI_SET_ABSBIT = _iow("U", 103, struct.calcsize("i"))
_UI_SET_PROPBIT = _iow("U", 110, struct.calcsize("i"))
_UINPUT_USER_DEV = struct.Struct("@80sHHHHi" + "i" * (64 * 4))
_INPUT_EVENT = struct.Struct("@llHHi")


class UInputPointerBackend:
    """Global Linux EV_ABS pointer backed by one /dev/uinput device."""

    def __init__(
        self,
        rect: PlayfieldRect,
        desktop_rect: PlayfieldRect | None = None,
    ) -> None:
        if not isinstance(rect, PlayfieldRect):
            raise TypeError("rect must be a PlayfieldRect")
        if not sys.platform.startswith("linux"):
            raise UnsupportedPointerBackendError("the uinput pointer backend requires Linux")
        if desktop_rect is None:
            raise PointerBackendError(
                "the uinput pointer backend requires explicit Linux desktop geometry"
            )
        if not isinstance(desktop_rect, PlayfieldRect):
            raise TypeError("desktop_rect must be a PlayfieldRect")
        if (
            rect.x < desktop_rect.x
            or rect.y < desktop_rect.y
            or rect.right > desktop_rect.right
            or rect.bottom > desktop_rect.bottom
        ):
            raise ValueError("playfield rectangle must be contained in the Linux desktop rectangle")
        self.rect = rect
        self.desktop_rect = desktop_rect
        self._fd = -1
        self._closed = False
        self._created = False
        self._events = bytearray(_INPUT_EVENT.size * 3)
        device_name = f"vosu input validation pointer {os.getpid()}"
        try:
            import fcntl

            self._fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK | os.O_CLOEXEC)
            fcntl.ioctl(self._fd, _UI_SET_EVBIT, _EV_ABS)
            fcntl.ioctl(self._fd, _UI_SET_EVBIT, _EV_KEY)
            fcntl.ioctl(self._fd, _UI_SET_ABSBIT, _ABS_X)
            fcntl.ioctl(self._fd, _UI_SET_ABSBIT, _ABS_Y)
            fcntl.ioctl(self._fd, _UI_SET_KEYBIT, _BTN_LEFT)
            try:
                fcntl.ioctl(self._fd, _UI_SET_PROPBIT, _INPUT_PROP_POINTER)
            except OSError as error:
                if error.errno not in (errno.EINVAL, errno.ENOTTY, errno.EOPNOTSUPP):
                    raise
            abs_max = [0] * 64
            abs_min = [0] * 64
            abs_max[_ABS_X] = desktop_rect.right
            abs_max[_ABS_Y] = desktop_rect.bottom
            abs_min[_ABS_X] = desktop_rect.x
            abs_min[_ABS_Y] = desktop_rect.y
            descriptor = _UINPUT_USER_DEV.pack(
                device_name.encode(),
                _BUS_USB,
                0x1D6B,
                0x0104,
                1,
                0,
                *abs_max,
                *abs_min,
                *([0] * 64),
                *([0] * 64),
            )
            self._write_all(descriptor)
            fcntl.ioctl(self._fd, _UI_DEV_CREATE)
            self._created = True
            self._wait_until_observable(device_name)
        except PermissionError as error:
            self._cleanup_failed_init()
            raise PermissionError(
                "permission denied opening /dev/uinput; grant this process uinput access"
            ) from error
        except FileNotFoundError as error:
            self._cleanup_failed_init()
            raise UnsupportedPointerBackendError(
                "/dev/uinput is unavailable; load the uinput kernel module"
            ) from error
        except Exception:
            self._cleanup_failed_init()
            raise

    @staticmethod
    def _wait_until_observable(device_name: str) -> None:
        deadline = time.monotonic() + 1.0
        while True:
            for name_path in Path("/sys/class/input").glob("input*/name"):
                try:
                    if name_path.read_text(encoding="utf-8").strip() == device_name:
                        time.sleep(0.05)
                        return
                except OSError:
                    continue
            if time.monotonic() >= deadline:
                raise PointerBackendError("uinput pointer did not become observable")
            time.sleep(0.01)

    def _cleanup_failed_init(self) -> None:
        if self._fd >= 0:
            if self._created:
                try:
                    import fcntl

                    fcntl.ioctl(self._fd, _UI_DEV_DESTROY)
                except OSError:
                    pass
            os.close(self._fd)
            self._fd = -1
        self._created = False
        self._closed = True

    def _write_all(self, data: bytes | bytearray) -> None:
        written = os.write(self._fd, data)
        if written != len(data):
            raise OSError(f"short write to /dev/uinput: {written} of {len(data)} bytes")

    def move(self, x: float, y: float) -> None:
        if self._closed:
            raise PointerBackendError("pointer backend is closed")
        target_x = round(
            self.rect._transform_axis(x, self.rect.x, self.rect.width, _PLAYFIELD_WIDTH)
        )
        target_y = round(
            self.rect._transform_axis(y, self.rect.y, self.rect.height, _PLAYFIELD_HEIGHT)
        )
        _INPUT_EVENT.pack_into(self._events, 0, 0, 0, _EV_ABS, _ABS_X, target_x)
        _INPUT_EVENT.pack_into(self._events, _INPUT_EVENT.size, 0, 0, _EV_ABS, _ABS_Y, target_y)
        _INPUT_EVENT.pack_into(
            self._events, _INPUT_EVENT.size * 2, 0, 0, _EV_SYN, _SYN_REPORT, 0
        )
        self._write_all(self._events)

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        fd = self._fd
        self._fd = -1
        self._created = False
        try:
            import fcntl

            fcntl.ioctl(fd, _UI_DEV_DESTROY)
        finally:
            os.close(fd)


def create_pointer_backend(
    name: str,
    rect: PlayfieldRect,
    desktop_rect: PlayfieldRect | None = None,
) -> PointerBackend:
    if not isinstance(name, str):
        raise TypeError("backend name must be a string")
    if not isinstance(rect, PlayfieldRect):
        raise TypeError("rect must be a PlayfieldRect")
    if name == "dry-run":
        return DryRunPointerBackend(rect)
    if name == "windows":
        return WindowsPointerBackend(rect)
    if name == "uinput":
        return UInputPointerBackend(rect, desktop_rect)
    if name == "auto":
        if sys.platform == "win32":
            return WindowsPointerBackend(rect)
        if sys.platform.startswith("linux"):
            return UInputPointerBackend(rect, desktop_rect)
        raise UnsupportedPointerBackendError(
            f"no automatic pointer backend is available on {sys.platform!r}"
        )
    raise ValueError(
        f"unknown pointer backend {name!r}; expected auto, windows, uinput, or dry-run"
    )
