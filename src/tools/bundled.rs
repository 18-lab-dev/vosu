use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

include!(concat!(env!("OUT_DIR"), "/bundled_tools.rs"));

pub struct Bundle {
    pub root: PathBuf,
    _lease: fs::File,
}

/// `None` when this build embeds no tools (development and CI builds), so
/// they do not create an empty bundled cache tree on every start.
pub fn materialize(cache: &Path) -> io::Result<Option<Bundle>> {
    if BUNDLED_TOOLS.is_empty() {
        return Ok(None);
    }
    materialize_files(cache, env!("CARGO_PKG_VERSION"), BUNDLED_TOOLS).map(Some)
}
fn bundle_lease(cache: &Path, name: &str) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join(format!(".{name}.lock")))
}
fn verify(root: &Path, files: &[(&str, &[u8])]) -> bool {
    fn names(root: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other("symlink in cached bundle"));
            }
            if kind.is_dir() {
                names(root, &entry.path(), out)?;
            } else if kind.is_file() {
                out.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            } else {
                return Err(io::Error::other("non-regular bundled file"));
            }
        }
        Ok(())
    }
    let mut actual = Vec::new();
    names(root, root, &mut actual).is_ok()
        && actual.len() == files.len()
        && files.iter().all(|(name, bytes)| {
            actual.iter().any(|found| found == name)
                && fs::read(root.join(name)).is_ok_and(|data| data == *bytes)
        })
}
pub(super) fn materialize_files(
    cache: &Path,
    version: &str,
    files: &[(&str, &[u8])],
) -> io::Result<Bundle> {
    let mut digest = Sha256::new();
    for (name, data) in files {
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((data.len() as u64).to_le_bytes());
        digest.update(data);
    }
    let hash = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    fs::create_dir_all(cache)?;
    let name = format!("bundled-{version}-{}", &hash[..16]);
    let target = cache.join(&name);
    let lease = bundle_lease(cache, &name)?;
    lease.lock_shared()?;
    if !verify(&target, files) {
        lease.unlock()?;
        lease
            .try_lock()
            .map_err(|_| io::Error::other("bundled cache is corrupt and currently in use"))?;
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        let temp = tempfile::Builder::new()
            .prefix(".bundled-")
            .tempdir_in(cache)?;
        for (name, data) in files {
            if !super::manifest::relative(name) {
                return Err(io::Error::other("unsafe embedded tool path"));
            }
            let path = temp.path().join(name);
            fs::create_dir_all(path.parent().unwrap())?;
            let mut file = fs::File::create(&path)?;
            file.write_all(data)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if Path::new(name)
                    .components()
                    .any(|part| part.as_os_str() == "bin")
                {
                    file.set_permissions(fs::Permissions::from_mode(0o755))?;
                }
            }
            file.sync_all()?;
        }
        fs::rename(temp.path(), &target)?;
        #[cfg(unix)]
        fs::File::open(cache)?.sync_all()?;
        lease.unlock()?;
        lease.lock_shared()?;
    }
    for item in fs::read_dir(cache)? {
        let item = item?;
        if item.path() != target
            && item.file_name().to_string_lossy().starts_with("bundled-")
            && item.file_type()?.is_dir()
        {
            let old_lease = bundle_lease(cache, &item.file_name().to_string_lossy())?;
            if old_lease.try_lock().is_ok() {
                fs::remove_dir_all(item.path())?;
            }
        }
    }
    Ok(Bundle {
        root: target,
        _lease: lease,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verifies_reuses_and_cleans() {
        let cache = tempfile::tempdir().unwrap();
        let files: &[(&str, &[u8])] = &[("demo/tool.toml", b"hello")];
        let a = materialize_files(cache.path(), "1", files).unwrap();
        let a_root = a.root.clone();
        assert_eq!(
            a.root,
            materialize_files(cache.path(), "1", files).unwrap().root
        );
        let b = materialize_files(cache.path(), "2", files).unwrap();
        assert!(a_root.exists(), "live bundle must not be removed");
        assert!(b.root.exists());
        drop(a);
        materialize_files(cache.path(), "2", files).unwrap();
        assert!(!a_root.exists());
        let b_root = b.root.clone();
        drop(b);
        fs::write(b_root.join("demo/tool.toml"), "corrupt").unwrap();
        materialize_files(cache.path(), "2", files).unwrap();
        assert_eq!(fs::read(b_root.join("demo/tool.toml")).unwrap(), b"hello");
    }
}
