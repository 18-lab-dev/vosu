fn ordered_beatmap_difficulty(
    mode: Ruleset,
    map: &Beatmap,
    structured_mods: &[StructuredMod],
) -> BeatmapDifficultyData {
    let mut difficulty = BeatmapDifficultyData {
        approach_rate: map.ar,
        circle_size: map.cs,
        overall_difficulty: map.od,
        drain_rate: map.hp,
    };

    for game_mod in structured_mods {
        let acronym = game_mod.acronym.to_ascii_uppercase();

        if !matches!(acronym.as_str(), "EZ" | "HR" | "DA" | "TP") {
            continue;
        }

        if mode == Ruleset::Mania && matches!(acronym.as_str(), "EZ" | "HR") {
            if acronym == "EZ" {
                difficulty.circle_size *= 0.5;
                difficulty.approach_rate *= 0.5;
                difficulty.drain_rate *= 0.5;
            } else {
                difficulty.drain_rate = (difficulty.drain_rate * 1.4).min(10.0);
            }

            continue;
        }

        let Ok(mods) = game_mods(mode, std::slice::from_ref(game_mod)) else {
            continue;
        };
        let mut single = map.clone();
        single.ar = difficulty.approach_rate;
        single.cs = difficulty.circle_size;
        single.od = difficulty.overall_difficulty;
        single.hp = difficulty.drain_rate;
        let mut builder = single.attributes();
        builder.mods(mods);
        let attrs = builder.build();
        difficulty = BeatmapDifficultyData {
            approach_rate: attrs.ar(),
            circle_size: attrs.cs(),
            overall_difficulty: attrs.od(),
            drain_rate: attrs.hp(),
        };

        if acronym == "TP" {
            difficulty.approach_rate *= 0.5;
        }
    }

    difficulty
}

fn adjusted_display_difficulty(map: &ParsedBeatmap) -> BeatmapDifficultyData {
    let mut difficulty = BeatmapDifficultyData {
        approach_rate: map.map.ar,
        circle_size: map.map.cs,
        overall_difficulty: map.map.od,
        drain_rate: map.map.hp,
    };
    let rate = display_rate(&map.structured_mods);

    match map.mode {
        Ruleset::Osu => {
            difficulty.approach_rate = inverse_difficulty_range(
                difficulty_range(difficulty.approach_rate, 1800.0, 1200.0, 450.0) / rate,
                1800.0,
                1200.0,
                450.0,
            ) as f32;
            difficulty.overall_difficulty = inverse_difficulty_range(
                difficulty_range(difficulty.overall_difficulty, 80.0, 50.0, 20.0) / rate,
                80.0,
                50.0,
                20.0,
            ) as f32;
        }
        Ruleset::Taiko => {
            difficulty.overall_difficulty = inverse_difficulty_range(
                difficulty_range(difficulty.overall_difficulty, 50.0, 35.0, 20.0) / rate,
                50.0,
                35.0,
                20.0,
            ) as f32;
        }
        Ruleset::Catch => {
            difficulty.approach_rate = inverse_difficulty_range(
                difficulty_range(difficulty.approach_rate, 1800.0, 1200.0, 450.0) / rate,
                1800.0,
                1200.0,
                450.0,
            ) as f32;
        }
        Ruleset::Mania => {
            let mut perfect = difficulty_range(difficulty.overall_difficulty, 22.4, 19.4, 13.9);

            if map
                .structured_mods
                .iter()
                .any(|game_mod| game_mod.acronym.eq_ignore_ascii_case("HR"))
            {
                perfect /= 1.4;
            } else if map
                .structured_mods
                .iter()
                .any(|game_mod| game_mod.acronym.eq_ignore_ascii_case("EZ"))
            {
                perfect /= 1.0 / 1.4;
            }

            difficulty.overall_difficulty =
                inverse_difficulty_range(perfect, 22.4, 19.4, 13.9) as f32;
            difficulty.circle_size = mania_display_key_count(map) as f32;
        }
    }

    difficulty
}

