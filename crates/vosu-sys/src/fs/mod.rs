//! Retained filesystem capabilities shared by core and HTTP file serving.
mod confined;
mod directory;
mod identity;
#[cfg(target_os = "linux")]
mod unix;
#[cfg(windows)]
mod windows;

pub use confined::{SafeChild, SafePathError, SafeRelativePath, SafeRoot, decode_request_path};
pub use directory::SafeDirectory;
pub use identity::FileIdentity;

#[cfg(test)]
mod tests;
