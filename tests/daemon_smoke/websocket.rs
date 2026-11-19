// Raw WebSocket client helpers for the daemon smoke test, included by
// `tests/daemon_smoke.rs` so they share its imports.

async fn websocket_connect(
    port: u16,
    path: &str,
    origin: Option<&str>,
) -> (tokio::net::TcpStream, Vec<u8>) {
    websocket_connect_with_bearer(port, path, origin, None, 101).await
}

async fn websocket_connect_with_bearer(
    port: u16,
    path: &str,
    origin: Option<&str>,
    bearer: Option<&str>,
    status: u16,
) -> (tokio::net::TcpStream, Vec<u8>) {
    let authorization = bearer
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    websocket_connect_with_authorization(port, path, origin, &authorization, status).await
}

async fn websocket_connect_with_authorization(
    port: u16,
    path: &str,
    origin: Option<&str>,
    authorization: &str,
    status: u16,
) -> (tokio::net::TcpStream, Vec<u8>) {
    let origin = origin
        .map(|origin| format!("Origin: {origin}\r\n"))
        .unwrap_or_default();
    let mut stream = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\n\
                 Host: 127.0.0.1:{port}\r\n{origin}{authorization}\
                 Connection: Upgrade\r\n\
                 Upgrade: websocket\r\n\
                 Sec-WebSocket-Version: 13\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let mut response = Vec::new();
    let header_end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(offset) = response.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "daemon closed during WebSocket upgrade");
            response.extend_from_slice(&chunk[..read]);
        }
    })
    .await
    .expect("WebSocket upgrade timed out");
    assert!(
        response[..header_end].starts_with(format!("HTTP/1.1 {status}").as_bytes()),
        "unexpected WebSocket upgrade response: {}",
        String::from_utf8_lossy(&response[..header_end])
    );
    if authorization.starts_with("Sec-WebSocket-Protocol:") && status == 101 {
        let headers = String::from_utf8_lossy(&response[..header_end]).to_ascii_lowercase();
        assert!(headers.contains("sec-websocket-protocol: vosu.key\r\n"));
        assert!(!headers.contains("vosu_k_"));
    }
    (stream, response.split_off(header_end))
}

fn masked_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 126);
    let mask = [0x12, 0x34, 0x56, 0x78];
    let mut frame = Vec::with_capacity(payload.len() + 6);
    frame.extend_from_slice(&[0x80 | opcode, 0x80 | payload.len() as u8]);
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    frame
}

async fn read_server_frame(
    stream: &mut tokio::net::TcpStream,
    buffered: &mut Vec<u8>,
) -> (u8, Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while buffered.len() < 2 {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "daemon closed before WebSocket frame");
            buffered.extend_from_slice(&chunk[..read]);
        }
        assert_eq!(buffered[1] & 0x80, 0, "server frames must not be masked");
        let short_len = (buffered[1] & 0x7f) as usize;
        let (header_len, payload_len) = match short_len {
            126 => {
                while buffered.len() < 4 {
                    let mut chunk = [0_u8; 1024];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(read, 0, "daemon closed inside WebSocket header");
                    buffered.extend_from_slice(&chunk[..read]);
                }
                (4, u16::from_be_bytes([buffered[2], buffered[3]]) as usize)
            }
            127 => panic!("smoke frame unexpectedly exceeds 64 KiB"),
            len => (2, len),
        };
        while buffered.len() < header_len + payload_len {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "daemon closed inside WebSocket payload");
            buffered.extend_from_slice(&chunk[..read]);
        }
        let frame: Vec<_> = buffered.drain(..header_len + payload_len).collect();
        (frame[0] & 0x0f, frame[header_len..].to_vec())
    })
    .await
    .expect("server WebSocket frame timed out")
}
