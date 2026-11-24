//! Path-safe static/game file resolution and byte-range response planning.
//!
//! Router code supplies an already selected root and a typed missing outcome. This
//! module does no state lookup and writes no response, keeping readiness decisions
//! explicit at the handler boundary.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use vosu_sys::fs::{SafeChild, SafePathError, SafeRelativePath, SafeRoot, decode_request_path};

mod directory;
mod skin;
mod validators;

pub use skin::{SkinFileIndex, SkinIndexCache};
pub use validators::{FileValidators, representation_validators};

pub use directory::{build_directory_index, encode_path_segment, html_escape};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingKind {
    /// The route is ready, but the requested/current file does not exist.
    NotFound,
    /// The file cannot be selected because osu! or beatmap state is not ready.
    NotReady,
}
impl MissingKind {
    pub const fn status_code(self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::NotReady => 500,
        }
    }
}

#[derive(Debug)]
pub enum ResourceOutcome {
    File(FileTarget),
    DirectoryIndex { html: String },
    Redirect { location: String },
    Missing(MissingKind),
}

#[derive(Debug)]
pub struct FileTarget {
    /// The already-opened file used for both metadata and response streaming.
    /// Handlers must stream this handle rather than reopening a path.
    pub file: fs::File,
    pub length: u64,
    pub content_type: &'static str,
    pub range_eligible: bool,
    pub modified: Option<std::time::SystemTime>,
    pub identity: PathBuf,
}

fn open_child(root: &Path, path: &SafeRelativePath) -> Result<SafeChild, SafePathError> {
    // Legacy configured roots may be relative. Resolve only the root spelling;
    // SafeRoot itself requires a host-native absolute capability declaration.
    let absolute;
    let root = if root.is_absolute() {
        root
    } else {
        absolute = std::env::current_dir()?.join(root);
        &absolute
    };
    SafeRoot::open(root)?.open_child(path)
}

/// Resolve a decoded route path. Invalid or escaping paths intentionally collapse
/// to NotFound so handlers do not disclose host file-system details.
pub fn resolve_resource(
    root: Option<&Path>,
    decoded_request: &str,
    request_target: &str,
    missing: MissingKind,
) -> Result<ResourceOutcome, SafePathError> {
    let Some(root) = root else {
        return Ok(ResourceOutcome::Missing(missing));
    };
    let child = match SafeRelativePath::parse(decoded_request) {
        Ok(child) => child,
        Err(SafePathError::InvalidComponent | SafePathError::EscapesRoot) => {
            return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
        }
        Err(error) => return Err(error),
    };
    resolve_child(root, child, decoded_request, request_target, missing)
}

fn resolve_child(
    root: &Path,
    child: SafeRelativePath,
    decoded_request: &str,
    request_target: &str,
    missing: MissingKind,
) -> Result<ResourceOutcome, SafePathError> {
    let opened = match open_child(root, &child) {
        Ok(opened) => opened,
        Err(SafePathError::EscapesRoot | SafePathError::InvalidComponent) => {
            return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
        }
        Err(SafePathError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(ResourceOutcome::Missing(missing));
        }
        Err(error) => return Err(error),
    };

    if opened.metadata.is_dir() {
        if !target_path_ends_in_slash(request_target) {
            return Ok(ResourceOutcome::Redirect {
                location: trailing_slash_location(request_target),
            });
        }
        return Ok(ResourceOutcome::DirectoryIndex {
            html: build_directory_index(&opened.file, request_target)?,
        });
    }
    if !opened.metadata.is_file() {
        return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
    }

    let identity = root.join(child.as_path());
    Ok(ResourceOutcome::File(FileTarget {
        file: opened.file,
        length: opened.metadata.len(),
        content_type: content_type(Path::new(decoded_request)),
        range_eligible: media_range_eligible(Path::new(decoded_request)),
        modified: opened.metadata.modified().ok(),
        identity,
    }))
}
/// Decode and resolve an encoded splat capture in one step. Malformed encoding is
/// a public 404, matching traversal handling, rather than an internal error.
pub fn resolve_encoded_resource(
    root: Option<&Path>,
    encoded_request: &str,
    request_target: &str,
    missing: MissingKind,
) -> Result<ResourceOutcome, SafePathError> {
    let decoded = match decode_request_path(encoded_request) {
        Ok(decoded) => decoded,
        Err(SafePathError::InvalidEncoding | SafePathError::InvalidComponent) => {
            return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
        }
        Err(error) => return Err(error),
    };
    resolve_resource(root, &decoded, request_target, missing)
}

