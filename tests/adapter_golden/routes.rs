//! Raw HTTP/1.1 exchanges with the replay's real listener. The request bytes
//! are sent exactly as given, nothing is followed or retried, and the status,
//! ordered headers and message body come back as received (transfer coding
//! removed).
use super::replay::WAIT;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
};
use vosu::server::websocket::Schema;

/// Larger than the 8 MiB capture budget, so a bound hit is a failure.
const MAX_RESPONSE: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct RawResponse {
    pub status: u16,
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

/// Sends `request` verbatim and reads the response until the server closes.
pub fn exchange(address: SocketAddr, request: &[u8]) -> Result<RawResponse, String> {
    let mut stream = TcpStream::connect_timeout(&address, WAIT).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(WAIT))
        .and_then(|()| stream.set_write_timeout(Some(WAIT)))
        .map_err(|e| e.to_string())?;
    stream.write_all(request).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > MAX_RESPONSE {
        return Err("response exceeds the replay bound".into());
    }
    parse(&raw, request.starts_with(b"HEAD "))
}

fn parse(raw: &[u8], head: bool) -> Result<RawResponse, String> {
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("response has no header terminator")?;
    let text = std::str::from_utf8(&raw[..end]).map_err(|_| "response head is not UTF-8")?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or("empty response")?;
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
        status,
        headers,
        body: Vec::new(),
    };
    let rest = &raw[end + 4..];
    if head {
        if !rest.is_empty() {
            return Err("HEAD response carries a body".into());
        }
        return Ok(response);
    }
    response.body = if response
        .header("transfer-encoding")
        .is_some_and(|coding| coding.eq_ignore_ascii_case("chunked"))
    {
        dechunk(rest)?
    } else if let Some(length) = response.header("content-length") {
        let length: usize = length.parse().map_err(|_| "bad content-length")?;
        if rest.len() != length {
            return Err(format!(
                "body is {} bytes, content-length {length}",
                rest.len()
            ));
        }
        rest.to_vec()
    } else {
        rest.to_vec()
    };
    Ok(response)
}

fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("unterminated chunk size")?;
        let size = std::str::from_utf8(&rest[..line])
            .ok()
            .and_then(|size| usize::from_str_radix(size.split(';').next()?.trim(), 16).ok())
            .ok_or("bad chunk size")?;
        rest = &rest[line + 2..];
        if size == 0 {
            return Ok(body);
        }
        if rest.len() < size + 2 || &rest[size..size + 2] != b"\r\n" {
            return Err("truncated chunk".into());
        }
        body.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}
