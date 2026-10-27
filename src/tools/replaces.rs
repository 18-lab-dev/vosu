//! Catalog-authorized identity migration. The durable intent is written before
//! any rename; recovery rolls forward before discovery can expose partial state.
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

impl ToolHost {
    fn replacement_journal(&self) -> PathBuf {
        self.config.join("tool-replacement.json")
    }
    pub(super) fn replacement_stage(&self) -> PathBuf {
        // Nested below a directory without a manifest: discovery never sees it.
        self.data.join(".replacement-stage").join("package")
    }
    fn tool_data(&self, id: &str) -> PathBuf {
        self.data
            .parent()
            .unwrap_or(&self.data)
            .join("tool-data")
            .join(id)
    }
    fn tool_settings(&self, id: &str) -> PathBuf {
        self.config.join("settings").join(format!("{id}.json"))
    }
    pub(super) fn replacement_for(&self, id: &str) -> Result<Option<catalog::Tool>, String> {
        let old = self.entry(id)?;
        let saved = self.saved.lock();
        let state = saved.get(id).ok_or("tool has no catalog install record")?;
        let catalog = self.catalog.lock();
        let claimed = catalog
            .tools
            .iter()
            .any(|tool| tool.replaces.iter().any(|old| old == id));
        if !claimed {
            return Ok(None);
        }
        if old.source == "dev" || !state.trusted || state.catalog_source.is_none() {
            return Err("replacement requires a trusted catalog-installed tool".into());
        }
        let mut candidates = catalog.tools.iter().filter(|tool| {
            tool.replaces.iter().any(|old| old == id)
                && state.catalog_source.as_deref() == Some(&tool.source)
        });
        let replacement = candidates.next().cloned();
        if candidates.next().is_some() {
            return Err(format!("ambiguous catalog replacement for {id}"));
        }
        if replacement.is_none() {
            return Err("replacement is from a different catalog source".into());
        }
        Ok(replacement)
    }
    pub(super) fn update_target(&self, id: &str) -> Option<String> {
        self.replacement_for(id).ok().flatten().map(|tool| tool.id)
    }
    pub(super) fn prepare_replacement(
        &self,
        old: &str,
        tool: &catalog::Tool,
        publish: bool,
    ) -> Result<(), String> {
        let entry = self.entry(old)?;
        let mut state = self
            .saved
            .lock()
            .get(old)
            .cloned()
            .ok_or("missing catalog install record")?;
        if entry.source == "dev"
            || !state.trusted
            || state.catalog_source.as_deref() != Some(&tool.source)
            || !tool.replaces.iter().any(|id| id == old)
        {
            return Err(
                "replacement must come from the installed tool's trusted catalog source".into(),
            );
        }
        if self.entry(&tool.id).is_ok() || self.saved.lock().contains_key(&tool.id) {
            return Err(format!(
                "replacement target {} already exists; nothing was overwritten",
                tool.id
            ));
        }
        for target in [
            self.data.join(&tool.id),
            self.tool_data(&tool.id),
            self.tool_settings(&tool.id),
            self.cache.join(&tool.id),
        ] {
            if exists(&target).map_err(|e| e.to_string())? {
                return Err(format!(
                    "replacement target {} already exists; nothing was overwritten",
                    target.display()
                ));
            }
        }
        for id in &tool.replaces {
            if id != old && self.entry(id).is_ok() {
                return Err(
                    "multiple replaced tools are installed; cannot merge their data".into(),
                );
            }
        }
        for id in &tool.replaces {
            if !state.replaces.contains(id) {
                state.replaces.push(id.clone());
            }
        }
        let transaction = Replacement {
            old: old.into(),
            new: tool.id.clone(),
            state,
            data: exists(&self.tool_data(old)).map_err(|e| e.to_string())?,
            settings: exists(&self.tool_settings(old)).map_err(|e| e.to_string())?,
            cache: exists(&self.cache.join(old)).map_err(|e| e.to_string())?,
        };
        if publish {
            atomic_json(&self.replacement_journal(), &transaction).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub(super) fn recover_replacement(&self) -> Result<(), String> {
        let path = self.replacement_journal();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let tx: Replacement = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if !manifest::valid_id(&tx.old)
            || !manifest::valid_id(&tx.new)
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
        self.cli_sessions
            .lock()
            .retain(|_, session| session.tool != tx.old);
        self.sessions.lock().retain(|_, owner| owner != &tx.old);
        self.codes.lock().retain(|_, code| code.tool != tx.old);
        Ok(())
    }
}

#[cfg(test)]
#[path = "replaces_tests.rs"]
mod tests;