fn difficulty_range(
    difficulty: f32,
    value_at_zero: f64,
    value_at_five: f64,
    value_at_ten: f64,
) -> f64 {
    let difficulty = f64::from(difficulty);
    let normalized = (difficulty - 5.0) / 5.0;

    if difficulty > 5.0 {
        value_at_five + (value_at_ten - value_at_five) * normalized
    } else if difficulty < 5.0 {
        value_at_five + (value_at_five - value_at_zero) * normalized
    } else {
        value_at_five
    }
}

fn inverse_difficulty_range(
    value: f64,
    value_at_zero: f64,
    value_at_five: f64,
    value_at_ten: f64,
) -> f64 {
    if (value - value_at_five).signum() == (value_at_ten - value_at_five).signum() {
        (value - value_at_five) / (value_at_ten - value_at_five) * 5.0 + 5.0
    } else {
        (value - value_at_five) / (value_at_five - value_at_zero) * 5.0 + 5.0
    }
}

fn display_rate(structured_mods: &[StructuredMod]) -> f64 {
    structured_mods.iter().fold(1.0, |rate, game_mod| {
        let acronym = game_mod.acronym.to_ascii_uppercase();
        let default = match acronym.as_str() {
            "DT" | "NC" => 1.5,
            "HT" | "DC" => 0.75,
            "AS" | "WU" | "WD" => 1.0,
            _ => return rate,
        };
        let key = if matches!(acronym.as_str(), "AS" | "WU" | "WD") {
            "initial_rate"
        } else {
            "speed_change"
        };
        let configured = match game_mod.settings.get(key) {
            Some(ModValue::Number(value)) if value.is_finite() => *value,
            Some(ModValue::String(value)) => value.parse().unwrap_or(default),
            _ => default,
        };
        let (min, max) = match acronym.as_str() {
            "DT" | "NC" => (1.01, 2.0),
            "HT" | "DC" => (0.5, 0.99),
            "AS" => (0.5, 2.0),
            "WU" => (0.5, 1.99),
            "WD" => (0.51, 2.0),
            _ => unreachable!(),
        };
        let applied = (configured.clamp(min, max) * 100.0).round_ties_even() / 100.0;

        rate * applied
    })
}

fn mania_display_key_count(map: &ParsedBeatmap) -> u32 {
    let native = map.original.mode == GameMode::Mania;
    let mut columns = if native {
        f64::from(map.map.cs).round_ties_even().max(1.0) as u32
    } else {
        let total = map.original.hit_objects.len() as u32;
        let ending = map
            .original
            .hit_objects
            .iter()
            .filter(|object| object.is_slider() || object.is_spinner())
            .count() as u32;
        let rounded_cs = f64::from(map.map.cs).round_ties_even();
        let rounded_od = f64::from(map.map.od).round_ties_even();

        if total > 0 {
            let special = f64::from(ending) / f64::from(total);

            if special < 0.2 {
                7
            } else if special < 0.3 || rounded_cs >= 5.0 {
                if rounded_od > 5.0 { 7 } else { 6 }
            } else if special > 0.6 {
                if rounded_od > 4.0 { 5 } else { 4 }
            } else {
                (rounded_od as i32 + 1).clamp(4, 7) as u32
            }
        } else {
            (rounded_od as i32 + 1).clamp(4, 7) as u32
        }
    };

    if !native {
        let mut dual = false;

        for game_mod in &map.structured_mods {
            let acronym = game_mod.acronym.to_ascii_uppercase();

            if let Some(keys) = acronym
                .strip_suffix('K')
                .and_then(|keys| keys.parse::<u32>().ok())
            {
                columns = keys;
            } else if acronym == "DS" {
                dual = true;
            }
        }

        if dual {
            columns = columns.saturating_mul(2);
        }
    } else if columns > 10 {
        columns = columns / 2 * 2;
    }

    columns
}

fn calculation_map<'a>(
    map: &'a ParsedBeatmap,
    config: &DifficultyConfig,
) -> Result<Cow<'a, Beatmap>, Error> {
    if config.mods.is_empty() {
        return Ok(Cow::Borrowed(&map.map));
    }

    let resolved = map.resolved_config_mods(&config.mods);
    let parsed = deserialize_mods(map.mode(), &resolved)?;
    let playable =
        playable_map(&map.original, map.mode, &parsed, &resolved).map_err(|_| Error::Convert {
            from: Ruleset::from_game_mode(map.original.mode),
            to: map.mode,
        })?;

    Ok(Cow::Owned(playable.map))
}

