//! Provisioning and assembly-only generation are confined to explicit `-g`.
use crate::net::{Deadlines, DownloadLimits, StreamingClient, ZipLimits, extract_zip_atomic};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};
use tokio_util::sync::CancellationToken;

const BUNDLE_TAG: &str = "offset-generator-v1";
const GENERATOR_RUNTIME: &str = "net8.0-8.0.29";
const RELEASE_ROOT: &str = "https://github.com/shiruviaa/vosu/releases/download";
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

struct Client {
    pid: u32,
    version: String,
    root: PathBuf,
}

fn current_client() -> io::Result<Client> {
    use crate::instance::{NativeProcessProvider, ProcessProvider};
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    let provider = NativeProcessProvider::polling_only();
    #[cfg(not(all(target_os = "linux", feature = "linux-process-events")))]
    let provider = NativeProcessProvider;
    let mut clients = Vec::new();
    for process in provider.enumerate().map_err(io::Error::other)? {
        if process.bitness != crate::process::ProcessBitness::X64 {
            continue;
        }
        let Ok(root) = crate::memory::lazer_version::process_executable_root(process.pid) else {
            continue;
        };
        if !root.join("osu!.deps.json").is_file() {
            continue;
        }
        let version =
            crate::memory::lazer_version::discover_from_root(&root).map_err(io::Error::other)?;
        clients.push(Client {
            pid: process.pid,
            version,
            root,
        });
    }
    select_client(clients)
}

fn select_client(mut clients: Vec<Client>) -> io::Result<Client> {
    match clients.len() {
        0 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no running native 64-bit osu!lazer client was found; start one unmodified client as the same user and retry vosu -g",
        )),
        1 => Ok(clients.remove(0)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "multiple running osu!lazer clients found (PIDs: {}); close all but one",
                clients
                    .iter()
                    .map(|client| client.pid.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

fn assembly_directory(client_root: &Path) -> io::Result<PathBuf> {
    [client_root.to_path_buf(), client_root.join("usr/bin"), client_root.join("bin")]
        .into_iter()
        .find(|path| path.join("osu!.dll").is_file() || path.join("osu.Game.dll").is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!(
            "could not locate osu!lazer managed assemblies under {}; wait for the client to finish starting and retry vosu -g",
            client_root.display()
        )))
}

fn cache_root() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Ok(crate::platform::Paths::system()?
            .cache()
            .join("offset-generator"))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(|root| PathBuf::from(root).join("vosu/cache/offset-generator"))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "LOCALAPPDATA is unavailable; use a normal Windows user session",
                )
            })
    }
}

fn file_hash(path: &Path, limit: u64) -> io::Result<String> {
    hash_reader(File::open(path)?, limit)
}

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
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn checksum(bytes: &[u8]) -> io::Result<String> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let digest = text.split_whitespace().next().unwrap_or_default();
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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

fn bundle_manifest(file: &mut File, expected_hash: &str) -> io::Result<Manifest> {
    file.seek(SeekFrom::Start(0))?;
    if hash_reader(&mut *file, MAX_ARCHIVE)? != expected_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator archive SHA-256 mismatch",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut archive = zip::ZipArchive::new(file)?;
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
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    validate_manifest(&manifest)?;
    if archive.len() != manifest.files.len() + 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generator archive contains unmanifested files",
        ));
    }
    Ok(manifest)
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
            || !checksum(digest.as_bytes()).is_ok_and(|hash| hash == *digest)
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

fn verify_extracted(root: &Path, manifest: &Manifest) -> io::Result<()> {
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
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative != "bundle.json" && !manifest.files.contains_key(&relative) {
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
        if !metadata.is_file() || file_hash(&path, MAX_EXPANDED)? != *expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("corrupt generator payload {}", path.display()),
            ));
        }
    }
    Ok(())
}

async fn obtain_bundle(
    cache: &Path,
    source: Option<&Path>,
    network: &StreamingClient,
) -> io::Result<PathBuf> {
    fs::create_dir_all(cache)?;
    let archive_name = format!("{BUNDLE_TAG}-{RID}.zip");
    let installed = cache.join("payload");
    let archive_path = cache.join(&archive_name);
    let checksum_path = cache.join(format!("{archive_name}.sha256"));
    if !archive_path.exists() {
        let temporary = tempfile::NamedTempFile::new_in(cache)?.into_temp_path();
        let expected = if let Some(directory) = source {
            println!(
                "Using configured local generator bundle {}",
                directory.display()
            );
            let expected = checksum(&fs::read(directory.join(format!("{archive_name}.sha256")))?)?;
            fs::copy(directory.join(&archive_name), &temporary)?;
            expected
        } else {
            let url = format!("{RELEASE_ROOT}/{BUNDLE_TAG}/{archive_name}");
            println!("Downloading generator bundle {url}");
            let response = network
                .clone()
                .with_deadlines_and_limits(
                    Deadlines::default(),
                    DownloadLimits {
                        max_bytes: 4096,
                        max_redirects: 5,
                    },
                )
                .request_bytes(
                    &format!("{url}.sha256"),
                    "vosu-offset-generator",
                    &CancellationToken::new(),
                )
                .await
                .map_err(download_error)?;
            if response.status == 404 {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "matching generator bundle has not been published: {url}; install a release with published offset-generator assets"
                    ),
                ));
            }
            if response.status != 200 {
                return Err(io::Error::other(format!(
                    "generator checksum download failed: HTTP {} from {url}.sha256",
                    response.status
                )));
            }
            let expected = checksum(&response.body)?;
            network
                .download_atomic(
                    &url,
                    "vosu-offset-generator",
                    &temporary,
                    &CancellationToken::new(),
                )
                .await
                .map_err(download_error)?;
            expected
        };
        bundle_manifest(&mut File::open(&temporary)?, &expected)?;
        // Publish checksum before archive: an interruption cannot expose an archive
        // with no checksum. Downloads and extraction are never executable in-place.
        crate::memory::lazer_loader::install_cache(
            &checksum_path,
            expected.as_bytes(),
            &CancellationToken::new(),
        )?;
        crate::platform::atomic_replace(&temporary, &archive_path)?;
    } else {
        println!("Reusing verified generator cache {}", cache.display());
    }
    let result = (|| {
        let expected = checksum(&fs::read(&checksum_path)?)?;
        let manifest = bundle_manifest(&mut File::open(&archive_path)?, &expected)?;
        if !installed.exists() {
            let staging = tempfile::Builder::new()
                .prefix(".verify-")
                .tempdir_in(cache)?;
            let payload = staging.path().join("payload");
            extract_zip_atomic(
                &archive_path,
                &payload,
                ZipLimits {
                    max_entries: 2048,
                    max_expanded_bytes: MAX_EXPANDED,
                },
                &CancellationToken::new(),
            )
            .map_err(io::Error::other)?;
            verify_extracted(&payload, &manifest)?;
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(payload.join(GENERATOR), fs::Permissions::from_mode(0o700))?;
                fs::set_permissions(payload.join(TARGET_HOST), fs::Permissions::from_mode(0o700))?;
            }
            // Competing writers may install the same verified immutable bundle.
            match fs::rename(&payload, &installed) {
                Ok(()) => {}
                Err(_) if installed.is_dir() => {}
                Err(error) => return Err(error),
            }
        }
        verify_extracted(&installed, &manifest)?;
        Ok(installed)
    })();
    result.map_err(|error: io::Error| io::Error::new(error.kind(), format!("generator cache validation failed at {}: {error}; remove this cache directory and retry vosu -g", cache.display())))
}

