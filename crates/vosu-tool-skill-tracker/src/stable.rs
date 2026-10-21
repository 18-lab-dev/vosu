//! Read-only legacy database parser. Format source: ppy/osu wiki,
//! Legacy-database-file-structure (including 20250107 float star pairs).
use crate::detect::Play;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("offset overflow")?;
        let b = self.data.get(self.at..end).ok_or("truncated database")?;
        self.at = end;
        Ok(b)
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn count(&mut self) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n > 1_000_000 {
            Err("database collection exceeds safety limit".into())
        } else {
            Ok(n)
        }
    }
    fn string(&mut self) -> Result<String, String> {
        match self.byte()? {
            0 => Ok(String::new()),
            11 => {
                let mut len = 0_usize;
                for shift in (0..35).step_by(7) {
                    let b = self.byte()?;
                    len |= ((b & 127) as usize) << shift;
                    if b & 128 == 0 {
                        if len > 1_048_576 {
                            return Err("database string too long".into());
                        }
                        return String::from_utf8(self.take(len)?.to_vec())
                            .map_err(|e| e.to_string());
                    }
                }
                Err("invalid ULEB128".into())
            }
            _ => Err("invalid string marker".into()),
        }
    }
}
#[derive(Debug)]
pub struct Map {
    pub folder: String,
    pub file: String,
    pub title: String,
    pub mode: u8,
    pub keys: u32,
}

pub fn maps(data: &[u8]) -> Result<HashMap<String, Map>, String> {
    let mut r = Reader::new(data);
    let version = r.u32()?;
    r.skip(4 + 1 + 8)?;
    r.string()?;
    let n = r.count()?;
    let mut maps = HashMap::new();
    for _ in 0..n {
        if version < 20191106 {
            r.skip(4)?;
        }
        r.string()?;
        r.string()?;
        let title = r.string()?;
        r.string()?;
        r.string()?;
        let diff = r.string()?;
        r.string()?;
        let hash = r.string()?;
        let file = r.string()?;
        r.skip(1 + 6 + 8)?;
        let keys = if version < 20140609 {
            r.skip(1)?;
            let cs = r.byte()?;
            r.skip(2)?;
            u32::from(cs)
        } else {
            r.skip(4)?;
            let cs = r.f32()?;
            r.skip(8)?;
            cs as u32
        };
        r.skip(8)?;
        if version >= 20140609 {
            for _ in 0..4 {
                let pairs = r.count()?;
                r.skip(pairs * if version >= 20250107 { 10 } else { 14 })?;
            }
        }
        r.skip(12)?;
        let timing = r.count()?;
        r.skip(timing * 17)?;
        r.skip(12 + 4 + 2 + 4)?;
        let mode = r.byte()?;
        r.string()?;
        r.string()?;
        r.skip(2)?;
        r.string()?;
        r.skip(1 + 8 + 1)?;
        let folder = r.string()?;
        r.skip(8 + 5)?;
        if version < 20140609 {
            r.skip(2)?;
        }
        r.skip(4 + 1)?;
        maps.insert(
            hash,
            Map {
                folder,
                file,
                title: format!("{title} [{diff}]"),
                mode,
                keys,
            },
        );
    }
    r.skip(4)?;
    if r.at != data.len() {
        return Err("unrecognized osu!.db trailing data".into());
    }
    Ok(maps)
}

pub fn scores(data: &[u8]) -> Result<Vec<Play>, String> {
    let mut r = Reader::new(data);
    let _version = r.u32()?;
    let n = r.count()?;
    let mut plays = Vec::new();
    for _ in 0..n {
        let group = r.string()?;
        let count = r.count()?;
        for _ in 0..count {
            if plays.len() >= 1_000_000 {
                return Err("too many imported scores".into());
            }
            let mode = r.byte()?;
            r.skip(4)?;
            let hash = r.string()?;
            r.string()?;
            r.string()?;
            let hits = json!({"300":r.u16()?,"100":r.u16()?,"50":r.u16()?,"geki":r.u16()?,"katu":r.u16()?,"0":r.u16()?});
            let score = r.u32()?;
            let combo = r.u16()?;
            r.skip(1)?;
            let mods = r.u32()?;
            r.string()?;
            let ticks = r.u64()?;
            r.skip(4 + 8)?;
            if mods & (1 << 23) != 0 {
                r.skip(8)?;
            }
            let seconds =
                i64::try_from(ticks / 10_000_000).map_err(|e| e.to_string())? - 62_135_596_800;
            let timestamp =
                DateTime::<Utc>::from_timestamp(seconds, ((ticks % 10_000_000) * 100) as u32)
                    .ok_or("invalid score timestamp")?
                    .to_rfc3339();
            let name = match mode {
                0 => "osu",
                1 => "taiko",
                2 => "fruits",
                3 => "mania",
                _ => return Err("invalid score mode".into()),
            };
            let n = crate::rating::total(&hits) as f64;
            let h = |k| crate::rating::count(&hits, k) as f64;
            let accuracy = if n > 0.0 && mode == 3 {
                100.0
                    * (300.0 * (h("geki") + h("300"))
                        + 200.0 * h("katu")
                        + 100.0 * h("100")
                        + 50.0 * h("50"))
                    / (300.0 * n)
            } else {
                0.0
            };
            let frame = vec![
                Value::Null,
                json!("stable"),
                json!({"checksum":if hash.is_empty(){&group}else{&hash},"mode":{"name":name}}),
                json!({"mode":{"name":name},"score":score,"hits":hits,"accuracy":accuracy,"combo":{"max":combo},"mods":{"number":mods}}),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ];
            let mut p = Play::from_frame(&frame, "stable-import");
            p.timestamp = timestamp;
            p.outcome = "passed".into();
            p.pp = None;
            p.stars = None;
            plays.push(p);
        }
    }
    if r.at != data.len() {
        return Err("unrecognized scores.db trailing data".into());
    }
    Ok(plays)
}

