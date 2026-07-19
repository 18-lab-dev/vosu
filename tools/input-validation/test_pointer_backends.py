from __future__ import annotations

import ctypes
import errno
import math
import os
import sys
import unittest
from types import SimpleNamespace
from unittest import mock

import pointer_backends
from pointer_backends import (
    DryRunPointerBackend,
    PlayfieldRect,
    PointerBackendError,
    UInputPointerBackend,
    UnsupportedPointerBackendError,
    WindowsPointerBackend,
    create_pointer_backend,
)


class PlayfieldRectTests(unittest.TestCase):
    def test_transform_corners_center_and_clamping(self) -> None:
        rect = PlayfieldRect(-100, 50, 513, 385)

        self.assertEqual(rect.transform(0, 0), (-100.0, 50.0))
        self.assertEqual(rect.transform(512, 384), (412.0, 434.0))
        self.assertEqual(rect.transform(256, 192), (156.0, 242.0))
        self.assertEqual(rect.transform(-20, 900), (-100.0, 434.0))
        self.assertEqual(rect.transform(700, -1), (412.0, 50.0))

    def test_transform_preserves_fractional_desktop_positions(self) -> None:
        rect = PlayfieldRect(10, 20, 100, 200)
        self.assertEqual(rect.transform(256, 192), (59.5, 119.5))

    def test_invalid_rectangles_are_rejected(self) -> None:
        for values in ((0, 0, 1, 10), (0, 0, 10, 0), (0, 0, -1, 10)):
            with self.subTest(values=values), self.assertRaises(ValueError):
                PlayfieldRect(*values)
        for values in ((True, 0, 10, 10), (0, 0, 10.0, 10)):
            with self.subTest(values=values), self.assertRaises(TypeError):
                PlayfieldRect(*values)  # type: ignore[arg-type]
        with self.assertRaises(ValueError):
            PlayfieldRect(2**31 - 1, 0, 2, 10)

    def test_non_finite_coordinates_are_rejected(self) -> None:
        rect = PlayfieldRect(0, 0, 100, 100)
        for value in (math.inf, -math.inf, math.nan):
            with self.subTest(value=value), self.assertRaises(ValueError):
                rect.transform(value, 0)


class DryRunBackendTests(unittest.TestCase):
    def test_records_transformed_actions_deterministically(self) -> None:
        backend = create_pointer_backend("dry-run", PlayfieldRect(100, 200, 513, 385))
        self.assertIsInstance(backend, DryRunPointerBackend)

        backend.move(0, 0)
        backend.move(256, 192)
        backend.move(999, -1)

        self.assertEqual(
            backend.actions,
            [(100.0, 200.0), (356.0, 392.0), (612.0, 200.0)],
        )

    def test_close_is_idempotent_and_prevents_moves(self) -> None:
        backend = DryRunPointerBackend(PlayfieldRect(0, 0, 513, 385))
        backend.close()
        backend.close()
        with self.assertRaisesRegex(PointerBackendError, "closed"):
            backend.move(1, 2)

    def test_invalid_backend_names_and_types_are_rejected(self) -> None:
        rect = PlayfieldRect(0, 0, 513, 385)
        with self.assertRaisesRegex(ValueError, "unknown pointer backend"):
            create_pointer_backend("memory", rect)
        with self.assertRaises(TypeError):
            create_pointer_backend(1, rect)  # type: ignore[arg-type]
        with self.assertRaises(TypeError):
            create_pointer_backend("dry-run", (0, 0, 513, 385))  # type: ignore[arg-type]

    def test_explicit_platform_backend_is_rejected_off_platform(self) -> None:
        rect = PlayfieldRect(0, 0, 513, 385)
        if sys.platform != "win32":
            with self.assertRaises(UnsupportedPointerBackendError):
                WindowsPointerBackend(rect)
        if not sys.platform.startswith("linux"):
            with self.assertRaises(UnsupportedPointerBackendError):
                UInputPointerBackend(rect)


