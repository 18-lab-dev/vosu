from __future__ import annotations

import io
import socket
import ssl
import struct
import unittest
from unittest import mock

from input_validation import ValidationError, WebSocket, decode_frame, websocket_connect


class WebSocketFrameTests(unittest.TestCase):
    def test_decodes_small_and_extended_frames(self) -> None:
        small = decode_frame(io.BytesIO(b"\x81\x02hi"))
        self.assertTrue(small.fin)
        self.assertEqual((small.opcode, small.payload), (1, b"hi"))

        payload = b"x" * 130
        extended = decode_frame(io.BytesIO(b"\x82\x7e" + struct.pack("!H", len(payload)) + payload))
        self.assertEqual((extended.opcode, extended.payload), (2, payload))

    def test_rejects_masked_server_nonminimal_and_oversized_frames(self) -> None:
        cases = [
            b"\x81\x80" + b"mask",
            b"\x81\x7e\x00\x01x",
            b"\x81\x7f" + struct.pack("!Q", 1_048_577),
        ]
        for encoded in cases:
            with self.subTest(encoded=encoded[:2]), self.assertRaises(ValidationError):
                decode_frame(io.BytesIO(encoded))

    def test_rejects_fragmented_control_frame(self) -> None:
        with self.assertRaisesRegex(ValidationError, "control"):
            decode_frame(io.BytesIO(b"\x09\x00"))

    def test_receive_resumes_after_timeout_during_partial_frame(self) -> None:
        client, server = socket.socketpair()
        websocket = WebSocket(client)
        client.settimeout(0.01)
        try:
            server.sendall(b"\x81\x05he")
            with self.assertRaises(TimeoutError):
                websocket.receive_message()

            server.sendall(b"llo")
            self.assertEqual(websocket.receive_message(), b"hello")
        finally:
            websocket.close()
            server.close()

    def test_secure_url_creates_default_tls_context(self) -> None:
        raw = mock.Mock()
        context = mock.Mock()
        context.wrap_socket.side_effect = ssl.SSLError("test TLS boundary")
        with (
            mock.patch("input_validation.socket.create_connection", return_value=raw),
            mock.patch("input_validation.ssl.create_default_context", return_value=context),
            self.assertRaisesRegex(ssl.SSLError, "test TLS boundary"),
        ):
            websocket_connect("wss://example.test", 1.0)

        context.wrap_socket.assert_called_once_with(raw, server_hostname="example.test")
        raw.close.assert_called_once_with()



if __name__ == "__main__":
    unittest.main()