/// Decode and resolve a counter resource. A top-level directory symlink is an
/// explicitly configured, separately confined mount.
pub fn resolve_encoded_counter_resource(
    root: Option<&Path>,
    encoded_request: &str,
    request_target: &str,
    missing: MissingKind,
) -> Result<ResourceOutcome, SafePathError> {
    let decoded = match decode_request_path(encoded_request) {
        Ok(decoded) => decoded,
        Err(SafePathError::InvalidEncoding | SafePathError::InvalidComponent) => {
            return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
        }
        Err(error) => return Err(error),
    };
    let Some(root) = root else {
        return Ok(ResourceOutcome::Missing(missing));
    };
    let logical_child = match SafeRelativePath::parse(&decoded) {
        Ok(child) => child,
        Err(SafePathError::InvalidComponent | SafePathError::EscapesRoot) => {
            return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
        }
        Err(error) => return Err(error),
    };

    let mut components = logical_child.as_path().components();
    let Some(alias_name) = components.next() else {
        return resolve_child(root, logical_child, &decoded, request_target, missing);
    };
    if alias_name
        .as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case("tools")
    {
        return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
    }
    let alias_path = root.join(alias_name.as_os_str());
    let is_symlink = match fs::symlink_metadata(&alias_path) {
        Ok(metadata) => metadata.file_type().is_symlink(),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            false
        }
        Err(error) => return Err(error.into()),
    };
    if !is_symlink {
        return resolve_child(root, logical_child, &decoded, request_target, missing);
    }

    let is_directory = fs::metadata(&alias_path)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    if !is_directory {
        return resolve_child(root, logical_child, &decoded, request_target, missing);
    }
    let mut relative = PathBuf::new();
    for component in components {
        relative.push(component.as_os_str());
    }

    resolve_child(
        &alias_path,
        SafeRelativePath::parse(&relative.to_string_lossy())?,
        &decoded,
        request_target,
        missing,
    )
}

fn target_path_ends_in_slash(target: &str) -> bool {
    target
        .split(['?', '#'])
        .next()
        .is_some_and(|path| path.ends_with('/'))
}