fn gradual_calculation_map<'a>(
    map: &'a ParsedBeatmap,
    config: &DifficultyConfig,
) -> Result<Cow<'a, Beatmap>, Error> {
    let resolved = if config.mods.is_empty() {
        map.structured_mods.clone()
    } else {
        map.resolved_config_mods(&config.mods)
    };
    let consumes_difficulty = |game_mod: &StructuredMod| {
        matches!(
            game_mod.acronym.to_ascii_uppercase().as_str(),
            "EZ" | "HR" | "DA" | "TP"
        )
    };

    if !resolved.iter().any(consumes_difficulty) {
        return calculation_map(map, config);
    }

    let parsed = deserialize_mods(map.mode(), &resolved)?;
    let mut playable =
        playable_map(&map.original, map.mode, &parsed, &resolved).map_err(|_| Error::Convert {
            from: Ruleset::from_game_mode(map.original.mode),
            to: map.mode,
        })?;
    let retained = resolved
        .iter()
        .filter(|game_mod| !consumes_difficulty(game_mod))
        .cloned()
        .collect::<Vec<_>>();
    let retained_parsed = game_mods(map.mode(), &retained)?;
    let neutral =
        playable_map(&map.original, map.mode, &retained_parsed, &retained).map_err(|_| {
            Error::Convert {
                from: Ruleset::from_game_mode(map.original.mode),
                to: map.mode,
            }
        })?;

    // `DiffWorkingBeatmap.GetPlayableBeatmap` has already folded the ordered
    // difficulty mods. Rosu still needs the typed mods for skill flags and
    // positional transforms, so retain them in `Difficulty` but restore the
    // pre-fold values here and let its builder apply the fold exactly once.
    playable.map.ar = neutral.map.ar;
    playable.map.cs = neutral.map.cs;
    playable.map.od = neutral.map.od;
    playable.map.hp = neutral.map.hp;

    Ok(Cow::Owned(playable.map))
}

fn calculate_difficulty(
    map: &ParsedBeatmap,
    config: &DifficultyConfig,
) -> Result<DifficultyAttributes, Error> {
    let difficulty = build_difficulty(map, config)?;
    let input = calculation_map(map, config)?;
    if map.mode == Ruleset::Osu {
        let mut gradual =
            RosuGradualDifficulty::new_with_mode(difficulty, input.as_ref(), map.mode.game_mode())
                .map_err(|_| Error::Convert {
                    from: Ruleset::from_game_mode(input.mode),
                    to: map.mode,
                })?;
        let RosuGradualDifficulty::Osu(gradual) = &mut gradual else {
            unreachable!("requested osu gradual difficulty");
        };
        gradual.skip_to_end();

        return Ok(DifficultyAttributes::Osu(gradual.attributes()));
    }

    if input.mode == map.mode.game_mode() {
        return Ok(difficulty.calculate(input.as_ref()));
    }

    match map.mode {
        Ruleset::Osu => difficulty
            .calculate_for_mode::<rosu_pp::osu::Osu>(input.as_ref())
            .map(DifficultyAttributes::Osu),
        Ruleset::Taiko => difficulty
            .calculate_for_mode::<rosu_pp::taiko::Taiko>(input.as_ref())
            .map(DifficultyAttributes::Taiko),
        Ruleset::Catch => difficulty
            .calculate_for_mode::<rosu_pp::catch::Catch>(input.as_ref())
            .map(DifficultyAttributes::Catch),
        Ruleset::Mania => difficulty
            .calculate_for_mode::<rosu_pp::mania::Mania>(input.as_ref())
            .map(DifficultyAttributes::Mania),
    }
    .map_err(|_| Error::Convert {
        from: Ruleset::from_game_mode(input.mode),
        to: map.mode,
    })
}

