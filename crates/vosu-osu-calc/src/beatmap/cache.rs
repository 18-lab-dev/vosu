use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use md5::{Digest, Md5};

use super::model::{
    BeatmapEngine, BeatmapError, BeatmapInput, CacheKey, ModSet, ParsedBeatmap, Ruleset,
    ScoringMode, parse_map,
};

pub struct BeatmapCache<E> {
    engine: E,
    current: Option<Arc<ParsedBeatmap>>,
    parse_count: u64,
}

impl<E: BeatmapEngine> BeatmapCache<E> {
    pub const fn new(engine: E) -> Self {
        Self {
            engine,
            current: None,
            parse_count: 0,
        }
    }
    pub fn current(&self) -> Option<&Arc<ParsedBeatmap>> {
        self.current.as_ref()
    }
    pub const fn parse_count(&self) -> u64 {
        self.parse_count
    }
    pub fn invalidate(&mut self) {
        self.current = None;
    }

    pub fn load(
        &mut self,
        input: BeatmapInput<'_>,
        requested_mode: Ruleset,
        mods: &ModSet,
        scoring_mode: ScoringMode,
    ) -> Result<Arc<ParsedBeatmap>, BeatmapError> {
        if let BeatmapInput::Content {
            content,
            identity_path: None,
        } = &input
            && let Some(current) = &self.current
        {
            let key = current.cache_key();
            if key.canonical_path.is_none()
                && key.input_path.is_none()
                && key.memory_identity.is_none()
                && key.requested_mode == requested_mode
                && key.mod_checksum == mods.checksum
                && key.clock_rate_bits == mods.clock_rate().to_bits()
                && key.scoring_mode == scoring_mode
                && current.source() == *content
            {
                return Ok(Arc::clone(current));
            }
        }
        if let BeatmapInput::PathWithIdentity {
            path,
            memory_identity,
        } = &input
            && let Some(current) = &self.current
        {
            let key = current.cache_key();
            if key.input_path.as_deref() == Some(*path)
                && key.memory_identity.as_deref() == Some(*memory_identity)
                && key.requested_mode == requested_mode
                && key.mod_checksum == mods.checksum
                && key.clock_rate_bits == mods.clock_rate().to_bits()
                && key.scoring_mode == scoring_mode
            {
                return Ok(Arc::clone(current));
            }
        }
        let (content, canonical_path, input_path, memory_identity) = read_input(input)?;
        let checksum = md5_hex(content.as_bytes());
        let key = CacheKey {
            canonical_path,
            input_path,
            memory_identity,
            source_checksum: checksum,
            requested_mode,
            mod_checksum: mods.checksum.clone(),
            scoring_mode,
            clock_rate_bits: mods.clock_rate().to_bits(),
        };
        if let Some(current) = &self.current
            && current.key == key
        {
            return Ok(Arc::clone(current));
        }
        let map = parse_map(
            key,
            content,
            requested_mode,
            mods,
            scoring_mode,
            &self.engine,
        )?;
        self.parse_count += 1;
        let map = Arc::new(map);
        self.current = Some(Arc::clone(&map));
        Ok(map)
    }
}

type ReadInput = (Arc<str>, Option<PathBuf>, Option<PathBuf>, Option<String>);

fn read_input(input: BeatmapInput<'_>) -> Result<ReadInput, BeatmapError> {
    match input {
        BeatmapInput::Path(path) => read_path(path, None),
        BeatmapInput::PathWithIdentity {
            path,
            memory_identity,
        } => read_path(path, Some(memory_identity)),
        BeatmapInput::Content {
            content,
            identity_path,
        } => {
            let canonical = identity_path
                .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()));
            Ok((Arc::from(content), canonical, None, None))
        }
    }
}

fn read_path(path: &Path, memory_identity: Option<&str>) -> Result<ReadInput, BeatmapError> {
    if !path.is_file() {
        return Err(BeatmapError::NotAFile(path.to_owned()));
    }
    let canonical =
        fs::canonicalize(path).map_err(|error| BeatmapError::Read(error.to_string()))?;
    let bytes = fs::read(&canonical).map_err(|error| BeatmapError::Read(error.to_string()))?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    let input_path = memory_identity.map(|_| path.to_owned());
    Ok((
        content.into(),
        Some(canonical),
        input_path,
        memory_identity.map(str::to_owned),
    ))
}
fn md5_hex(bytes: &[u8]) -> String {
    let mut digest = Md5::new();
    digest.update(bytes);
    let bytes = digest.finalize();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}
