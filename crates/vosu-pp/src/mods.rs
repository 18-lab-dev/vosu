use rosu_mods::serde::GameModsSeed;
use rosu_pp::{Beatmap, GameMods};
use serde::de::DeserializeSeed;
use serde_json::{Map, Value};

use crate::{DifficultyConfig, Error, ModValue, Ruleset, StructuredMod, camel_to_snake};

pub fn game_mods(mode: Ruleset, mods: &[StructuredMod]) -> Result<GameMods, Error> {
    Ok(GameMods::from(decode_mods(mode, mods)?))
}

pub(crate) fn decode_mods(
    mode: Ruleset,
    mods: &[StructuredMod],
) -> Result<rosu_mods::GameMods, Error> {
    let values = mods.iter().map(normalized_mod).collect::<Vec<_>>();
    let json = serde_json::to_string(&values).map_err(|err| Error::Mods(err.to_string()))?;
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    GameModsSeed::Mode {
        mode: mode.mods_mode(),
        deny_unknown_fields: false,
    }
    .deserialize(&mut deserializer)
    .map_err(|err| Error::Mods(err.to_string()))
}

fn normalized_mod(game_mod: &StructuredMod) -> Value {
    let mut settings = Map::new();
    for (name, value) in &game_mod.settings {
        let name = camel_to_snake(name);
        let value = match value {
            ModValue::Number(value) if value.is_finite() => Value::from(*value),
            ModValue::Bool(value) => Value::from(*value),
            ModValue::String(value) if value.eq_ignore_ascii_case("true") => Value::Bool(true),
            ModValue::String(value) if value.eq_ignore_ascii_case("false") => Value::Bool(false),
            ModValue::String(value) if name == "reflection" => Value::String(value.clone()),
            ModValue::String(value) if value.parse::<f64>().is_ok() => {
                Value::from(value.parse::<f64>().unwrap_or_default())
            }
            ModValue::String(value) => Value::String(value.clone()),
            ModValue::Json(value)
                if value.is_boolean() || value.is_number() || value.is_string() =>
            {
                value.clone()
            }
            _ => continue,
        };
        settings.insert(name, value);
    }
    serde_json::json!({ "acronym": game_mod.acronym.to_ascii_uppercase(), "settings": settings })
}

pub(crate) fn difficulty_builder(
    map: &ParsedMapView<'_>,
    config: &DifficultyConfig,
) -> Result<(rosu_pp::Difficulty, Beatmap), Error> {
    let resolved_config = crate::beatmap::resolve_random_seeds(&config.mods);
    let mut combined = map.mods.to_vec();
    combined.extend(resolved_config.iter().cloned());
    let typed_input: Vec<_> = combined
        .iter()
        .filter(|game_mod| !crate::beatmap::is_consumed_mod(map.mode, &game_mod.acronym))
        .cloned()
        .collect();
    let mods = game_mods(map.mode, &typed_input)?;
    let playable = if config.mods.is_empty() {
        map.playable.clone()
    } else {
        let mut playable = map.playable.clone();
        crate::beatmap::apply_mods_to_map(&mut playable, map.mode, &resolved_config);
        playable
    };
    let mut difficulty = rosu_pp::Difficulty::new()
        .mods(mods.clone().into_core())
        .lazer(config.lazer.unwrap_or(true))
        .ar(playable.ar, false)
        .cs(playable.cs, false)
        .hp(playable.hp, false)
        .od(playable.od, false);
    if let Some(value) = config.clock_rate {
        difficulty = difficulty.clock_rate(value);
    }
    if let Some(value) = config.passed_objects {
        difficulty = difficulty.passed_objects(value);
    }
    if let Some(value) = config.ar {
        difficulty = difficulty.ar(value, false);
    }
    if let Some(value) = config.cs {
        difficulty = difficulty.cs(value, false);
    }
    if let Some(value) = config.hp {
        difficulty = difficulty.hp(value, false);
    }
    if let Some(value) = config.od {
        difficulty = difficulty.od(value, false);
    }
    if let Some(value) = config.hardrock_offsets {
        difficulty = difficulty.hardrock_offsets(value);
    }

    Ok((difficulty, playable))
}

pub(crate) struct ParsedMapView<'a> {
    pub mode: Ruleset,
    pub playable: &'a Beatmap,
    pub mods: &'a [StructuredMod],
}
