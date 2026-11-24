#[cfg(target_os = "linux")]
use super::unix as native;
#[cfg(windows)]
use super::windows as native;
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum SafePathError {
    #[error("malformed percent encoding or non-UTF-8 path")]
    InvalidEncoding,
    #[error("absolute, parent, prefix, or NUL path component")]
    InvalidComponent,
    #[error("resolved path escapes its configured root")]
    EscapesRoot,
    #[error("file-system error: {0}")]
    Io(#[from] io::Error),
}

/// A validated, already-decoded relative path. Parsing never percent-decodes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeRelativePath(PathBuf);

impl SafeRelativePath {
    pub fn parse(decoded: &str) -> Result<Self, SafePathError> {
        // Apply both separator conventions on every platform. In particular, a
        // Unix host must not interpret Windows prefixes as ordinary filenames.
        if decoded.contains('\0') || decoded.starts_with(['/', '\\']) {
            return Err(SafePathError::InvalidComponent);
        }
        #[cfg(windows)]
        if decoded.contains(':') {
            return Err(SafePathError::InvalidComponent);
        }
        for part in decoded.split(['/', '\\']) {
            if part == ".."
                || (part.as_bytes().get(1) == Some(&b':')
                    && part.as_bytes()[0].is_ascii_alphabetic())
            {
                return Err(SafePathError::InvalidComponent);
            }
        }
        let mut relative = PathBuf::new();
        for part in Path::new(decoded).components() {
            match part {
                Component::Normal(value) => relative.push(value),
                Component::CurDir => {}
                _ => return Err(SafePathError::InvalidComponent),
            }
        }
        Ok(Self(relative))
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// A directory retained from one successful open; children never reopen its name.
#[derive(Debug)]
pub struct SafeRoot {
    file: fs::File,
}
impl SafeRoot {
    /// Open a host-native absolute directory, following its configured alias once.
    pub fn open(path: &Path) -> Result<Self, SafePathError> {
        if !path.is_absolute() {
            return Err(SafePathError::InvalidComponent);
        }
        Ok(Self {
            file: native::open_root(path)?,
        })
    }
    pub fn open_child(&self, path: &SafeRelativePath) -> Result<SafeChild, SafePathError> {
        native::open_child(&self.file, path.as_path())
    }
}

/// Content and metadata obtained from the same opened object.
#[derive(Debug)]
pub struct SafeChild {
    pub file: fs::File,
    pub metadata: fs::Metadata,
}

/// Percent-decode a request path exactly once. `+` remains a literal plus because
/// this is a URL path, not form data.
pub fn decode_request_path(encoded: &str) -> Result<String, SafePathError> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(SafePathError::InvalidEncoding);
            }
            let high = hex(bytes[index + 1]).ok_or(SafePathError::InvalidEncoding)?;
            let low = hex(bytes[index + 2]).ok_or(SafePathError::InvalidEncoding)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    let decoded = String::from_utf8(decoded).map_err(|_| SafePathError::InvalidEncoding)?;
    if decoded.contains('\0') {
        return Err(SafePathError::InvalidComponent);
    }
    Ok(decoded)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
