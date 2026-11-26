//! Opaque declared-root authority. Paths are data; only core supplies backends.
use std::{future::Future, pin::Pin, sync::Arc, time::SystemTime};

pub type Operation<'a, T> = Pin<Box<dyn Future<Output = Result<T, FileError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileError {
    NotFound,
    NotReady,
    PermissionDenied,
    TargetGone,
    InvalidPath,
    LimitExceeded,
    Io,
}
impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for FileError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativeChild(String);
impl RelativeChild {
    /// Already-decoded spelling. This does not percent-decode URL text.
    pub fn parse(path: &str) -> Result<Self, FileError> {
        if path.len() > 4096
            || path.contains('\0')
            || path.starts_with(['/', '\\'])
            || path.split(['/', '\\']).any(|part| {
                part == ".."
                    || (part.as_bytes().get(1) == Some(&b':')
                        && part.as_bytes()[0].is_ascii_alphabetic())
            })
        {
            return Err(FileError::InvalidPath);
        }
        Ok(Self(path.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct FileMetadata {
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Same-open-object identity, usable as an opaque validator.
    pub identity: Arc<[u8]>,
}

/// The implementation retains the opened object and bounds each offset read.
pub trait FileReader: Send + Sync {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>>;
}
pub struct OpenedFile {
    pub metadata: FileMetadata,
    reader: Box<dyn FileReader>,
}
impl OpenedFile {
    pub fn new(metadata: FileMetadata, reader: impl FileReader + 'static) -> Self {
        Self {
            metadata,
            reader: Box::new(reader),
        }
    }
    pub fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        self.reader.read(offset, len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Other,
}
#[derive(Clone, Debug)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}
#[derive(Debug)]
pub struct DirectoryPage {
    pub entries: Vec<DirectoryEntry>,
    /// More entries may exist. Keep paging before concluding absence.
    pub truncated: bool,
}
pub trait DirectoryReader: Send {
    fn next_page(&mut self) -> Operation<'_, DirectoryPage>;
}
pub trait RootOperations: Send + Sync {
    fn open(&self, path: &RelativeChild) -> Operation<'_, OpenedFile>;
    fn directory(&self, path: &RelativeChild) -> Operation<'_, Box<dyn DirectoryReader>>;
}
#[derive(Clone)]
pub struct RootLease(Arc<dyn RootOperations>);
impl RootLease {
    pub fn new(backend: impl RootOperations + 'static) -> Self {
        Self(Arc::new(backend))
    }
    pub fn open(&self, path: &RelativeChild) -> Operation<'_, OpenedFile> {
        self.0.open(path)
    }
    pub fn directory(&self, path: &RelativeChild) -> Operation<'_, Box<dyn DirectoryReader>> {
        self.0.directory(path)
    }
}

/// Implemented only by a core-minted bundled requester, never by a bus path.
pub trait FilesRequester: Send + Sync {
    fn declared_root(
        &self,
        session: &crate::state::IdentifiedSession,
        slot: &str,
    ) -> Result<RootLease, FileError>;
    fn open_calculation_path(
        &self,
        session: &crate::state::IdentifiedSession,
        path: &str,
    ) -> Operation<'_, OpenedFile>;
}

/// The one served-file media type table, shared by core and bundled hosts.
pub fn content_type(path: &std::path::Path) -> &'static str {
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
mod tests {
    use super::*;
    #[test]
    fn relative_children_are_decoded_data_not_root_authority() {
        for path in [
            "/root",
            r"\root",
            r"C:\root",
            "C:root",
            "../root",
            r"a\..\root",
            "a/../root",
            "a\0b",
        ] {
            assert_eq!(
                RelativeChild::parse(path),
                Err(FileError::InvalidPath),
                "{path}"
            );
        }
        assert_eq!(
            RelativeChild::parse("%252e%252e/chart.osu")
                .unwrap()
                .as_str(),
            "%252e%252e/chart.osu"
        );
        assert_eq!(
            RelativeChild::parse("Mixed Skin/HitCircle.png")
                .unwrap()
                .as_str(),
            "Mixed Skin/HitCircle.png"
        );
    }
}
