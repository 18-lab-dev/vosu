use std::{fs::File, io};

/// Disk-backed representation identity sampled from an opened file, never from
/// a second pathname lookup. Change time detects writes with restored mtime.
/// A privileged Windows actor can still reset ChangeTime; this is not a
/// cryptographic content hash or protection against such an actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub size: u64,
    volume: u64,
    file_id: [u8; 16],
    modified: i128,
    changed: i128,
}

impl FileIdentity {
    /// Fixed, versioned identity input for cold resource-id preparation.
    pub fn encode(self) -> [u8; 65] {
        let mut bytes = [0; 65];
        bytes[0] = 1;
        bytes[1..9].copy_from_slice(&self.size.to_le_bytes());
        bytes[9..17].copy_from_slice(&self.volume.to_le_bytes());
        bytes[17..33].copy_from_slice(&self.file_id);
        bytes[33..49].copy_from_slice(&self.modified.to_le_bytes());
        bytes[49..65].copy_from_slice(&self.changed.to_le_bytes());
        bytes
    }

    #[cfg(target_os = "linux")]
    pub fn of(file: &File) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource is not a file",
            ));
        }
        let mut file_id = [0; 16];
        file_id[..8].copy_from_slice(&metadata.ino().to_le_bytes());
        Ok(Self {
            size: metadata.len(),
            volume: metadata.dev(),
            file_id,
            modified: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            changed: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
        })
    }

    #[cfg(windows)]
    pub fn of(file: &File) -> io::Result<Self> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_BASIC_INFO, FILE_ID_INFO, FILE_STANDARD_INFO, FileBasicInfo, FileIdInfo,
            FileStandardInfo,
        };
        let basic: FILE_BASIC_INFO = query(file, FileBasicInfo)?;
        let id: FILE_ID_INFO = query(file, FileIdInfo)?;
        let standard: FILE_STANDARD_INFO = query(file, FileStandardInfo)?;
        if standard.Directory || standard.EndOfFile < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource is not a file",
            ));
        }
        Ok(Self {
            size: standard.EndOfFile as u64,
            volume: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
            modified: i128::from(basic.LastWriteTime),
            changed: i128::from(basic.ChangeTime),
        })
    }
}

#[cfg(windows)]
fn query<T>(
    file: &File,
    class: windows_sys::Win32::Storage::FileSystem::FILE_INFO_BY_HANDLE_CLASS,
) -> io::Result<T> {
    use std::{
        mem::{MaybeUninit, size_of},
        os::windows::io::AsRawHandle,
    };
    use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx;
    let mut result = MaybeUninit::<T>::uninit();
    // SAFETY: the only callers pair each information class with its documented
    // output structure. The borrowed file handle remains open, and the buffer
    // is aligned and writable for exactly the size supplied to the API.
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            result.as_mut_ptr().cast(),
            size_of::<T>() as u32,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful GetFileInformationByHandleEx initialized this class's
    // complete output structure; none of these structures has invalid bit patterns.
    Ok(unsafe { result.assume_init() })
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "Tests create isolated temporary disk files"
    )]
    use super::*;

    #[test]
    fn resource_file_identity_distinguishes_files_with_equal_size_and_mtime() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("first.bin");
        let b = directory.path().join("second.bin");
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, b"bbbb").unwrap();
        let a = File::open(a).unwrap();
        let b = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(b)
            .unwrap();
        b.set_times(
            std::fs::FileTimes::new().set_modified(a.metadata().unwrap().modified().unwrap()),
        )
        .unwrap();
        let first = FileIdentity::of(&a).unwrap();
        let second = FileIdentity::of(&b).unwrap();
        assert_eq!(first.size, second.size);
        assert_eq!(first.modified, second.modified);
        assert_eq!(first.volume, second.volume);
        assert_ne!(first.file_id, second.file_id);
        assert_ne!(first.encode(), second.encode());
        assert_eq!(first, FileIdentity::of(&a).unwrap());
    }
}
