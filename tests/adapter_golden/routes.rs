//! Raw HTTP/1.1 exchanges with the replay's real listener. The request bytes
//! are sent exactly as given, nothing is followed or retried, and the status,
//! ordered headers and message body come back as received (transfer coding
//! removed). The body is framed by the response itself (`content-length`,
//! chunked, no body, or connection close), so a recorded request without
//! `Connection: close` does not wait for the server's idle timeout.
use super::replay::WAIT;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
};
use vosu_core::server::websocket::Schema;

/// Larger than the 8 MiB capture budget, so a bound hit is a failure.
const MAX_RESPONSE: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct RawResponse {
    /// The status line as received, without its CRLF.
    pub status_line: String,
    pub status: u16,
    /// Every header in received order, names as sent, values trimmed.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl RawResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The production state route of each capture schema.
pub fn state_path(schema: Schema) -> &'static str {
    match schema {
        Schema::V1 => "/json",
        Schema::StreamCompanion => "/json/sc",
        Schema::V2 => "/json/v2",
        Schema::V2Precise => "/json/v2/precise",
    }
}

/// `GET path` with only `Host` and `Connection: close`.
pub fn get(address: SocketAddr, path: &str) -> RawResponse {
    exchange(
        address,
        format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .unwrap_or_else(|error| panic!("GET {path}: {error}"))
}

/// Sends `request` verbatim and reads exactly one response. When the
/// request asks the server to close, nothing may follow that response (a
/// `HEAD` response carrying a body fails here).
pub fn exchange(address: SocketAddr, request: &[u8]) -> Result<RawResponse, String> {
    exchange_or_close(address, request)?
        .ok_or_else(|| "connection closed without a response".to_owned())
}

/// Like [`exchange`], but a connection the server closes or resets before
/// any response byte is `Ok(None)`: the golden `closed` outcome.
pub fn exchange_or_close(
    address: SocketAddr,
    request: &[u8],
) -> Result<Option<RawResponse>, String> {
    let mut wire = Wire::connect(address)?;
    let sent = wire.send(request);
    if wire.closed_before_response()? {
        return Ok(None);
    }
    sent?;
    let response = wire.response(request.starts_with(b"HEAD "))?;
    let head = String::from_utf8_lossy(request).to_ascii_lowercase();
    if head.contains("\r\nconnection: close\r\n") && !wire.rest()?.is_empty() {
        return Err("bytes follow the response".into());
    }
    Ok(Some(response))
}

/// One buffered client connection.
pub struct Wire {
    stream: TcpStream,
    buffer: Vec<u8>,
    read: usize,
}

impl Wire {
    pub fn connect(address: SocketAddr) -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&address, WAIT).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(WAIT))
            .and_then(|()| stream.set_write_timeout(Some(WAIT)))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stream,
            buffer: Vec::new(),
            read: 0,
        })
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.stream.write_all(bytes).map_err(|e| e.to_string())
    }

    /// Reads the first response bytes; `true` when the server closed or
    /// reset the connection before sending any.
    fn closed_before_response(&mut self) -> Result<bool, String> {
        let mut chunk = [0_u8; 16 * 1024];
        match self.stream.read(&mut chunk) {
            Ok(0) => Ok(true),
            Ok(count) => {
                self.read += count;
                self.buffer.extend_from_slice(&chunk[..count]);
                Ok(false)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                Ok(true)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Reads more bytes; `Ok(false)` at end of stream.
    fn fill(&mut self) -> Result<bool, String> {
        let mut chunk = [0_u8; 16 * 1024];
        let count = self.stream.read(&mut chunk).map_err(|e| e.to_string())?;
        self.read += count;
        if self.read > MAX_RESPONSE {
            return Err("response exceeds the replay bound".into());
        }
        self.buffer.extend_from_slice(&chunk[..count]);
        Ok(count != 0)
    }

    /// Takes exactly `count` bytes.
    pub fn take(&mut self, count: usize) -> Result<Vec<u8>, String> {
        while self.buffer.len() < count {
            if !self.fill()? {
                return Err(format!(
                    "connection closed after {} of {count} bytes",
                    self.buffer.len()
                ));
            }
        }
        Ok(self.buffer.drain(..count).collect())
    }

    /// Takes everything up to and including `delimiter`, returning the part
    /// before it.
    fn until(&mut self, delimiter: &[u8]) -> Result<Vec<u8>, String> {
        loop {
            if let Some(at) = self
                .buffer
                .windows(delimiter.len())
                .position(|window| window == delimiter)
            {
                let mut part: Vec<u8> = self.buffer.drain(..at + delimiter.len()).collect();
                part.truncate(at);
                return Ok(part);
            }
            if !self.fill()? {
                return Err("connection closed inside a delimited part".into());
            }
        }
    }

    fn rest(&mut self) -> Result<Vec<u8>, String> {
        while self.fill()? {}
        Ok(std::mem::take(&mut self.buffer))
    }

    /// Reads one response. `head` marks a response to `HEAD`, which never
    /// has a body.
    pub fn response(&mut self, head: bool) -> Result<RawResponse, String> {
        let raw = self.until(b"\r\n\r\n")?;
        let text = std::str::from_utf8(&raw).map_err(|_| "response head is not UTF-8")?;
        let mut lines = text.split("\r\n");
        let status_line = lines.next().ok_or("empty response")?.to_owned();
        let status = status_line
            .strip_prefix("HTTP/1.1 ")
            .and_then(|rest| rest.get(..3))
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| format!("bad status line {status_line:?}"))?;
        let headers = lines
            .map(|line| {
                line.split_once(':')
                    .map(|(name, value)| (name.to_owned(), value.trim().to_owned()))
                    .ok_or_else(|| format!("bad header {line:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut response = RawResponse {
            status_line,
            status,
            headers,
            body: Vec::new(),
        };
        if head || status == 101 || status == 204 || status == 304 || status < 200 {
            return Ok(response);
        }
        response.body = if response
            .header("transfer-encoding")
            .is_some_and(|coding| coding.eq_ignore_ascii_case("chunked"))
        {
            self.dechunk()?
        } else if let Some(length) = response.header("content-length") {
            let length: usize = length.parse().map_err(|_| "bad content-length")?;
            self.take(length)?
        } else {
            self.rest()?
        };
        Ok(response)
    }

    fn dechunk(&mut self) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        loop {
            let line = self.until(b"\r\n")?;
            let size = std::str::from_utf8(&line)
                .ok()
                .and_then(|size| usize::from_str_radix(size.split(';').next()?.trim(), 16).ok())
                .ok_or("bad chunk size")?;
            if size == 0 {
                // No trailers are expected; the final CRLF ends the message.
                if !self.until(b"\r\n")?.is_empty() {
                    return Err("unexpected chunked trailer".into());
                }
                return Ok(body);
            }
            body.extend(self.take(size)?);
            if self.take(2)? != b"\r\n" {
                return Err("truncated chunk".into());
            }
        }
    }
}
