use super::FileTarget;
use axum::http::{HeaderMap, HeaderValue, header};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct FileValidators {
    pub etag: String,
    pub modified: Option<SystemTime>,
}

fn quoted_hash(prefix: &str, digest: impl IntoIterator<Item = u8>) -> String {
    let mut value = String::with_capacity(prefix.len() + 66);
    value.push('"');
    value.push_str(prefix);
    for byte in digest {
        write!(value, "{byte:02x}").expect("String writing cannot fail");
    }
    value.push('"');
    value
}

impl FileValidators {
    pub fn for_file(target: &FileTarget, lazer: bool) -> Self {
        let hash = lazer
            .then(|| target.identity.file_name().and_then(|p| p.to_str()))
            .flatten()
            .filter(|hash| {
                matches!(hash.len(), 32 | 64) && hash.bytes().all(|b| b.is_ascii_hexdigit())
            });
        let etag = if let Some(hash) = hash {
            format!("\"{hash}\"")
        } else {
            let mut digest = Sha256::new();
            digest.update(target.identity.as_os_str().as_encoded_bytes());
            digest.update(target.length.to_le_bytes());
            if let Some(time) = target
                .modified
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            {
                digest.update(time.as_nanos().to_le_bytes());
            }
            quoted_hash("file-", digest.finalize())
        };
        Self {
            etag,
            modified: target.modified,
        }
    }

    pub fn not_modified(&self, request: &HeaderMap) -> bool {
        if request.contains_key(header::IF_NONE_MATCH) {
            return request
                .get_all(header::IF_NONE_MATCH)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .any(|line| {
                    line.split(',')
                        .map(str::trim)
                        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == self.etag)
                });
        }
        let Some(modified) = self
            .modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        else {
            return false;
        };
        request
            .get(header::IF_MODIFIED_SINCE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| httpdate::parse_http_date(v).ok())
            .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|since| modified.as_secs() <= since.as_secs())
    }

    pub fn apply(&self, headers: &mut HeaderMap) {
        headers.insert(
            header::ETAG,
            HeaderValue::from_str(&self.etag).expect("generated ETag is ASCII"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        if let Some(modified) = self.modified.filter(|t| {
            t.duration_since(UNIX_EPOCH)
                .ok()
                .is_some_and(|d| d.as_secs() < 253_402_300_800)
        }) {
            headers.insert(
                header::LAST_MODIFIED,
                HeaderValue::from_str(&httpdate::fmt_http_date(modified))
                    .expect("HTTP dates are ASCII"),
            );
        }
    }
}

pub fn representation_validators(bytes: &[u8], modified: Option<SystemTime>) -> FileValidators {
    FileValidators {
        etag: quoted_hash("sha256-", Sha256::digest(bytes)),
        modified,
    }
}
