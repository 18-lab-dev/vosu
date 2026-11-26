//! ADR 0004 legacy calculation; only declared roots or published chart bytes.
use crate::files::Provider;
use bytes::Bytes;
use serde::Serialize;
use serde_json::{Serializer, Value};
use vosu_adapter_support::json::JsFormatter;
use vosu_osu_calc::engine::{
    HitResultPriority, LegacyMods, LegacyPerformanceRequest, LegacyPerformanceResult,
    LegacyRosuEngine,
};
use vosu_surface::{files::FileError, resources::read_chart};

#[derive(Clone, Debug, PartialEq)]
pub enum ModsInput {
    Bits(u32),
    Json(Value),
    Acronyms(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PpQuery {
    pub path: Option<String>,
    pub lazer: Option<bool>,
    pub mode: Option<u8>,
    pub ar: Option<f64>,
    pub cs: Option<f64>,
    pub hp: Option<f64>,
    pub od: Option<f64>,
    pub clock_rate: Option<f64>,
    pub passed_objects: Option<u32>,
    pub combo: Option<u32>,
    pub n_misses: Option<u32>,
    pub n100: Option<u32>,
    pub n300: Option<u32>,
    pub n50: Option<u32>,
    pub n_geki: Option<u32>,
    pub n_katu: Option<u32>,
    pub mods: Option<ModsInput>,
    pub accuracy: Option<f64>,
    pub slider_end_hits: Option<u32>,
    pub small_tick_hits: Option<u32>,
    pub large_tick_hits: Option<u32>,
    pub hitresult_priority: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResultBody {
    Ready(Bytes),
    NotReady,
    Failed { status: u16, body: Bytes },
}

impl Provider {
    pub async fn calculate(&self, query: PpQuery) -> ResultBody {
        let selected = match self.capture() {
            Ok(selected) => selected,
            Err(_) => return ResultBody::NotReady,
        };
        let bytes = if let Some(path) = query.path.as_deref() {
            if std::path::Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("osu"))
            {
                return failure(400, "path must select a .osu file");
            }
            match selected
                .requester
                .open_calculation_path(&selected.session, path)
                .await
            {
                Ok(file) => read_chart(file).await,
                Err(error) => Err(error),
            }
        } else {
            let Some(token) = selected.chart.as_deref() else {
                return ResultBody::NotReady;
            };
            match selected
                .requester
                .open_published_resource(&selected.session, token)
                .await
            {
                Ok(resource) => resource.read_chart().await,
                Err(FileError::NotReady | FileError::NotFound) => return ResultBody::NotReady,
                Err(error) => Err(error),
            }
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                return failure(
                    400,
                    match error {
                        FileError::LimitExceeded => "beatmap exceeds the 64 MiB input limit",
                        FileError::InvalidPath | FileError::PermissionDenied => {
                            "invalid beatmap path: outside declared roots or unsafe spelling"
                        }
                        FileError::NotFound => "beatmap path does not exist",
                        _ => "failed to read beatmap",
                    },
                );
            }
        };
        let request = match performance_request(&query) {
            Ok(request) => request,
            Err(error) => return failure(400, &error),
        };
        match LegacyRosuEngine.calculate(&bytes, &request) {
            Ok(result) => match serialize_success(&result) {
                Ok(body) => ResultBody::Ready(body),
                Err(error) => failure(500, &error.to_string()),
            },
            Err(error) => failure(400, &error.to_string()),
        }
    }
}

fn serialize_success(result: &LegacyPerformanceResult) -> serde_json::Result<Bytes> {
    let mut output = Vec::with_capacity(1024);
    result.serialize(&mut Serializer::with_formatter(&mut output, JsFormatter))?;
    Ok(Bytes::from(output))
}

fn failure(status: u16, message: &str) -> ResultBody {
    ResultBody::Failed {
        status,
        body: Bytes::from(
            serde_json::to_vec(&serde_json::json!({"error": message}))
                .expect("error JSON serializes"),
        ),
    }
}
fn performance_request(query: &PpQuery) -> Result<LegacyPerformanceRequest, String> {
    const MIN_CLOCK_RATE: f64 = 0.01;
    const MAX_CLOCK_RATE: f64 = 100.0;
    if query
        .clock_rate
        .is_some_and(|rate| !rate.is_finite() || !(MIN_CLOCK_RATE..=MAX_CLOCK_RATE).contains(&rate))
    {
        return Err("clockRate must be between 0.01 and 100".into());
    }
    let mode = match query.mode {
        None => None,
        Some(0) => Some(vosu_osu_calc::engine::Ruleset::Osu),
        Some(1) => Some(vosu_osu_calc::engine::Ruleset::Taiko),
        Some(2) => Some(vosu_osu_calc::engine::Ruleset::Catch),
        Some(3) => Some(vosu_osu_calc::engine::Ruleset::Mania),
        Some(_) => return Err("mode must be 0, 1, 2, or 3".into()),
    };
    let mods = parse_legacy_mods(query.mods.as_ref())?;
    let hitresult_priority = match query.hitresult_priority.unwrap_or(0) {
        0 => HitResultPriority::BestCase,
        1 => HitResultPriority::WorstCase,
        _ => return Err("hitresultPriority must be 0 or 1".into()),
    };
    Ok(LegacyPerformanceRequest {
        mode,
        lazer: query.lazer,
        mods,
        clock_rate: query.clock_rate,
        ar: query.ar.map(|value| value as f32),
        cs: query.cs.map(|value| value as f32),
        hp: query.hp.map(|value| value as f32),
        od: query.od.map(|value| value as f32),
        passed_objects: query.passed_objects,
        combo: query.combo,
        n_misses: query.n_misses,
        n100: query.n100,
        n300: query.n300,
        n50: query.n50,
        n_geki: query.n_geki,
        n_katu: query.n_katu,
        slider_end_hits: query.slider_end_hits,
        small_tick_hits: query.small_tick_hits,
        large_tick_hits: query.large_tick_hits,
        accuracy: query.accuracy,
        hitresult_priority,
    })
}

fn parse_legacy_mods(input: Option<&ModsInput>) -> Result<LegacyMods, String> {
    use ModsInput;
    match input {
        None => Ok(LegacyMods::Bits(0)),
        Some(ModsInput::Bits(bits)) => Ok(LegacyMods::Bits(*bits)),
        Some(ModsInput::Json(value)) => serde_json::from_value::<LegacyMods>(value.clone())
            .map_err(|error| format!("invalid mods: {error}")),
        Some(ModsInput::Acronyms(value)) => {
            let compact: String = value
                .chars()
                .filter(|ch| !ch.is_ascii_whitespace() && *ch != ',')
                .collect();
            if !compact.len().is_multiple_of(2)
                || !compact.bytes().all(|byte| byte.is_ascii_alphabetic())
            {
                return Err("invalid mods acronyms".into());
            }
            let mods = compact
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| vosu_osu_calc::engine::StructuredMod {
                    acronym: String::from_utf8_lossy(pair).to_ascii_uppercase(),
                    settings: Default::default(),
                })
                .collect();
            Ok(LegacyMods::Structured(mods))
        }
    }
}

