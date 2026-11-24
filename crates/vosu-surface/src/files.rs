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