class WindowsMappingTests(unittest.TestCase):
    def test_virtual_desktop_normalization_clamps(self) -> None:
        normalize = WindowsPointerBackend._normalize
        self.assertEqual(normalize(-1920, -1920, 3840), 0)
        self.assertEqual(normalize(1919, -1920, 3840), 65535)
        self.assertEqual(normalize(-0.5, -1920, 3840), 32768)
        self.assertEqual(normalize(-5000, -1920, 3840), 0)
        self.assertEqual(normalize(5000, -1920, 3840), 65535)


    def test_sendinput_uses_one_absolute_input_and_close_is_idempotent(self) -> None:
        metrics = {
            WindowsPointerBackend._SM_XVIRTUALSCREEN: -1920,
            WindowsPointerBackend._SM_YVIRTUALSCREEN: 0,
            WindowsPointerBackend._SM_CXVIRTUALSCREEN: 3840,
            WindowsPointerBackend._SM_CYVIRTUALSCREEN: 1080,
        }
        get_metrics = mock.Mock(side_effect=metrics.__getitem__)
        send_input = mock.Mock(return_value=1)
        user32 = SimpleNamespace(GetSystemMetrics=get_metrics, SendInput=send_input)
        with (
            mock.patch.object(sys, "platform", "win32"),
            mock.patch.object(ctypes, "WinDLL", return_value=user32, create=True),
        ):
            backend = WindowsPointerBackend(PlayfieldRect(100, 200, 513, 385))

        input_pointer = backend._input_pointer
        backend.move(256, 192)
        self.assertIs(send_input.call_args.args[1], input_pointer)
        self.assertEqual(
            backend._input.mi.dx,
            backend._normalize(356.0, -1920, 3840),
        )
        self.assertEqual(
            backend._input.mi.dy,
            backend._normalize(392.0, 0, 1080),
        )
        self.assertEqual(
            backend._input.mi.dwFlags,
            WindowsPointerBackend._MOUSEEVENTF_MOVE
            | WindowsPointerBackend._MOUSEEVENTF_VIRTUALDESK
            | WindowsPointerBackend._MOUSEEVENTF_ABSOLUTE,
        )
        backend.close()
        backend.close()
        with self.assertRaisesRegex(PointerBackendError, "closed"):
            backend.move(0, 0)


