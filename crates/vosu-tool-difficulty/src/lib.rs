#![forbid(unsafe_op_in_unsafe_fn)]

use md5::{Digest, Md5};
use rosu_map::{
    Beatmap,
    section::{general::GameMode, hit_objects::HitObjectKind},
};
use serde::{Deserialize, Serialize};

pub const CALCULATOR: &str = "minacalc-527@2d706147-heads-v1";
pub const DAN_MODEL: &str = "reform-v1-course-msd527@1";
pub const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct Row {
    pub notes: u32,
    pub seconds: f32,
}
unsafe extern "C" {
    fn vosu_minacalc(
        rows: *const Row,
        count: usize,
        keys: u32,
        rate: f32,
        goal: f32,
        ssr: bool,
        out: *mut f32,
    ) -> i32;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skills {
    pub overall: f32,
    pub stream: f32,
    pub jumpstream: f32,
    pub handstream: f32,
    pub stamina: f32,
    pub jackspeed: f32,
    pub chordjack: f32,
    pub technical: f32,
}
impl From<[f32; 8]> for Skills {
    fn from(v: [f32; 8]) -> Self {
        Self {
            overall: v[0],
            stream: v[1],
            jumpstream: v[2],
            handstream: v[3],
            stamina: v[4],
            jackspeed: v[5],
            chordjack: v[6],
            technical: v[7],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dan {
    pub system: String,
    pub label: String,
    pub value: f32,
    pub experimental: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Calculator {
    pub msd: String,
    pub dan: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Difficulty {
    pub checksum: String,
    pub rate: f32,
    pub mode: String,
    pub keys: u32,
    pub notes: usize,
    pub msd: Option<Skills>,
    pub score: Option<f32>,
    pub ssr: Option<Skills>,
    pub dan: Option<Dan>,
    pub calculator: Calculator,
    pub unsupported: Option<String>,
}
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub rate_tick: u8,
    pub score: Option<f32>,
}
impl Params {
    pub fn new(rate: f32, score: Option<f32>) -> Result<Self, String> {
        if !rate.is_finite() || !(0.5..=2.0).contains(&rate) {
            return Err("rate must be finite and between 0.5 and 2.0".into());
        }
        if score.is_some_and(|s| !s.is_finite() || !(0.0..=1.0).contains(&s)) {
            return Err("score must be finite and between 0 and 1".into());
        }
        Ok(Self {
            rate_tick: (rate * 20.0).round() as u8,
            score: score.map(|s| if s == 0.0 { 0.0 } else { s }),
        })
    }
    pub fn rate(self) -> f32 {
        f32::from(self.rate_tick) / 20.0
    }
    pub fn key(self, checksum: &str) -> String {
        format!(
            "{checksum}:{}:{:?}:{CALCULATOR}:{DAN_MODEL}",
            self.rate_tick,
            self.score.map(f32::to_bits)
        )
    }
}
pub fn checksum(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(32);
    for b in Md5::digest(bytes) {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

pub fn rows(map: &Beatmap, keys: u32) -> Result<Vec<Row>, String> {
    if !matches!(keys, 4 | 6 | 7) {
        return Err("unsupported key count".into());
    }
    let mut rows: Vec<Row> = Vec::with_capacity(map.hit_objects.len());
    for note in &map.hit_objects {
        let x = match &note.kind {
            HitObjectKind::Circle(c) => c.pos.x,
            HitObjectKind::Hold(h) => h.pos_x,
            _ => return Err("mania sliders and spinners are not supported".into()),
        };
        let seconds = (note.start_time / 1000.0) as f32;
        if !x.is_finite()
            || !(0.0..=512.0).contains(&x)
            || !seconds.is_finite()
            || !(0.0..=24000.0).contains(&seconds)
        {
            return Err(
                "invalid note position or time (supported duration: 0–24000 seconds)".into(),
            );
        }
        let col = ((x * keys as f32 / 512.0).floor() as u32).min(keys - 1);
        rows.push(Row {
            notes: 1 << col,
            seconds,
        });
    }
    rows.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
    let mut merged: Vec<Row> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(last) = merged.last_mut().filter(|last| last.seconds == row.seconds) {
            last.notes |= row.notes;
        } else {
            merged.push(row);
        }
    }
    Ok(merged)
}
pub fn native(rows: &[Row], keys: u32, rate: f32, score: Option<f32>) -> Result<Skills, String> {
    Params::new(rate, score)?;
    if !matches!(keys, 4 | 6 | 7)
        || rows.iter().any(|r| {
            r.notes == 0
                || r.notes >= (1 << keys)
                || !r.seconds.is_finite()
                || !(0.0..=24000.0).contains(&r.seconds)
        })
        || rows.windows(2).any(|w| w[0].seconds >= w[1].seconds)
    {
        return Err("invalid MinaCalc rows or key count".into());
    }
    let mut out = [0.0; 8];
    // Row is repr(C), matches NoteInfo's uint32 + float; buffers live through
    // this synchronous call. The shim catches all C++ exceptions.
    let status = unsafe {
        vosu_minacalc(
            rows.as_ptr(),
            rows.len(),
            keys,
            rate,
            score.unwrap_or(0.93),
            score.is_some(),
            out.as_mut_ptr(),
        )
    };
    if status != 0 {
        return Err(format!("MinaCalc failed ({status})"));
    }
    Ok(out.into())
}
pub fn calculate(bytes: &[u8], params: Params) -> Result<Difficulty, String> {
    if bytes.len() > MAX_BYTES {
        return Err("beatmap exceeds 1 MiB".into());
    }
    if !bytes.starts_with(b"osu file format v")
        && !bytes.starts_with(b"\xef\xbb\xbfosu file format v")
    {
        return Err("expected an osu beatmap".into());
    }
    let map = Beatmap::from_bytes(bytes).map_err(|e| e.to_string())?;
    let mode = match map.mode {
        GameMode::Osu => "osu",
        GameMode::Taiko => "taiko",
        GameMode::Catch => "fruits",
        GameMode::Mania => "mania",
    };
    let keys = map.circle_size as u32;
    let mut result = Difficulty {
        checksum: checksum(bytes),
        rate: params.rate(),
        mode: mode.into(),
        keys,
        notes: map.hit_objects.len(),
        msd: None,
        score: params.score,
        ssr: None,
        dan: None,
        calculator: Calculator {
            msd: CALCULATOR.into(),
            dan: DAN_MODEL.into(),
        },
        unsupported: None,
    };
    if map.mode != GameMode::Mania {
        result.unsupported = Some("only native osu!mania beatmaps are supported".into());
        return Ok(result);
    }
    if !matches!(keys, 4 | 6 | 7) || keys as f32 != map.circle_size {
        result.unsupported = Some("MinaCalc provider supports 4K, 6K and 7K".into());
        return Ok(result);
    }
    let rows = rows(&map, keys)?;
    let msd = native(&rows, keys, params.rate(), None)?;
    if keys == 4 {
        result.dan = estimate_dan(msd.overall);
    }
    result.ssr = params
        .score
        .map(|s| native(&rows, keys, params.rate(), Some(s)))
        .transpose()?;
    result.msd = Some(msd);
    Ok(result)
}

pub fn estimate_dan(overall: f32) -> Option<Dan> {
    static ANCHORS: std::sync::LazyLock<[f32; 10]> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../calibration.json")).expect("embedded calibration")
    });
    let anchors = &*ANCHORS;
    if !overall.is_finite() || overall < anchors[0] || overall > anchors[9] {
        return None;
    }
    let index = anchors
        .windows(2)
        .position(|w| overall <= w[1])
        .unwrap_or(8);
    let value =
        1.0 + index as f32 + (overall - anchors[index]) / (anchors[index + 1] - anchors[index]);
    Some(Dan {
        system: "reform".into(),
        label: format!("Reform {}", value.floor() as u32),
        value,
        experimental: true,
    })
}
