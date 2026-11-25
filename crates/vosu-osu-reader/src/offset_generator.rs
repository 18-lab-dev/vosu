use bytes::Bytes;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Cursor, Read, Seek},
    path::{Component, Path, PathBuf},
};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use vosu_net::{ZipLimits, extract_zip_atomic};

mod client_runtime;

const GENERATOR_RUNTIME: &str = "net8.0-8.0.29";
const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;
#[cfg(target_os = "linux")]
const RID: &str = "linux-x64";
#[cfg(windows)]
const RID: &str = "win-x64";
#[cfg(target_os = "linux")]
const GENERATOR: &str = "generator/lazer-offset-generator";
#[cfg(windows)]
const GENERATOR: &str = "generator/lazer-offset-generator.exe";
#[cfg(target_os = "linux")]
const TARGET_HOST: &str = "target/lazer-offset-target";
#[cfg(windows)]
const TARGET_HOST: &str = "target/lazer-offset-target.exe";

fn hash_reader(mut reader: impl Read, limit: u64) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "generator payload exceeds size limit",
            ));
        }
        hasher.update(&buffer[..size]);
    }
    use std::fmt::Write;
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(digest)
}

fn checksum(bytes: &[u8]) -> io::Result<String> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let digest = text.split_whitespace().next().unwrap_or_default();
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator bundle checksum is not SHA-256",
        ));
    }
    Ok(digest.to_ascii_lowercase())
}

#[derive(Deserialize)]
struct Manifest {
    rid: String,
    files: BTreeMap<String, String>,
}

fn validate_manifest(manifest: &Manifest) -> io::Result<()> {
    if manifest.rid != RID {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator bundle does not match this platform",
        ));
    }
    for (name, digest) in &manifest.files {
        if name.is_empty()
            || name.contains('\\')
            || name.contains(':')
            || Path::new(name)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || checksum(digest.as_bytes()).is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid generator manifest path or digest",
            ));
        }
    }
    if !manifest.files.contains_key(GENERATOR) || !manifest.files.contains_key(TARGET_HOST) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator bundle is missing its generator or owned target executable",
        ));
    }
    Ok(())
}

fn bundle_manifest(reader: impl Read + Seek) -> io::Result<Manifest> {
    let mut archive = zip::ZipArchive::new(reader)?;
    let mut bytes = Vec::new();
    archive
        .by_name("bundle.json")?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator manifest exceeds size limit",
        ));
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    validate_manifest(&manifest)?;
    if archive.len() != manifest.files.len() + 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator archive contains unmanifested files",
        ));
    }
    Ok(manifest)
}

fn verify_extracted(root: &Path, manifest: &Manifest) -> io::Result<()> {
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator cache is not a directory",
        ));
    }
    let mut directories = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            count += 1;
            if count > 2048 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "generator cache entry limit exceeded",
                ));
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel != "bundle.json" && !manifest.files.contains_key(&rel) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "generator cache contains an unmanifested file",
                    ));
                }
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "generator cache contains a symlink or special file",
                ));
            }
        }
    }
    for (name, expected) in &manifest.files {
        let path = root.join(name);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || hash_reader(File::open(&path)?, MAX_EXPANDED)? != *expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("corrupt generator payload {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn extract_bundle(cache_root: &Path, cancel: &CancellationToken) -> io::Result<PathBuf> {
    let (archive, checksum_text) =
        crate::assets::DaemonAssets::generator_bundle().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "embedded offset generator bundle is missing from this build",
            )
        })?;
    install_bundle(cache_root, archive, checksum_text, cancel)
}

fn install_bundle(
    cache_root: &Path,
    archive: &[u8],
    checksum_text: &str,
    cancel: &CancellationToken,
) -> io::Result<PathBuf> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let expected = checksum(checksum_text.as_bytes())?;
    if hash_reader(archive, MAX_ARCHIVE)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "embedded generator bundle checksum mismatch",
        ));
    }
    let manifest = bundle_manifest(Cursor::new(archive))?;
    let cache = cache_root.join(RID).join(GENERATOR_RUNTIME).join(&expected);
    let installed = cache.join("payload");
    fs::create_dir_all(&cache)?;
    if installed.is_dir() {
        verify_extracted(&installed, &manifest)?;
        return Ok(installed);
    }
    let mut archive_file = tempfile::NamedTempFile::new_in(&cache)?;
    std::io::Write::write_all(&mut archive_file, archive)?;
    let staging = tempfile::Builder::new()
        .prefix(".extract-")
        .tempdir_in(&cache)?;
    let payload = staging.path().join("payload");
    extract_zip_atomic(
        archive_file.path(),
        &payload,
        ZipLimits {
            max_entries: 2048,
            max_expanded_bytes: MAX_EXPANDED,
        },
        cancel,
    )
    .map_err(io::Error::other)?;
    verify_extracted(&payload, &manifest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(payload.join(GENERATOR), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(payload.join(TARGET_HOST), fs::Permissions::from_mode(0o700))?;
    }
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    match fs::rename(&payload, &installed) {
        Ok(()) => {}
        Err(_) if installed.is_dir() => {}
        Err(e) => return Err(e),
    }
    verify_extracted(&installed, &manifest)?;
    Ok(installed)
}

fn generator_command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command.kill_on_drop(true);
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("DOTNET_")
            || upper.starts_with("COMPLUS_")
            || upper.starts_with("CORECLR_")
        {
            command.env_remove(name);
        }
    }
    command
}