@unittest.skipUnless(sys.platform.startswith("linux"), "uinput format is Linux-specific")
class UInputBackendTests(unittest.TestCase):
    def _new_backend(self, ioctl_side_effect=None):
        writes: list[object] = []

        def write(_fd: int, data: object) -> int:
            writes.append(data)
            return len(data)  # type: ignore[arg-type]

        patches = (
            mock.patch.object(os, "open", return_value=41),
            mock.patch.object(os, "write", side_effect=write),
            mock.patch.object(os, "close"),
            mock.patch("fcntl.ioctl", side_effect=ioctl_side_effect),
            mock.patch.object(UInputPointerBackend, "_wait_until_observable"),
        )
        mocks = [patch.start() for patch in patches]
        self.addCleanup(lambda: [patch.stop() for patch in reversed(patches)])
        backend = UInputPointerBackend(
            PlayfieldRect(704, 348, 513, 385),
            PlayfieldRect(0, 0, 1920, 1080),
        )
        return backend, writes, mocks

    def test_one_device_lifetime_move_packet_and_idempotent_close(self) -> None:
        backend, writes, mocks = self._new_backend()
        open_mock, _, close_mock, ioctl_mock, readiness_mock = mocks
        event_buffer_id = id(backend._events)

        backend.move(256, 192)
        backend.move(999, -10)

        self.assertEqual(open_mock.call_count, 1)
        self.assertEqual(id(backend._events), event_buffer_id)
        self.assertIs(writes[1], backend._events)
        self.assertIs(writes[2], backend._events)
        event = pointer_backends._INPUT_EVENT
        packet = writes[2]
        self.assertEqual(event.unpack_from(packet, 0)[2:], (3, 0, 1216))
        self.assertEqual(event.unpack_from(packet, event.size)[2:], (3, 1, 348))
        self.assertEqual(event.unpack_from(packet, event.size * 2)[2:], (0, 0, 0))
        readiness_mock.assert_called_once()

        backend.close()
        backend.close()
        close_mock.assert_called_once_with(41)
        destroy_calls = [
            call for call in ioctl_mock.call_args_list if call.args[1] == pointer_backends._UI_DEV_DESTROY
        ]
        self.assertEqual(len(destroy_calls), 1)
        with self.assertRaisesRegex(PointerBackendError, "closed"):
            backend.move(0, 0)

    def test_declares_libinput_pointer_classification_capabilities(self) -> None:
        _, _, mocks = self._new_backend()
        ioctl_mock = mocks[3]
        self.assertIn(
            mock.call(41, pointer_backends._UI_SET_EVBIT, pointer_backends._EV_KEY),
            ioctl_mock.call_args_list,
        )
        self.assertIn(
            mock.call(41, pointer_backends._UI_SET_KEYBIT, pointer_backends._BTN_LEFT),
            ioctl_mock.call_args_list,
        )
        self.assertIn(
            mock.call(
                41,
                pointer_backends._UI_SET_PROPBIT,
                pointer_backends._INPUT_PROP_POINTER,
            ),
            ioctl_mock.call_args_list,
        )

    def test_unsupported_pointer_property_ioctl_is_optional(self) -> None:
        def ioctl(_fd: int, command: int, *_args) -> None:
            if command == pointer_backends._UI_SET_PROPBIT:
                raise OSError(errno.EINVAL, "unsupported property ioctl")

        backend, _, _ = self._new_backend(ioctl)
        backend.close()

    def test_descriptor_spans_desktop_while_subrectangle_stays_non_edge(self) -> None:
        backend, writes, _ = self._new_backend()
        descriptor = pointer_backends._UINPUT_USER_DEV.unpack(writes[0])
        abs_max_start = 6
        abs_min_start = abs_max_start + 64
        self.assertEqual(descriptor[abs_max_start + 0], 1919)
        self.assertEqual(descriptor[abs_max_start + 1], 1079)
        self.assertEqual(descriptor[abs_min_start + 0], 0)
        self.assertEqual(descriptor[abs_min_start + 1], 0)

        backend.move(256, 192)
        event = pointer_backends._INPUT_EVENT
        packet = writes[1]
        center_x = event.unpack_from(packet, 0)[4]
        center_y = event.unpack_from(packet, event.size)[4]
        self.assertEqual((center_x, center_y), (960, 540))
        self.assertNotIn(center_x, (0, 1919))
        self.assertNotIn(center_y, (0, 1079))

    def test_linux_desktop_geometry_is_required_and_must_contain_playfield(self) -> None:
        rect = PlayfieldRect(100, 100, 513, 385)
        with self.assertRaisesRegex(PointerBackendError, "desktop geometry"):
            UInputPointerBackend(rect)
        with self.assertRaisesRegex(ValueError, "contained"):
            UInputPointerBackend(rect, PlayfieldRect(0, 0, 200, 200))

    def test_permission_error_is_actionable_and_does_not_close_invalid_fd(self) -> None:
        with (
            mock.patch.object(os, "open", side_effect=PermissionError("denied")),
            mock.patch.object(os, "close") as close_mock,
        ):
            with self.assertRaisesRegex(PermissionError, "grant this process uinput access"):
                UInputPointerBackend(
                    PlayfieldRect(0, 0, 513, 385),
                    PlayfieldRect(0, 0, 1920, 1080),
                )
        close_mock.assert_not_called()


if __name__ == "__main__":
    unittest.main()
