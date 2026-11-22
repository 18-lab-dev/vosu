use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub fn dashboard_url(host: &str, port: u16) -> String {
    let host = browser_host(host);
    if host.contains(':') {
        format!("http://[{host}]:{port}/")
    } else {
        format!("http://{host}:{port}/")
    }
}

fn browser_host(host: &str) -> &str {
    match host {
        "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "::1",
        host => host.trim_start_matches('[').trim_end_matches(']'),
    }
}

/// A listening port alone is not identity: require the additive vosu response header.
/// The total timeout includes name resolution, connection, and a bounded header read.
pub async fn existing_dashboard(host: &str, port: u16) -> Option<String> {
    let host = browser_host(host);
    let probe = async {
        let mut stream = tokio::time::timeout(
            Duration::from_millis(400),
            tokio::net::TcpStream::connect((host, port)),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handoff connection"))??;
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        stream
            .write_all(
                format!(
                    "GET /favicon.ico HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await?;
        let mut header = [0_u8; 8192];
        let mut used = 0;
        loop {
            if used == header.len() {
                return Ok(false);
            }
            let count = stream.read(&mut header[used..]).await?;
            if count == 0 {
                return Ok(false);
            }
            used += count;
            if let Some(end) = header[..used]
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
            {
                return Ok::<_, io::Error>(identifies_vosu(&header[..end]));
            }
        }
    };
    match tokio::time::timeout(Duration::from_millis(1500), probe).await {
        Ok(Ok(true)) => Some(dashboard_url(host, port)),
        _ => None,
    }
}

fn identifies_vosu(bytes: &[u8]) -> bool {
    let Ok(header) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut lines = header.split("\r\n");
    if !lines
        .next()
        .is_some_and(|line| line.starts_with("HTTP/1.1 200 ") || line.starts_with("HTTP/1.0 200 "))
    {
        return false;
    }
    lines.any(|line| {
        line.split_once(':').is_some_and(|(key, value)| {
            key.eq_ignore_ascii_case("x-vosu-identity") && value.trim() == "vosu"
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_urls_use_loopback() {
        assert_eq!(dashboard_url("0.0.0.0", 24101), "http://127.0.0.1:24101/");
        assert_eq!(dashboard_url("::", 24101), "http://[::1]:24101/");
        assert_eq!(dashboard_url("[::1]", 24101), "http://[::1]:24101/");
    }

    #[test]
    fn identity_is_an_exact_header_not_body_text() {
        assert!(identifies_vosu(b"HTTP/1.1 200 OK\r\nX-Vosu-Identity: vosu"));
        assert!(!identifies_vosu(
            b"HTTP/1.1 404 Not Found\r\nX-Vosu-Identity: vosu"
        ));
        assert!(!identifies_vosu(b"HTTP/1.1 200 OK\r\nServer: vosu"));
        assert!(!identifies_vosu(
            b"HTTP/1.1 200 OK\r\nX-Vosu-Identity: not-vosu"
        ));
    }

    #[tokio::test]
    async fn positive_and_negative_listener_probe() {
        for identity in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                assert!(stream.read(&mut request).await.unwrap() > 0);
                let response: &[u8] = if identity {
                    b"HTTP/1.1 200 OK\r\nX-Vosu-Identity: vosu\r\nContent-Length: 0\r\n\r\n"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
                };
                stream.write_all(response).await.unwrap();
            });
            assert_eq!(
                existing_dashboard("127.0.0.1", port).await.is_some(),
                identity
            );
            server.await.unwrap();
        }
    }
}
