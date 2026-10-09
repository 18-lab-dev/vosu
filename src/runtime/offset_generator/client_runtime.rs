use super::RID;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

const MAX_MANIFEST: u64 = 4 * 1024 * 1024;

fn read_manifest(path: &Path) -> io::Result<Value> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(invalid("runtime manifest exceeds 4 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn runtime_pack(deps: &Value) -> io::Result<(&str, &str, &Value)> {
    let target = deps["runtimeTarget"]["name"]
        .as_str()
        .ok_or_else(|| invalid("runtime dependencies have no target"))?;
    let entries = deps["targets"][target]
        .as_object()
        .ok_or_else(|| invalid("runtime dependencies have no target entries"))?;
    let prefix = format!("runtimepack.Microsoft.NETCore.App.Runtime.{RID}/");
    let mut packs = entries.iter().filter(|(name, _)| name.starts_with(&prefix));
    let (name, pack) = packs
        .next()
        .ok_or_else(|| invalid("client has no matching self-contained runtime pack"))?;
    if packs.next().is_some() {
        return Err(invalid(
            "runtime dependencies contain multiple runtime packs",
        ));
    }
    Ok((target, name, pack))
}

fn runtime_files(pack: &Value) -> io::Result<BTreeSet<&str>> {
    let mut files = BTreeSet::new();
    for kind in ["runtime", "native"] {
        let entries = pack[kind]
            .as_object()
            .ok_or_else(|| invalid("runtime pack is missing its managed or native files"))?;
        for name in entries.keys() {
            let mut parts = Path::new(name).components();
            if !matches!(parts.next(), Some(Component::Normal(_)))
                || parts.next().is_some()
                || name.contains(['\\', ':'])
            {
                return Err(invalid("runtime pack file must be a local filename"));
            }
            files.insert(name.as_str());
        }
    }
    if files.len() > 2048 {
        return Err(invalid("runtime pack exceeds file limit"));
    }
    Ok(files)
}

fn link_or_copy(source: &Path, destination: &Path) -> io::Result<()> {
    if fs::hard_link(source, destination).is_err() {
        fs::copy(source, destination)?;
    }
    Ok(())
}

/// Give each helper the exact client's CLR and framework, not the bundle's build
/// runtime. A newer game can reference framework APIs unavailable to the helper's
/// .NET SDK, and CLR forks can have different object layouts from stock .NET.
/// Everything lives in disposable generation work; neither installation changes.
pub(super) fn stage(payload: &Path, assembly: &Path, work: &Path) -> io::Result<PathBuf> {
    let client_deps = read_manifest(&assembly.join("osu!.deps.json"))?;
    let config = read_manifest(&assembly.join("osu!.runtimeconfig.json"))?;
    let (client_target, client_name, client_pack) = runtime_pack(&client_deps)?;
    let client_files = runtime_files(client_pack)?;
    #[cfg(target_os = "linux")]
    let hosts = ["libhostfxr.so", "libhostpolicy.so"];
    #[cfg(windows)]
    let hosts = ["hostfxr.dll", "hostpolicy.dll"];
    #[cfg(target_os = "linux")]
    let coreclr = "libcoreclr.so";
    #[cfg(windows)]
    let coreclr = "coreclr.dll";
    for name in [hosts[0], hosts[1], coreclr, "System.Private.CoreLib.dll"] {
        if !client_files.contains(name) || !assembly.join(name).is_file() {
            return Err(invalid("client self-contained runtime is incomplete"));
        }
    }
    let staged = work.join("client-runtime");
    fs::create_dir(&staged)?;
    for helper in ["generator", "target"] {
        let name = format!("lazer-offset-{helper}");
        let source = payload.join(helper);
        let destination = staged.join(helper);
        fs::create_dir(&destination)?;
        let deps_name = format!("{name}.deps.json");
        let config_name = format!("{name}.runtimeconfig.json");
        let mut deps = read_manifest(&source.join(&deps_name))?;
        let (old_target, old_name, old_pack) = runtime_pack(&deps)?;
        let old_files = runtime_files(old_pack)?;
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            let filename = entry.file_name();
            let filename = filename
                .to_str()
                .ok_or_else(|| invalid("helper filename is not UTF-8"))?;
            if entry.file_type()?.is_file()
                && filename != deps_name
                && filename != config_name
                && !old_files.contains(filename)
                && !client_files.contains(filename)
            {
                // Apphosts canonicalize the managed entry point's path. A symlink
                // would silently bind it back to the original .NET 8 bundle.
                link_or_copy(&entry.path(), &destination.join(filename))?;
            }
        }
        let mut entries = deps["targets"][old_target]
            .as_object()
            .expect("runtime_pack validated target")
            .clone();
        entries.remove(old_name);
        entries.insert(client_name.to_owned(), client_pack.clone());
        let old_name = old_name.to_owned();
        deps["targets"] = serde_json::json!({client_target: entries});
        deps["runtimeTarget"] = client_deps["runtimeTarget"].clone();
        let libraries = deps["libraries"]
            .as_object_mut()
            .ok_or_else(|| invalid("helper dependencies have no library entries"))?;
        libraries.remove(&old_name);
        libraries.insert(
            client_name.to_owned(),
            client_deps["libraries"][client_name].clone(),
        );
        fs::write(destination.join(deps_name), serde_json::to_vec(&deps)?)?;
        fs::write(destination.join(config_name), serde_json::to_vec(&config)?)?;
        for filename in &client_files {
            let source = assembly.join(filename);
            // Official clients omit the optional crash-dump executable.
            if matches!(*filename, "createdump" | "createdump.exe") && !source.exists() {
                continue;
            }
            if !source.is_file() {
                return Err(invalid("client runtime pack file is missing"));
            }
            let destination = destination.join(filename);
            if hosts.contains(filename) {
                link_or_copy(&source, &destination)?;
            } else {
                #[cfg(target_os = "linux")]
                std::os::unix::fs::symlink(&source, &destination)?;
                #[cfg(windows)]
                link_or_copy(&source, &destination)?;
            }
        }
    }
    Ok(staged)
}

#[cfg(test)]
mod tests;
