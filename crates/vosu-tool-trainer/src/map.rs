use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub rate: f64,
    pub target_bpm: Option<f64>,
    pub pitch_preserving: bool,
    pub hp: Option<f64>,
    pub cs: Option<f64>,
    pub ar: Option<f64>,
    pub od: Option<f64>,
    pub lock_ar: bool,
    pub lock_od: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            rate: 1.0,
            target_bpm: None,
            pitch_preserving: true,
            hp: None,
            cs: None,
            ar: None,
            od: None,
            lock_ar: false,
            lock_od: false,
        }
    }
}
pub fn number(v: f64) -> String {
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}
pub fn ar_ms(ar: f64) -> f64 {
    if ar < 5.0 {
        1800.0 - 120.0 * ar
    } else {
        1200.0 - 150.0 * (ar - 5.0)
    }
}
pub fn ms_ar(ms: f64) -> f64 {
    if ms > 1200.0 {
        (1800.0 - ms) / 120.0
    } else {
        5.0 + (1200.0 - ms) / 150.0
    }
}
pub fn rate_from_bpm(bpm: f64, target: f64) -> Result<f64> {
    anyhow::ensure!(bpm > 0.0 && target.is_finite(), "invalid BPM");
    let r = target / bpm;
    anyhow::ensure!((0.5..=2.0).contains(&r), "rate must be 0.5–2.0");
    Ok(r)
}
pub fn value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = "";
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.starts_with('[') {
            current = line;
        }
        if current == section
            && let Some((k, v)) = line.split_once(':')
            && k.trim() == key
        {
            return Some(v.trim().to_owned());
        }
    }
    None
}
pub fn bpm(text: &str) -> Result<f64> {
    let mut timing = false;
    let mut beats = Vec::<(f64, f64)>::new();
    for line in text.lines() {
        if line.starts_with('[') {
            timing = line.trim() == "[TimingPoints]";
        }
        if timing
            && let Some((_, rest)) = line.split_once(',')
            && let Some(v) = rest.split(',').next().and_then(|s| s.parse::<f64>().ok())
            && v > 0.0
        {
            beats.push((v, 60000.0 / v));
        }
    }
    beats
        .first()
        .map(|v| v.1)
        .context("map has no uninherited timing point")
}
pub fn effective(options: &Options, text: &str) -> Result<Options> {
    let mut o = options.clone();
    if let Some(target) = o.target_bpm {
        o.rate = rate_from_bpm(bpm(text)?, target)?;
    }
    anyhow::ensure!(
        o.rate.is_finite() && (0.5..=2.0).contains(&o.rate),
        "rate must be 0.5–2.0"
    );
    for x in [o.hp, o.cs, o.ar, o.od].into_iter().flatten() {
        anyhow::ensure!(
            x.is_finite() && (0.0..=10.0).contains(&x),
            "stats must be 0–10"
        );
    }
    Ok(o)
}
pub fn csv(line: &str) -> Vec<String> {
    let mut quote = false;
    let mut start = 0;
    let mut out = Vec::new();
    for (i, c) in line.char_indices() {
        if c == '"' {
            quote = !quote;
        }
        if c == ',' && !quote {
            out.push(line[start..i].to_owned());
            start = i + 1;
        }
    }
    out.push(line[start..].to_owned());
    out
}
fn scale(parts: &mut [String], i: usize, rate: f64) -> Result<()> {
    let p = parts.get_mut(i).context("missing timestamp")?;
    *p = number((p.trim().parse::<f64>().context("invalid timestamp")? / rate).round());
    Ok(())
}
pub fn transform(text: &str, o: &Options, audio: &str) -> Result<String> {
    let o = effective(o, text)?;
    let mut section = String::new();
    let mut out = Vec::new();
    let mode = value(text, "[General]", "Mode").unwrap_or_default();
    let original_ar: f64 = value(text, "[Difficulty]", "ApproachRate")
        .or_else(|| value(text, "[Difficulty]", "OverallDifficulty"))
        .unwrap_or_else(|| "5".into())
        .parse()?;
    let original_od: f64 = value(text, "[Difficulty]", "OverallDifficulty")
        .unwrap_or_else(|| "5".into())
        .parse()?;
    let ar =
        o.ar.unwrap_or_else(|| {
            if o.lock_ar {
                original_ar
            } else {
                ms_ar(ar_ms(original_ar) / o.rate)
            }
        })
        .clamp(0.0, 10.0);
    let od_base = if mode == "1" {
        (50.0, 3.0)
    } else if mode == "3" {
        (64.0, 3.0)
    } else {
        (80.0, 6.0)
    };
    let od =
        o.od.unwrap_or_else(|| {
            if o.lock_od {
                original_od
            } else {
                (od_base.0 - (od_base.0 - od_base.1 * original_od) / o.rate) / od_base.1
            }
        })
        .clamp(0.0, 10.0);
    let name = format!(
        "[{}] {}x ({}bpm) AR{} OD{}",
        value(text, "[Metadata]", "Version").unwrap_or_default(),
        number(o.rate),
        number(bpm(text)? * o.rate),
        number(ar),
        number(od)
    );
    let variables = variables(text);
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.starts_with('[') {
            section = line.to_owned();
            out.push(raw.to_owned());
            continue;
        }
        if line.is_empty() || line.starts_with("//") {
            out.push(raw.to_owned());
            continue;
        }
        if let Some((key, val)) = line.split_once(':') {
            let replacement = match (section.as_str(), key.trim()) {
                ("[General]", "AudioFilename") => Some(audio.to_owned()),
                ("[General]", "PreviewTime") => Some(if val.trim() == "-1" {
                    "-1".into()
                } else {
                    number((val.trim().parse::<f64>()? / o.rate).round())
                }),
                ("[Editor]", "Bookmarks") => Some(
                    val.split(',')
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| {
                            s.trim()
                                .parse::<f64>()
                                .map(|n| number((n / o.rate).round()))
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?
                        .join(","),
                ),
                ("[Metadata]", "Version") => Some(name.clone()),
                ("[Metadata]", "BeatmapID") => Some("0".into()),
                ("[Difficulty]", "ApproachRate") => Some(number(ar)),
                ("[Difficulty]", "OverallDifficulty") => Some(number(od)),
                ("[Difficulty]", "HPDrainRate") => o.hp.map(number),
                ("[Difficulty]", "CircleSize") => o.cs.map(number),
                _ => None,
            };
            if let Some(v) = replacement {
                out.push(format!("{}:{v}", key.trim()));
                continue;
            }
        }
        let expanded = expand(line, &variables);
        let mut p = csv(&expanded);
        match section.as_str() {
            "[TimingPoints]" => {
                p[0] = number(p[0].parse::<f64>()? / o.rate);
                if p.get(1).context("missing beat length")?.parse::<f64>()? > 0.0 {
                    p[1] = number(p[1].parse::<f64>()? / o.rate);
                }
            }
            "[HitObjects]" => {
                scale(&mut p, 2, o.rate)?;
                let kind: u32 = p.get(3).context("missing object type")?.parse()?;
                if kind & 8 != 0 {
                    scale(&mut p, 5, o.rate)?;
                }
                if kind & 128 != 0 {
                    let end = p.get(5).context("missing hold end")?.clone();
                    let (time, rest) = end.split_once(':').unwrap_or((&end, ""));
                    p[5] = format!("{}:{rest}", number((time.parse::<f64>()? / o.rate).round()));
                }
            }
            "[Events]" => {
                if !retime_event(&mut p, o.rate)? {
                    out.push(raw.to_owned());
                    continue;
                }
            }
            _ => {
                out.push(raw.to_owned());
                continue;
            }
        }
        let indent = &raw[..raw.len() - raw.trim_start().len()];
        out.push(format!("{indent}{}", p.join(",")));
    }
    if value(text, "[General]", "AudioFilename").is_none() {
        bail!("missing AudioFilename");
    }
    // Old format maps omit AR; explicitly emit the rate-adjusted value.
    if value(text, "[Difficulty]", "ApproachRate").is_none() {
        let i = out
            .iter()
            .position(|l| l.trim() == "[Difficulty]")
            .context("missing Difficulty section")?;
        out.insert(i + 1, format!("ApproachRate:{}", number(ar)));
    }
    if value(text, "[Metadata]", "BeatmapID").is_none() {
        let i = out
            .iter()
            .position(|l| l.trim() == "[Metadata]")
            .context("missing Metadata section")?;
        out.insert(i + 1, "BeatmapID:0".into());
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    Ok(out.join(newline) + newline)
}

fn retime_event(p: &mut [String], rate: f64) -> Result<bool> {
    match p[0].trim() {
        "3" | "Colour" => scale(p, 1, rate)?,
        "2" | "Break" => {
            scale(p, 1, rate)?;
            scale(p, 2, rate)?;
        }
        "0" | "1" | "Video" | "Background" | "Sample" | "5" => scale(p, 1, rate)?,
        "Animation" | "6" => {
            if p.len() > 7 {
                scale(p, 7, rate)?;
            }
        }
        "L" => scale(p, 1, rate)?,
        "T" => {
            if p.len() > 2 && !p[2].is_empty() {
                scale(p, 2, rate)?;
            }
            if p.len() > 3 && !p[3].is_empty() {
                scale(p, 3, rate)?;
            }
        }
        "F" | "M" | "MX" | "MY" | "S" | "V" | "R" | "C" | "P" => {
            scale(p, 2, rate)?;
            if p.len() > 3 && !p[3].is_empty() {
                scale(p, 3, rate)?;
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub fn storyboard(text: &str, rate: f64) -> Result<String> {
    let mut out = Vec::new();
    let variables = variables(text);
    for raw in text.lines() {
        let expanded = expand(raw.trim(), &variables);
        let mut p = csv(&expanded);
        if raw.trim().starts_with("//") || !retime_event(&mut p, rate)? {
            out.push(raw.to_owned());
        } else {
            let indent = &raw[..raw.len() - raw.trim_start().len()];
            out.push(format!("{indent}{}", p.join(",")));
        }
    }
    Ok(out.join("\n") + "\n")
}

pub fn referenced_assets(text: &str) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut section = "[Events]";
    let variables = variables(text);
    for line in text.lines() {
        if line.starts_with('[') {
            section = line.trim();
            continue;
        }
        let expanded = expand(line.trim(), &variables);
        let p = csv(&expanded);
        let field = match (section, p[0].trim()) {
            ("[Events]", "0" | "1" | "Video" | "Background") => p.get(2),
            ("[Events]", "Sprite" | "Animation" | "4" | "6" | "Sample" | "5") => p.get(3),
            _ => None,
        };
        if let Some(name) = field {
            let name = name.trim_matches('"').replace('\\', "/");
            if section == "[Events]" && matches!(p[0].trim(), "Animation" | "6") {
                if let Some(count) = p.get(6).and_then(|v| v.parse::<usize>().ok())
                    && let Some((base, ext)) = name.rsplit_once('.')
                {
                    for frame in 0..count {
                        names.insert(format!("{base}{frame}.{ext}"));
                    }
                }
            } else {
                names.insert(name);
            }
        }
        // Custom per-object sample filenames are the last hit-sample component.
        if section == "[HitObjects]"
            && p.len() >= 6
            && let Some(sample) = p.last().and_then(|s| s.rsplit(':').next())
            && !sample.is_empty()
            && sample.contains('.')
        {
            names.insert(sample.replace('\\', "/"));
        }
    }
    names.into_iter().collect()
}

fn variables(text: &str) -> Vec<(&str, &str)> {
    let mut section = "";
    let mut vars = Vec::new();
    for line in text.lines() {
        if line.starts_with('[') {
            section = line.trim();
        } else if section == "[Variables]"
            && let Some((k, v)) = line.split_once('=')
            && k.trim().starts_with('$')
        {
            vars.push((k.trim(), v.trim()));
        }
    }
    vars.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    vars
}
fn expand<'a>(line: &'a str, vars: &[(&str, &str)]) -> std::borrow::Cow<'a, str> {
    let mut result = std::borrow::Cow::Borrowed(line);
    for (key, value) in vars {
        if result.contains(key) {
            result = std::borrow::Cow::Owned(result.replace(key, value));
        }
    }
    result
}