fn calculate_strains(map: &ParsedBeatmap, config: &DifficultyConfig) -> Result<Strains, Error> {
    let difficulty = build_difficulty(map, config)?;
    let input = calculation_map(map, config)?;
    if map.mode == Ruleset::Osu {
        let mut gradual =
            RosuGradualDifficulty::new_with_mode(difficulty, input.as_ref(), map.mode.game_mode())
                .map_err(|_| Error::Convert {
                    from: Ruleset::from_game_mode(input.mode),
                    to: map.mode,
                })?;
        let RosuGradualDifficulty::Osu(gradual) = &mut gradual else {
            unreachable!("requested osu gradual difficulty");
        };
        gradual.skip_to_end();

        return Ok(Strains::Osu(gradual.current_strains()));
    }

    if input.mode == map.mode.game_mode() {
        return Ok(difficulty.strains(input.as_ref()));
    }

    match map.mode {
        Ruleset::Osu => difficulty
            .strains_for_mode::<rosu_pp::osu::Osu>(input.as_ref())
            .map(Strains::Osu),
        Ruleset::Taiko => difficulty
            .strains_for_mode::<rosu_pp::taiko::Taiko>(input.as_ref())
            .map(Strains::Taiko),
        Ruleset::Catch => difficulty
            .strains_for_mode::<rosu_pp::catch::Catch>(input.as_ref())
            .map(Strains::Catch),
        Ruleset::Mania => difficulty
            .strains_for_mode::<rosu_pp::mania::Mania>(input.as_ref())
            .map(Strains::Mania),
    }
    .map_err(|_| Error::Convert {
        from: Ruleset::from_game_mode(input.mode),
        to: map.mode,
    })
}

fn build_difficulty(map: &ParsedBeatmap, config: &DifficultyConfig) -> Result<Difficulty, Error> {
    let mods = if config.mods.is_empty() {
        map.mods.clone()
    } else {
        let resolved = map.resolved_config_mods(&config.mods);
        let parsed = deserialize_mods(map.mode(), &resolved)?;
        calculation_mods(map.mode(), &resolved, parsed)?
    };
    let mut difficulty = Difficulty::new()
        .mods(mods)
        .lazer(!config_classic(map, config));
    if config.mods.is_empty() {
        let applicable_count = map
            .structured_mods
            .iter()
            .filter(|game_mod| {
                matches!(
                    game_mod.acronym.to_ascii_uppercase().as_str(),
                    "EZ" | "HR" | "DA" | "TP"
                )
            })
            .count();

        // The playable map already contains the ordered difficulty fold.
        // Pin those retained values so typed mods remain available for skill
        // and performance flags without applying their difficulty effect again.
        if applicable_count > 0 {
            let adjusted = LiveLazerEngine.beatmap_difficulty(map);
            difficulty = difficulty
                .ar(adjusted.approach_rate, true)
                .cs(adjusted.circle_size, true)
                .hp(adjusted.drain_rate, true)
                .od(adjusted.overall_difficulty, true);
        }
    } else {
        let resolved = map.resolved_config_mods(&config.mods);
        let applicable_count = resolved
            .iter()
            .filter(|game_mod| {
                matches!(
                    game_mod.acronym.to_ascii_uppercase().as_str(),
                    "EZ" | "HR" | "DA" | "TP"
                )
            })
            .count();

        if applicable_count > 0 {
            let input = calculation_map(map, config)?;
            let adjusted = BeatmapDifficultyData {
                approach_rate: input.ar,
                circle_size: input.cs,
                overall_difficulty: input.od,
                drain_rate: input.hp,
            };
            difficulty = difficulty
                .ar(adjusted.approach_rate, true)
                .cs(adjusted.circle_size, true)
                .hp(adjusted.drain_rate, true)
                .od(adjusted.overall_difficulty, true);
        }
    }
    if let Some(value) = config.clock_rate {
        difficulty = difficulty.clock_rate(value);
    }
    if let Some(value) = config.ar {
        difficulty = difficulty.ar(value, true);
    }
    if let Some(value) = config.cs {
        difficulty = difficulty.cs(value, true);
    }
    if let Some(value) = config.hp {
        difficulty = difficulty.hp(value, true);
    }
    if let Some(value) = config.od {
        difficulty = difficulty.od(value, true);
    }
    if let Some(value) = config.passed_objects {
        difficulty = difficulty.passed_objects(value);
    }
    Ok(difficulty)
}

