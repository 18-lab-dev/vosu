//! Roll-forward recovery of a 1.x catalog identity migration (`replaces`)
//! whose durable intent was written before a crash. Catalog v2 never starts
//! one (I0 refuses an add whose `replaces` names an installed package); a
//! journal left by 1.x is completed before discovery can expose partial state.
use super::*;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    old: String,
    new: String,
    state: Saved,
    data: bool,
    settings: bool,
    cache: bool,
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    fs::File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent"))?,
    )?
    .sync_all()
}
#[cfg(windows)]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}

// std::fs::rename can overwrite an empty directory on Unix. Neither a user
// folder nor settings may be clobbered, including between check and rename.
#[cfg(target_os = "linux")]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to_c = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both paths are live NUL-terminated C strings; no pointers escape.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    sync_parent(to)?;
    Ok(())
}
#[cfg(windows)]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both paths are live NUL-terminated UTF-16 strings.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn move_once(from: &Path, to: &Path, present: bool) -> io::Result<()> {
    match (exists(from)?, exists(to)?, present) {
        (true, false, true) => {
            fs::create_dir_all(
                to.parent()
                    .ok_or_else(|| io::Error::other("missing parent"))?,
            )?;
            rename_new(from, to)?;
            sync_parent(from)
        }
        (false, true, true) | (false, false, false) => Ok(()),
        _ => Err(io::Error::other(format!(
            "replacement storage conflict: {} -> {}",
            from.display(),
            to.display()
        ))),
    }
}

impl ToolRegistry {
    fn replacement_journal(&self) -> PathBuf {
        self.config.join("tool-replacement.json")
    }
    fn replacement_stage(&self) -> PathBuf {
        // Nested below a directory without a manifest: discovery never sees it.
        self.data.join(".replacement-stage").join("package")
    }
    pub(super) fn recover_replacement(&self) -> Result<(), String> {
        let path = self.replacement_journal();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let tx: Replacement = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if !crate::server::ids::valid_id(&tx.old)
            || !crate::server::ids::valid_id(&tx.new)
            || tx.old == tx.new
            || !tx.state.trusted
            || tx.state.catalog_source.is_none()
            || !tx.state.replaces.contains(&tx.old)
        {
            return Err("invalid tool replacement journal".into());
        }
        let apply = || -> io::Result<()> {
            move_once(&self.replacement_stage(), &self.data.join(&tx.new), true)?;
            move_once(&self.tool_data(&tx.old), &self.tool_data(&tx.new), tx.data)?;
            move_once(
                &self.tool_settings(&tx.old),
                &self.tool_settings(&tx.new),
                tx.settings,
            )?;
            move_once(
                &self.cache.join(&tx.old),
                &self.cache.join(&tx.new),
                tx.cache,
            )?;
            let mut saved = self.saved.lock();
            let mut next = saved.clone();
            next.remove(&tx.old);
            next.insert(tx.new.clone(), tx.state.clone());
            atomic_json(&self.config.join("state.json"), &next)?;
            *saved = next;
            drop(saved);
            let old_root = self.data.join(&tx.old);
            if exists(&old_root)? {
                fs::remove_dir_all(&old_root)?;
                sync_parent(&old_root)?;
            }
            let stage = self.data.join(".replacement-stage");
            if exists(&stage)? {
                fs::remove_dir(&stage)?;
                sync_parent(&stage)?;
            }
            fs::remove_file(&path)?;
            sync_parent(&path)?;
            Ok(())
        };
        apply().map_err(|e| format!("tool replacement recovery: {e}"))?;
        self.invalidate_tool(&tx.old);
        Ok(())
    }
}

#[cfg(test)]
#[path = "replaces_tests.rs"]
mod tests;
