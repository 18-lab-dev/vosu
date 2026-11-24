//! Retained filesystem capabilities shared by core and HTTP file serving.
mod confined;
#[cfg(target_os = "linux")]
mod unix;
#[cfg(windows)]
mod windows;

pub use confined::{SafeChild, SafePathError, SafeRelativePath, SafeRoot, decode_request_path};

#[cfg(test)]
mod tests;