impl PpQuery {
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        let mut result = Self::default();
        for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
            let value = value.into_owned();
            macro_rules! parse {
                ($field:ident, $ty:ty) => {
                    result.$field =
                        Some(value.parse::<$ty>().map_err(|_| format!("invalid {key}"))?)
                };
            }
            macro_rules! count {
                ($field:ident) => {
                    result.$field = Some(parse_js_u32(&value, &key)?)
                };
            }
            match key.as_ref() {
                "path" => result.path = Some(value),
                "lazer" => parse!(lazer, bool),
                "mode" => parse!(mode, u8),
                "ar" => parse!(ar, f64),
                "cs" => parse!(cs, f64),
                "hp" => parse!(hp, f64),
                "od" => parse!(od, f64),
                "clockRate" => parse!(clock_rate, f64),
                "passedObjects" => count!(passed_objects),
                "combo" => count!(combo),
                "nMisses" => count!(n_misses),
                "n100" => count!(n100),
                "n300" => count!(n300),
                "n50" => count!(n50),
                "nGeki" => count!(n_geki),
                "nKatu" => count!(n_katu),
                "acc" => parse!(accuracy, f64),
                "sliderEndHits" => count!(slider_end_hits),
                "smallTickHits" => count!(small_tick_hits),
                "largeTickHits" => count!(large_tick_hits),
                "hitresultPriority" => {
                    result.hitresult_priority = Some(
                        parse_js_u32(&value, &key)?
                            .try_into()
                            .map_err(|_| format!("invalid {key}"))?,
                    )
                }
                "mods" => result.mods = Some(parse_mods(&value)?),
                _ => {}
            }
        }
        Ok(result)
    }
}

fn parse_js_u32(value: &str, key: &str) -> Result<u32, String> {
    let value = value.trim();
    let number = if value.is_empty() {
        0.0
    } else if let Some(digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(digits, 16)
            .map(|value| value as f64)
            .map_err(|_| format!("invalid {key}"))?
    } else if let Some(digits) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        u64::from_str_radix(digits, 2)
            .map(|value| value as f64)
            .map_err(|_| format!("invalid {key}"))?
    } else if let Some(digits) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        u64::from_str_radix(digits, 8)
            .map(|value| value as f64)
            .map_err(|_| format!("invalid {key}"))?
    } else {
        value.parse::<f64>().map_err(|_| format!("invalid {key}"))?
    };
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > u32::MAX as f64 {
        return Err(format!("invalid {key}"));
    }
    Ok(number as u32)
}

fn parse_mods(value: &str) -> Result<ModsInput, String> {
    if let Ok(bits) = value.parse::<u32>() {
        return Ok(ModsInput::Bits(bits));
    }
    if matches!(value.trim_start().chars().next(), Some('{' | '[')) {
        let json: Value = serde_json::from_str(value).map_err(|_| "invalid mods".to_owned())?;
        if json.is_object() || json.is_array() {
            return Ok(ModsInput::Json(json));
        }
        return Err("mods JSON must be an object or array".to_owned());
    }
    Ok(ModsInput::Acronyms(value.to_owned()))
}

#[cfg(test)]
mod tests;