pub fn game_mods(mode: Ruleset, mods: &[StructuredMod]) -> Result<GameMods, Error> {
    let mut sanitized = Vec::with_capacity(mods.len());

    for game_mod in mods {
        let mut accepted = StructuredMod {
            acronym: game_mod.acronym.clone(),
            settings: BTreeMap::new(),
        };

        // APIMod applies settings independently. An arbitrary or malformed
        // setting does not prevent the mod, nor other valid settings on it,
        // from being constructed.
        for (name, value) in &game_mod.settings {
            let mut candidate = accepted.clone();
            candidate.settings.insert(name.clone(), value.clone());

            if parse_game_mods(mode, std::slice::from_ref(&candidate)).is_ok() {
                accepted = candidate;
                continue;
            }

            // APIMod.CopyAdjustedSetting delegates string values to the
            // target setting type's IParseable implementation. Try the same
            // generic numeric and boolean coercions, while retaining the
            // per-setting failure isolation above.
            let ModValue::String(text) = value else {
                continue;
            };
            let coerced = text
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(ModValue::Number)
                .or_else(|| match text.to_ascii_lowercase().as_str() {
                    "true" => Some(ModValue::Bool(true)),
                    "false" => Some(ModValue::Bool(false)),
                    _ => None,
                });
            let Some(coerced) = coerced else {
                continue;
            };

            let mut candidate = accepted.clone();
            candidate.settings.insert(name.clone(), coerced);

            if parse_game_mods(mode, std::slice::from_ref(&candidate)).is_ok() {
                accepted = candidate;
            }
        }

        sanitized.push(accepted);
    }

    parse_game_mods(mode, &sanitized)
}

fn parse_game_mods(mode: Ruleset, mods: &[StructuredMod]) -> Result<GameMods, Error> {
    let mut value = serde_json::to_value(mods).map_err(|err| Error::Mods(err.to_string()))?;

    if let serde_json::Value::Array(items) = &mut value {
        for item in items {
            if let Some(serde_json::Value::Object(settings)) = item.get_mut("settings") {
                let original = std::mem::take(settings);
                settings.extend(
                    original
                        .into_iter()
                        .map(|(key, value)| (camel_to_snake(&key), value)),
                );
            }
        }
    }

    let json = serde_json::to_string(&value).map_err(|err| Error::Mods(err.to_string()))?;
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    let mode = match mode {
        Ruleset::Osu => rosu_mods::GameMode::Osu,
        Ruleset::Taiko => rosu_mods::GameMode::Taiko,
        Ruleset::Catch => rosu_mods::GameMode::Catch,
        Ruleset::Mania => rosu_mods::GameMode::Mania,
    };

    GameModsSeed::Mode {
        mode,
        deny_unknown_fields: false,
    }
    .deserialize(&mut deserializer)
    .map(Into::into)
    .map_err(|err| Error::Mods(err.to_string()))
}

fn deserialize_mods(mode: Ruleset, mods: &[StructuredMod]) -> Result<GameMods, Error> {
    game_mods(mode, mods)
}

fn camel_to_snake(input: &str) -> String {
    let chars: Vec<_> = input.chars().collect();
    let mut output = String::with_capacity(input.len() + 4);

    for (index, &ch) in chars.iter().enumerate() {
        if ch.is_ascii_uppercase() {
            let previous_is_lower_or_digit = index > 0
                && (chars[index - 1].is_ascii_lowercase() || chars[index - 1].is_ascii_digit());
            let ends_acronym_run = index > 0
                && chars[index - 1].is_ascii_uppercase()
                && chars.get(index + 1).is_some_and(char::is_ascii_lowercase);

            if previous_is_lower_or_digit || ends_acronym_run {
                output.push('_');
            }

            output.push(ch.to_ascii_lowercase());
        } else {
            output.push(ch);
        }
    }

    output
}

fn has_mod(mods: &[StructuredMod], acronym: &str) -> bool {
    mods.iter().any(|m| m.acronym.eq_ignore_ascii_case(acronym))
}

fn config_classic(map: &ParsedBeatmap, config: &DifficultyConfig) -> bool {
    if config.mods.is_empty() {
        map.classic_scoring()
    } else {
        has_mod(&config.mods, "CL")
    }
}

