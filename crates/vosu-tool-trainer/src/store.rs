use crate::{
    audio,
    map::{self, Options},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use vosu_tool_sdk::{BeatmapContext, Context as HostContext};

#[derive(Clone, Serialize, Deserialize)]
pub struct OwnedFile {
    pub path: PathBuf,
    pub hash: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub files: Vec<OwnedFile>,
    pub export: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}
#[derive(Default, Serialize, Deserialize)]
pub struct Index {
    pub maps: Vec<Entry>,
}
pub fn hash(bytes: &[u8]) -> String {
    hash_parts(&[bytes])
}
fn hash_parts(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for bytes in parts {
        digest.update(bytes);
    }
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest.finalize() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 15) as usize] as char);
    }
    out
}
pub fn audio_name(bytes: &[u8], options: &Options) -> String {
    format!(
        "vosu-trainer-{}-{}-{}.ogg",
        &hash(bytes)[..16],
        map::number(options.rate),
        if options.pitch_preserving {
            "stretch"
        } else {
            "resample"
        }
    )
}
pub fn load(data: &Path) -> Result<Index> {
    let p = data.join("index.json");
    if p.exists() {
        Ok(serde_json::from_slice(&fs::read(p)?)?)
    } else {
        Ok(Index::default())
    }
}
pub fn save(data: &Path, index: &Index) -> Result<()> {
    fs::create_dir_all(data)?;
    let tmp = data.join("index.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(index)?)?;
    fs::rename(tmp, data.join("index.json"))?;
    Ok(())
}
pub fn confined(root: &Path, path: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        !path.components().any(|c| matches!(c, Component::ParentDir)),
        "parent traversal refused"
    );
    anyhow::ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "symlink refused"
    );
    let p = path.canonicalize()?;
    let r = root.canonicalize()?;
    anyhow::ensure!(
        p != r && p.starts_with(&r) && p.is_file(),
        "file outside beatmap folder"
    );
    Ok(p)
}
pub fn delete(data: &Path, id: &str) -> Result<()> {
    let mut index = load(data)?;
    let entry = index
        .maps
        .iter()
        .find(|e| e.id == id)
        .context("map not in index")?
        .clone();
    // Validate the complete removal set before touching any file. Modified files
    // belong to the user now; never remove them, even if their names still match.
    let shared: BTreeSet<_> = index
        .maps
        .iter()
        .filter(|e| e.id != id)
        .flat_map(|e| e.files.iter().map(|f| &f.path))
        .collect();
    let files: Vec<_> = entry
        .files
        .iter()
        .filter(|f| !shared.contains(&f.path))
        .collect();
    for f in &files {
        if f.path.exists() {
            confined(&entry.root, &f.path)?;
            anyhow::ensure!(
                hash(&fs::read(&f.path)?) == f.hash,
                "generated file changed; refusing deletion"
            );
        }
    }
    for f in files {
        if f.path.exists() {
            fs::remove_file(&f.path)?;
        }
    }
    index.maps.retain(|e| e.id != id);
    save(data, &index)
}
fn add_file(path: &Path, root: &Path) -> Result<OwnedFile> {
    confined(root, path)?;
    Ok(OwnedFile {
        path: path.to_owned(),
        hash: hash(&fs::read(path)?),
    })
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn asset_files(folder: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        anyhow::ensure!(!ty.is_symlink(), "symlink in export assets");
        if ty.is_dir() {
            asset_files(&entry.path(), out)?;
        } else if ty.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}
pub fn generate(
    data: &Path,
    ctx: &HostContext,
    options: &Options,
    cancel: &AtomicBool,
    progress: impl Fn(f64, &str),
) -> Result<Entry> {
    let b: &BeatmapContext = ctx.beatmap.as_ref().context("no selected beatmap")?;
    let text = fs::read_to_string(&b.file)?;
    let o = map::effective(options, &text)?;
    let source = b.audio.as_ref().context("selected map has no audio")?;
    let original = fs::read(source)?;
    let audio_name = audio_name(&original, &o);
    let location = serde_json::to_vec(&(&ctx.client, &b.folder, &b.file))?;
    let id = hash_parts(&[
        text.as_bytes(),
        serde_json::to_vec(&o)?.as_slice(),
        original.as_slice(),
        &location,
    ]);
    let mut index = load(data)?;
    if let Some(entry) = index.maps.iter().find(|e| e.id == id) {
        for f in &entry.files {
            confined(&entry.root, &f.path)?;
            anyhow::ensure!(
                hash(&fs::read(&f.path)?) == f.hash,
                "existing generated file changed"
            );
        }
        return Ok(entry.clone());
    }
    let lazer = ctx.client.as_deref() == Some("lazer");
    anyhow::ensure!(
        lazer || ctx.client.as_deref() == Some("stable"),
        "unsupported client"
    );
    let root = if lazer {
        let p = data.join("exports");
        fs::create_dir_all(&p)?;
        p.canonicalize()?
    } else {
        b.folder.canonicalize()?
    };
    if !lazer {
        let songs = ctx
            .songs_dir
            .as_ref()
            .context("missing songs directory")?
            .canonicalize()?;
        anyhow::ensure!(root.starts_with(songs), "beatmap folder outside Songs");
        confined(&root, &b.file)?;
    }
    let working = if lazer {
        let p = data.join(format!("work-{}", &id[..16]));
        fs::create_dir(&p)?;
        p
    } else {
        root.clone()
    };
    let audio_path = if lazer {
        root.join(&audio_name)
    } else {
        working.join(&audio_name)
    };
    let osu_path = working.join(format!("vosu-trainer-{}.osu", &id[..24]));
    let mut newly_created = Vec::new();
    let result = (|| {
        let mut warnings = Vec::new();
        progress(0.05, "Decoding audio");
        if !audio_path.exists() {
            let decoded = audio::decode(source, cancel)?;
            progress(0.2, "Processing audio");
            let changed = audio::change(&decoded, o.rate, o.pitch_preserving, cancel, |v| {
                progress(0.2 + v * 0.55, "Processing audio")
            })?;
            audio::encode(&changed, &audio_path, cancel)?;
            newly_created.push(audio_path.clone());
        } else {
            anyhow::ensure!(
                index
                    .maps
                    .iter()
                    .flat_map(|e| &e.files)
                    .any(|f| f.path == audio_path
                        && fs::read(&audio_path).is_ok_and(|v| hash(&v) == f.hash)),
                "audio path already exists and is not owned"
            );
        }
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
        let mut output = map::transform(&text, &o, &audio_name)?;
        // Lazer may expose the background as a hash-named file; give the archive
        // its referenced asset name rather than writing into the managed store.
        if lazer
            && let Some(bg) = &b.background
            && bg.is_file()
        {
            let name = format!(
                "vosu-background.{}",
                bg.extension().and_then(|s| s.to_str()).unwrap_or("jpg")
            );
            let mut lines = Vec::new();
            let mut events = false;
            for line in output.lines() {
                if line.starts_with('[') {
                    events = line == "[Events]";
                }
                let mut p = map::csv(line);
                if events && matches!(p[0].trim(), "0" | "Background") && p.len() > 2 {
                    p[2] = format!("\"{name}\"");
                    lines.push(p.join(","));
                } else {
                    lines.push(line.to_owned());
                }
            }
            output = lines.join("\n") + "\n";
            fs::copy(bg, working.join(name))?;
        }
        write_new(&osu_path, output.as_bytes())?;
        newly_created.push(osu_path.clone());
        let name = map::value(&output, "[Metadata]", "Version").unwrap_or_default();
        let files = if lazer {
            progress(0.9, "Packaging lazer import");
            let archive = root.join(format!("vosu-trainer-{}.osz", &id[..24]));
            let mut z = zip::ZipWriter::new(
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&archive)?,
            );
            newly_created.push(archive.clone());
            let mut assets = Vec::new();
            asset_files(&working, &mut assets)?;
            // Preserve custom hitsounds, videos, and external storyboards for
            // exported-folder contexts. Hashed lazer contexts supply audio/bg.
            let mut names = BTreeSet::new();
            names.insert(audio_name.clone());
            z.start_file(&audio_name, zip::write::SimpleFileOptions::default())?;
            z.write_all(&fs::read(&audio_path)?)?;
            for p in assets {
                let rel = p
                    .strip_prefix(&working)?
                    .to_string_lossy()
                    .replace('\\', "/");
                names.insert(rel.clone());
                z.start_file(rel, zip::write::SimpleFileOptions::default())?;
                z.write_all(&fs::read(p)?)?;
            }
            let mut required = map::referenced_assets(&output);
            // Only visit named assets: lazer's parent folder may contain many
            // unrelated hashes, and must never be archived wholesale.
            for file in fs::read_dir(&b.folder)? {
                let file = file?;
                if file.path().extension().is_some_and(|ext| ext == "osb") {
                    let storyboard = match confined(&b.folder, &file.path())
                        .and_then(|p| Ok(fs::read_to_string(p)?))
                        .and_then(|s| map::storyboard(&s, o.rate))
                    {
                        Ok(s) => s,
                        Err(e) => {
                            warnings.push(format!(
                                "Omitted storyboard {}: {e}",
                                file.file_name().to_string_lossy()
                            ));
                            continue;
                        }
                    };
                    required.extend(map::referenced_assets(&storyboard));
                    let name = file.file_name().to_string_lossy().into_owned();
                    if names.insert(name.clone()) {
                        z.start_file(name, zip::write::SimpleFileOptions::default())?;
                        z.write_all(storyboard.as_bytes())?;
                    }
                } else {
                    let name = file.file_name().to_string_lossy().into_owned();
                    let lower = name.to_ascii_lowercase();
                    if [
                        "normal-",
                        "soft-",
                        "drum-",
                        "taiko-normal-",
                        "taiko-soft-",
                        "taiko-drum-",
                    ]
                    .iter()
                    .any(|p| lower.starts_with(p))
                        && [".wav", ".ogg", ".mp3"]
                            .iter()
                            .any(|ext| lower.ends_with(ext))
                    {
                        required.push(name);
                    }
                }
            }
            for rel in required {
                if names.contains(&rel) {
                    continue;
                }
                let relative = Path::new(&rel);
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
                {
                    warnings.push(format!("Omitted unsafe asset: {rel}"));
                    continue;
                }
                let Ok(p) = confined(&b.folder, &b.folder.join(relative)) else {
                    warnings.push(format!("Omitted unavailable asset: {rel}"));
                    continue;
                };
                if names.insert(rel.clone()) {
                    z.start_file(rel, zip::write::SimpleFileOptions::default())?;
                    z.write_all(&fs::read(p)?)?;
                }
            }
            z.finish()?;
            vec![add_file(&archive, &root)?, add_file(&audio_path, &root)?]
        } else {
            vec![add_file(&osu_path, &root)?, add_file(&audio_path, &root)?]
        };
        let entry = Entry {
            id: id.clone(),
            name,
            root: root.clone(),
            files,
            export: lazer,
            warnings,
        };
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
        index.maps.push(entry.clone());
        save(data, &index)?;
        Ok(entry)
    })();
    if lazer {
        let _ = fs::remove_dir_all(&working);
    }
    if result.is_err() {
        for p in newly_created {
            let _ = fs::remove_file(p);
        }
    }
    result
}