pub fn trailing_slash_location(target: &str) -> String {
    let split = target.find(['?', '#']).unwrap_or(target.len());
    let (path, suffix) = target.split_at(split);
    if path.ends_with('/') {
        target.to_owned()
    } else {
        format!("{path}/{suffix}")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ByteRange {
    Full,
    Partial { start: u64, end_inclusive: u64 },
}

impl ByteRange {
    pub const fn length(self, full_length: u64) -> u64 {
        match self {
            Self::Full => full_length,
            Self::Partial {
                start,
                end_inclusive,
            } => end_inclusive - start + 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RangeError {
    #[error("unsupported or malformed byte range")]
    Malformed,
    #[error("multiple byte ranges are unsupported")]
    Multiple,
    #[error("byte range is not satisfiable")]
    Unsatisfiable,
}

/// Parse one RFC 9110 byte range. Multiple ranges are rejected rather than
/// partially honored. End positions beyond EOF are malformed for upstream tosu compatibility
/// instead of being silently clamped.
pub fn parse_range(header: Option<&str>, full_length: u64) -> Result<ByteRange, RangeError> {
    let Some(header) = header else {
        return Ok(ByteRange::Full);
    };
    let header = header.trim();
    let specification = header.strip_prefix("bytes=").ok_or(RangeError::Malformed)?;
    if specification.contains(',') {
        return Err(RangeError::Multiple);
    }
    if specification.is_empty() || specification.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(RangeError::Malformed);
    }
    let (start, end) = specification.split_once('-').ok_or(RangeError::Malformed)?;
    if end.contains('-') {
        return Err(RangeError::Malformed);
    }
    if full_length == 0 {
        return Err(RangeError::Unsatisfiable);
    }

    if start.is_empty() {
        let suffix = parse_decimal(end)?;
        if suffix == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        let length = suffix.min(full_length);
        return Ok(ByteRange::Partial {
            start: full_length - length,
            end_inclusive: full_length - 1,
        });
    }

    let start = parse_decimal(start)?;
    if start >= full_length {
        return Err(RangeError::Unsatisfiable);
    }
    let end_inclusive = if end.is_empty() {
        full_length - 1
    } else {
        let end = parse_decimal(end)?;
        if end >= full_length || end < start {
            return Err(RangeError::Unsatisfiable);
        }
        end
    };
    Ok(ByteRange::Partial {
        start,
        end_inclusive,
    })
}

fn parse_decimal(value: &str) -> Result<u64, RangeError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RangeError::Malformed);
    }
    value.parse().map_err(|_| RangeError::Malformed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileResponsePlan {
    pub status: u16,
    pub offset: u64,
    pub content_length: u64,
    pub accept_ranges: Option<&'static str>,
    pub content_range: Option<String>,
    pub content_type: &'static str,
}

pub fn plan_file_response(
    target: &FileTarget,
    range_header: Option<&str>,
) -> Result<FileResponsePlan, RangeError> {
    let range = parse_range(range_header, target.length)?;
    match range {
        ByteRange::Full => Ok(FileResponsePlan {
            status: 200,
            offset: 0,
            content_length: target.length,
            accept_ranges: target.range_eligible.then_some("bytes"),
            content_range: None,
            content_type: target.content_type,
        }),
        ByteRange::Partial {
            start,
            end_inclusive,
        } => Ok(FileResponsePlan {
            status: 206,
            offset: start,
            content_length: end_inclusive - start + 1,
            accept_ranges: Some("bytes"),
            content_range: Some(format!("bytes {start}-{end_inclusive}/{}", target.length)),
            content_type: target.content_type,
        }),
    }
}

pub fn unsatisfied_content_range(full_length: u64) -> String {
    format!("bytes */{full_length}")
}

pub fn media_range_eligible(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("mp3" | "wav" | "ogg" | "gif" | "webm" | "mp4" | "avi" | "webp")
    )
}

pub fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    match extension {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png; charset=utf-8",
        "jpg" => "image/jpg; charset=utf-8",
        "jpeg" => "image/jpeg; charset=utf-8",
        "gif" => "image/gif; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "wav" => "audio/wav; charset=utf-8",
        "mp4" => "video/mp4; charset=utf-8",
        "woff" => "application/font-woff; charset=utf-8",
        "woff2" => "font/woff2; charset=utf-8",
        "ttf" => "application/font-ttf; charset=utf-8",
        "eot" => "application/vnd.ms-fontobject; charset=utf-8",
        "otf" => "application/font-otf; charset=utf-8",
        "wasm" => "application/wasm; charset=utf-8",
        "aac" => "audio/aac; charset=utf-8",
        "abw" => "application/x-abiword; charset=utf-8",
        "apng" => "image/apng; charset=utf-8",
        "arc" => "application/x-freearc; charset=utf-8",
        "avif" => "image/avif; charset=utf-8",
        "avi" => "video/x-msvideo; charset=utf-8",
        "bin" => "application/octet-stream; charset=utf-8",
        "bmp" => "image/bmp; charset=utf-8",
        "bz" => "application/x-bzip; charset=utf-8",
        "bz2" => "application/x-bzip2; charset=utf-8",
        "cda" => "application/x-cdf; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "gz" => "application/gzip; charset=utf-8",
        "ico" => "image/vnd.microsoft.icon; charset=utf-8",
        "jsonld" => "application/ld+json; charset=utf-8",
        "mid" | "midi" => "audio/x-midi; charset=utf-8",
        "mp3" => "audio/mpeg; charset=utf-8",
        "mpeg" => "video/mpeg; charset=utf-8",
        "mpkg" => "application/vnd.apple.installer+xml; charset=utf-8",
        "ogg" | "oga" => "audio/ogg; charset=utf-8",
        "ogv" => "video/ogg; charset=utf-8",
        "ogx" => "application/ogg; charset=utf-8",
        "opus" => "audio/opus; charset=utf-8",
        "rar" => "application/vnd.rar; charset=utf-8",
        "sh" => "application/x-sh; charset=utf-8",
        "tar" => "application/x-tar; charset=utf-8",
        "tif" | "tiff" => "image/tiff; charset=utf-8",
        "ts" => "video/mp2t; charset=utf-8",
        "txt" | "osu" => "text/plain; charset=utf-8",
        "weba" => "audio/webm; charset=utf-8",
        "webm" => "video/webm; charset=utf-8",
        "webp" => "image/webp; charset=utf-8",
        "xml" => "application/xmlz; charset=utf-8",
        "xhtml" => "application/xhtml+xml; charset=utf-8",
        "xul" => "application/vnd.mozilla.xul+xml; charset=utf-8",
        "zip" => "application/zip; charset=utf-8",
        "3gp" => "video/3gpp;; charset=utf-8",
        "3g2" => "video/3gpp2;; charset=utf-8",
        "7z" => "application/x-7z-compressed; charset=utf-8",
        "swf" => "application/x-shockwave-flash; charset=utf-8",
        _ => "",
    }
}

#[cfg(test)]
mod tests;