pub fn map_path(songs: &Path, map: &Map) -> Result<PathBuf, String> {
    for s in [&map.folder, &map.file] {
        if s.contains('\\')
            || s.contains(':')
            || Path::new(s)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("unsafe beatmap path in database".into());
        }
    }
    let root = songs.canonicalize().map_err(|e| e.to_string())?;
    let path = root
        .join(&map.folder)
        .join(&map.file)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(&root) {
        return Err("beatmap symlink leaves songs directory".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn string(b: &mut Vec<u8>, s: &str) {
        b.push(11);
        b.push(s.len() as u8);
        b.extend(s.as_bytes());
    }
    fn u32(b: &mut Vec<u8>, v: u32) {
        b.extend(v.to_le_bytes());
    }
    #[test]
    fn score_fixture() {
        let mut b = Vec::new();
        u32(&mut b, 20250107);
        u32(&mut b, 1);
        string(&mut b, "hash");
        u32(&mut b, 1);
        b.push(3);
        u32(&mut b, 20250107);
        for s in ["hash", "player", "replay"] {
            string(&mut b, s);
        }
        for n in [2_u16, 0, 0, 8, 0, 0] {
            b.extend(n.to_le_bytes());
        }
        u32(&mut b, 1000000);
        b.extend(10_u16.to_le_bytes());
        b.push(1);
        u32(&mut b, 64);
        string(&mut b, "");
        b.extend(638000000000000000_u64.to_le_bytes());
        u32(&mut b, u32::MAX);
        b.extend(42_u64.to_le_bytes());
        let p = scores(&b).unwrap();
        assert_eq!(p[0].rate, 1.5);
        assert_eq!(p[0].accuracy, 100.0);
        assert_eq!(p[0].checksum, "hash");
        assert!(scores(&b[..b.len() - 1]).is_err());
    }
    #[test]
    fn map_fixtures() {
        for version in [20100101, 20150101, 20200101, 20250107] {
            let mut b = Vec::new();
            u32(&mut b, version);
            b.extend([0; 13]);
            string(&mut b, "player");
            u32(&mut b, 1);
            if version < 20191106 {
                u32(&mut b, 0);
            }
            for s in [
                "artist", "", "title", "", "mapper", "diff", "audio", "hash", "map.osu",
            ] {
                string(&mut b, s);
            }
            b.extend([0; 15]);
            if version < 20140609 {
                b.extend([8, 4, 8, 8]);
            } else {
                for n in [8_f32, 4.0, 8.0, 8.0] {
                    b.extend(n.to_le_bytes());
                }
            }
            b.extend([0; 8]);
            if version >= 20140609 {
                for _ in 0..4 {
                    u32(&mut b, 1);
                    b.extend(vec![0; if version >= 20250107 { 10 } else { 14 }]);
                }
            }
            b.extend([0; 12]);
            u32(&mut b, 1);
            b.extend([0; 17]);
            b.extend([0; 22]);
            b.push(3);
            string(&mut b, "");
            string(&mut b, "");
            b.extend([0; 2]);
            string(&mut b, "");
            b.extend([0; 10]);
            string(&mut b, "folder");
            b.extend([0; 13]);
            if version < 20140609 {
                b.extend([0; 2]);
            }
            b.extend([0; 5]);
            u32(&mut b, 0);
            let m = maps(&b).unwrap();
            assert_eq!(m["hash"].keys, 4);
            assert_eq!(m["hash"].folder, "folder");
            assert_eq!(m["hash"].mode, 3);
            assert!(maps(&b[..b.len() - 1]).is_err());
        }
    }
}