/// Creates one generator child process from a fully configured command.
///
/// The reader has no `vosu-sys` edge (M5 R1), so it cannot reach the
/// process-wide creation gate (`vosu_sys::process_spawn`, plugin-abi.md
/// §7.4) that keeps a plugin host's inheritable ring handles out of every
/// other child. Whoever composes the generator injects that gated spawn
/// (the bundled osu! plugin passes `vosu_sys::process_spawn::spawn_tokio`);
/// the reader never calls `Command::spawn` itself.
pub type SpawnCommand = fn(&mut Command) -> io::Result<tokio::process::Child>;

/// `Command::output` through the injected `spawn`: standard input null,
/// output and error captured, the wait outside the spawn and cancellable
/// (dropping the child kills it, `generator_command`'s `kill_on_drop`).
async fn output(
    spawn: SpawnCommand,
    command: &mut Command,
    cancel: &CancellationToken,
) -> io::Result<std::process::Output> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let child = spawn(command)?;
    tokio::select! {
        _ = cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")),
        result = child.wait_with_output() => result,
    }
}

pub struct EmbeddedOffsetGenerator {
    cache_root: PathBuf,
    spawn: SpawnCommand,
}
impl EmbeddedOffsetGenerator {
    /// `spawn` creates the generator's child processes; see [`SpawnCommand`].
    pub fn new(cache_root: PathBuf, spawn: SpawnCommand) -> Self {
        Self { cache_root, spawn }
    }
}
impl crate::memory::lazer_loader::OffsetGenerator for EmbeddedOffsetGenerator {
    fn generate<'a>(
        &'a self,
        executable_root: &'a Path,
        version: &'a str,
        cancel: &'a CancellationToken,
    ) -> crate::memory::lazer_loader::GenerationFuture<'a> {
        Box::pin(async move {
            let root = executable_root.to_path_buf();
            let version = version.to_owned();
            let cancel = cancel.child_token();
            let _cancel_on_drop = cancel.clone().drop_guard();
            let cache = self.cache_root.clone();
            let spawn = self.spawn;
            let runtime = tokio::runtime::Handle::current();
            // Keep filesystem work, JSON merging, and temporary-directory
            // destruction off async workers, including every error/cancel path.
            // Child processes still use cancellation-aware asynchronous output.
            tokio::task::spawn_blocking(move || {
                runtime.block_on(generate_offsets(cache, root, version, spawn, cancel))
            })
            .await
            .map_err(io::Error::other)?
        })
    }
}

// Polled only by the blocking-pool bridge above, not by an async worker.
async fn generate_offsets(
    cache: PathBuf,
    root: PathBuf,
    version: String,
    spawn: SpawnCommand,
    cancel: CancellationToken,
) -> io::Result<Bytes> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let discovered =
        crate::memory::lazer_version::discover_from_root(&root).map_err(io::Error::other)?;
    if discovered != version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "client version changed before generation",
        ));
    }
    let assembly = [root.clone(), root.join("usr/bin"), root.join("bin")]
        .into_iter()
        .find(|p| p.join("osu!.dll").is_file() || p.join("osu.Game.dll").is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not locate osu!lazer managed assemblies",
            )
        })?;
    let payload = extract_bundle(&cache, &cancel)?;
    let work = tempfile::Builder::new()
        .prefix(".generation-")
        .tempdir_in(payload.parent().expect("payload has cache parent"))?;
    let staged = client_runtime::stage(&payload, &assembly, work.path())?;
    let template = work.path().join("template.json");
    let generated = work.path().join("offsets.json");
    let asset = crate::assets::DaemonAssets::get("offsets.json")
        .ok_or_else(|| io::Error::other("embedded offsets template is missing"))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the template is generator input in a disposable work directory; the generated offsets are validated before atomic installation"
    )]
    fs::write(&template, asset.data.as_ref())?;
    let mut cmd = generator_command(&staged.join(GENERATOR));
    cmd.args(["--assemblies-only", "--assembly-dir"])
        .arg(&assembly)
        .args(["--template"])
        .arg(&template)
        .args(["--output"])
        .arg(&generated);
    let result = output(spawn, &mut cmd, &cancel).await?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "assembly-only offset generator failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    let mut sig = generator_command(&staged.join(TARGET_HOST));
    sig.args(["--game-base-signature", "--assembly-dir"])
        .arg(&assembly);
    let signature = output(spawn, &mut sig, &cancel).await?;
    if !signature.status.success() {
        return Err(io::Error::other(format!(
            "owned target could not derive GameBaseVtable: {}",
            String::from_utf8_lossy(&signature.stderr).trim()
        )));
    }
    let bytes = merge_game_base_vtable(&fs::read(generated)?, &signature.stdout)?;
    let after =
        crate::memory::lazer_version::discover_from_root(&root).map_err(io::Error::other)?;
    if after != version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "client version changed during generation",
        ));
    }
    Ok(Bytes::from(bytes))
}

fn merge_game_base_vtable(generated: &[u8], signature: &[u8]) -> io::Result<Vec<u8>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Signature {
        osu_version: String,
        game_base_vtable: u64,
    }
    let signature: Signature = serde_json::from_slice(signature).map_err(io::Error::other)?;
    let mut value: serde_json::Value =
        serde_json::from_slice(generated).map_err(io::Error::other)?;
    if value.get("OsuVersion").and_then(serde_json::Value::as_str)
        != Some(signature.osu_version.as_str())
        || signature.game_base_vtable == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owned target must supply a nonzero GameBaseVtable for the generated OsuVersion",
        ));
    }
    value
        .as_object_mut()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "generated offsets root must be an object",
            )
        })?
        .insert(
            "GameBaseVtable".to_owned(),
            signature.game_base_vtable.into(),
        );
    serde_json::to_vec(&value).map_err(io::Error::other)
}

#[cfg(test)]
mod tests;