fn download_error(error: crate::net::NetworkError) -> io::Error {
    io::Error::other(format!(
        "generator bundle download failed: {error}; check HTTPS connectivity and retry vosu -g"
    ))
}

fn generator_command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    for (name, _) in std::env::vars_os() {
        let name_upper = name.to_string_lossy().to_ascii_uppercase();
        if name_upper.starts_with("DOTNET_")
            || name_upper.starts_with("COMPLUS_")
            || name_upper.starts_with("CORECLR_")
        {
            command.env_remove(name);
        }
    }
    command
}

fn invoke(
    generator: &Path,
    assembly_dir: &Path,
    template: &Path,
    output: &Path,
) -> io::Result<Output> {
    generator_command(generator)
        .arg("--assemblies-only")
        .arg("--assembly-dir")
        .arg(assembly_dir)
        .arg("--template")
        .arg(template)
        .arg("--output")
        .arg(output)
        .output()
}

fn merge_game_base_vtable(generated: &[u8], signature: &[u8]) -> io::Result<Vec<u8>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Signature {
        osu_version: String,
        game_base_vtable: u64,
    }
    let signature: Signature = serde_json::from_slice(signature)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut value: serde_json::Value = serde_json::from_slice(generated)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if value.get("OsuVersion").and_then(serde_json::Value::as_str)
        != Some(signature.osu_version.as_str())
        || signature.game_base_vtable == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owned target must supply a nonzero GameBaseVtable for the generated OsuVersion",
        ));
    }
    // This value comes only from this build's owned child, never the template.
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
    serde_json::to_vec(&value).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(super) async fn generate() -> io::Result<PathBuf> {
    let client = current_client()?;
    let assembly_dir = assembly_directory(&client.root)?;
    let cache = cache_root()?
        .join(RID)
        .join(GENERATOR_RUNTIME)
        .join(BUNDLE_TAG);
    let local_source = std::env::var_os("VOSU_OFFSET_BUNDLE_SOURCE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let network = StreamingClient::production().with_deadlines_and_limits(
        Deadlines::default(),
        DownloadLimits {
            max_bytes: MAX_ARCHIVE,
            max_redirects: 5,
        },
    );
    let payload = obtain_bundle(&cache, local_source.as_deref(), &network).await?;
    let generator = payload.join(GENERATOR);
    let work = tempfile::Builder::new()
        .prefix(".generation-")
        .tempdir_in(&cache)?;
    let template = work.path().join("template.json");
    let output = work.path().join("offsets.json");
    let asset = crate::assets::DaemonAssets::get("offsets.json")
        .ok_or_else(|| io::Error::other("embedded offsets template is missing"))?;
    fs::write(&template, asset.data.as_ref())?;
    println!(
        "Generating from osu!lazer {} PID {} assemblies ({RID})",
        client.version, client.pid
    );
    let result = invoke(&generator, &assembly_dir, &template, &output)?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "assembly-only offset generator failed ({}); the client runtime or assembly layout may be unsupported: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    let signature = generator_command(&payload.join(TARGET_HOST))
        .arg("--game-base-signature")
        .arg("--assembly-dir")
        .arg(&assembly_dir)
        .output()?;
    if !signature.status.success() {
        return Err(io::Error::other(format!(
            "owned target could not derive GameBaseVtable ({}); the client runtime or assembly layout may be unsupported: {}",
            signature.status,
            String::from_utf8_lossy(&signature.stderr).trim()
        )));
    }
    let generated = merge_game_base_vtable(&fs::read(output)?, &signature.stdout)?;
    super::cli::validate_generated_offsets(&generated, &client.version)?;
    let destination = crate::platform::Paths::system()?
        .cache()
        .join("offsets")
        .join(format!("{}.json", client.version));
    crate::memory::lazer_loader::install_cache(
        &destination,
        &generated,
        &CancellationToken::new(),
    )?;
    Ok(destination)
}

#[cfg(test)]
mod tests;
