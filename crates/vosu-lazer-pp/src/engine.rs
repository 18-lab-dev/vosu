#[derive(Debug, Clone, Copy, Default)]
pub struct LiveLazerEngine;

impl LiveLazerEngine {
    pub fn parse(&self, bytes: &[u8]) -> Result<ParsedBeatmap, Error> {
        ParsedBeatmap::parse(bytes)
    }
    pub fn parse_str(&self, content: &str) -> Result<ParsedBeatmap, Error> {
        ParsedBeatmap::parse_str(content)
    }
    pub fn convert(&self, map: &ParsedBeatmap, mode: Ruleset) -> Result<ParsedBeatmap, Error> {
        map.convert(mode)
    }
    pub fn apply_mods(&self, map: &mut ParsedBeatmap, mods: &[StructuredMod]) -> Result<(), Error> {
        map.apply_mods(mods)
    }

    pub fn difficulty(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<DifficultyAttributes, Error> {
        calculate_difficulty(map, config)
    }

    pub fn strains(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<Strains, Error> {
        calculate_strains(map, config)
    }

    pub fn difficulty_data(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<DifficultyAttrsData, Error> {
        self.difficulty(map, config)
            .map(|attrs| DifficultyAttrsData::from(&attrs))
    }

    pub fn strains_data(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<StrainsData, Error> {
        let mut data = self.strains(map, config).map(StrainsData::from)?;

        if !build_difficulty(map, config)?.get_mods().fl() {
            data.flashlight = PeakStrains::default();
        }

        Ok(data)
    }

    pub fn original_beatmap_difficulty(&self, map: &ParsedBeatmap) -> BeatmapDifficultyData {
        let attrs = map.original.attributes().build();

        BeatmapDifficultyData {
            approach_rate: attrs.ar(),
            circle_size: attrs.cs(),
            overall_difficulty: attrs.od(),
            drain_rate: attrs.hp(),
        }
    }

    pub fn beatmap_difficulty(&self, map: &ParsedBeatmap) -> BeatmapDifficultyData {
        adjusted_display_difficulty(map)
    }

    pub fn performance(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        state: LiveScoreState,
    ) -> Result<PerformanceAttributes, Error> {
        let difficulty = build_difficulty(map, config)?;
        let classic = config_classic(map, config);
        let attrs = self.difficulty(map, config)?;
        let performance = Performance::new(attrs)
            .mods(difficulty.get_mods().clone())
            .state(performance_score_state(map.mode(), state))
            .lazer(!classic);
        let performance = if map.mode() == Ruleset::Osu {
            performance.accuracy(state.accuracy * 100.0)
        } else {
            performance
        };

        Ok(performance.calculate())
    }

    pub fn progressive_accuracy(
        &self,
        mode: Ruleset,
        state: LiveScoreState,
        classic: bool,
        max_slider_ends: u32,
        max_large_ticks: u32,
    ) -> f64 {
        accuracy(mode, state, classic, max_slider_ends, max_large_ticks)
    }

    pub fn generate_score(
        &self,
        mode: Ruleset,
        totals: ScoreTotals,
        accuracy: f64,
        classic: bool,
    ) -> Result<LiveScoreState, Error> {
        generate_score(mode, totals, accuracy, classic)
    }

    /// Create the wrapper's exact signed score record from the playable,
    /// converted, and modded beatmap.
    pub fn create_score(
        &self,
        map: &ParsedBeatmap,
        target_accuracy: f64,
    ) -> Result<ScoreInfoData, Error> {
        let attrs = self.difficulty(map, &DifficultyConfig::default())?;
        simulate_score(
            map.mode(),
            ScoreTotals::from_difficulty(&attrs),
            target_accuracy,
            config_classic(map, &DifficultyConfig::default()),
        )
    }

    /// Calculate wrapper-compatible accuracy against map-derived object totals.
    pub fn calculate_accuracy(
        &self,
        map: &ParsedBeatmap,
        score: ScoreInfoData,
    ) -> Result<f64, Error> {
        let attrs = self.difficulty(map, &DifficultyConfig::default())?;

        calculate_accuracy_from_difficulty_attrs(map, &attrs, &score)
    }

    /// Calculate wrapper-compatible accuracy from retained difficulty attributes.
    pub fn calculate_accuracy_from_attrs(
        &self,
        map: &ParsedBeatmap,
        attrs: &DifficultyAttrsData,
        score: ScoreInfoData,
    ) -> Result<f64, Error> {
        let attrs = difficulty_attrs_from_data(map, attrs)?;

        calculate_accuracy_from_difficulty_attrs(map, attrs, &score)
    }

    /// Calculate performance from the exact wrapper score DTO.
    pub fn performance_from_score(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttributes, Error> {
        self.performance(map, config, score_to_live(map.mode(), score)?)
    }

    pub fn performance_data(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        if use_signed_performance(map.mode(), &score) {
            let attrs = self.difficulty(map, config)?;
            let difficulty = build_difficulty(map, config)?;

            return Ok(signed_performance_data(
                &attrs,
                difficulty.get_mods(),
                config_classic(map, config),
                &score,
            ));
        }

        self.performance_from_score(map, config, score)
            .map(|attrs| PerformanceAttrsData::from(&attrs))
    }
    /// Calculate performance from the exact difficulty-attribute object
    /// produced by [`LiveLazerEngine::difficulty_data`].
    ///
    /// This mirrors the wrapper overload that consumes
    /// `DifficultyAttributes.Inner`. Retained attributes supply the calculated
    /// difficulty values while current map attributes supply live playable
    /// context such as hit windows.
    pub fn performance_data_from_attrs(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        attrs: &DifficultyAttrsData,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        let difficulty = build_difficulty(map, config)?;
        let classic = config_classic(map, config);
        let retained_attrs = difficulty_attrs_from_data(map, attrs)?;
        let current_attrs = self.difficulty(map, config)?;

        performance_data_from_difficulty_attrs(
            map,
            retained_attrs,
            &current_attrs,
            difficulty.get_mods(),
            classic,
            score,
        )
    }

    /// Calculate performance from retained difficulty attributes using
    /// separately supplied current playable-map context.
    pub fn performance_data_from_attrs_with_context(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        retained_attrs: &DifficultyAttrsData,
        current_attrs: &DifficultyAttrsData,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        let difficulty = build_difficulty(map, config)?;
        let classic = config_classic(map, config);
        let retained_attrs = difficulty_attrs_from_data(map, retained_attrs)?;
        let current_attrs = difficulty_attrs_from_data(map, current_attrs)?;

        performance_data_from_difficulty_attrs(
            map,
            retained_attrs,
            current_attrs,
            difficulty.get_mods(),
            classic,
            score,
        )
    }

    pub fn gradual_difficulty(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<GradualDifficulty, Error> {
        GradualDifficulty::new(map, config)
    }
}

impl From<LiveScoreState> for ScoreState {
    fn from(value: LiveScoreState) -> Self {
        Self {
            max_combo: value.max_combo,
            osu_large_tick_hits: value.large_tick_hits,
            osu_large_tick_misses: value.large_tick_misses,
            osu_small_tick_hits: value.small_tick_hits,
            slider_end_hits: value.slider_end_hits,
            n_geki: value.n_geki,
            n_katu: value.n_katu,
            n300: value.n300,
            n100: value.n100,
            n50: value.n50,
            misses: value.misses,
            legacy_total_score: value.is_legacy_score.then_some(value.total_score),
            total_score: value.total_score,
            is_legacy_score: value.is_legacy_score,
            combo_breaks: value.combo_breaks,
            ignore_hits: value.ignore_hits,
            ignore_misses: value.ignore_misses,
            large_bonuses: value.large_bonuses,
            small_bonuses: value.small_bonuses,
            osu_small_tick_misses: value.small_tick_misses,
        }
    }
}

fn score_to_live(mode: Ruleset, value: ScoreInfoData) -> Result<LiveScoreState, Error> {
    fn unsigned(name: &'static str, value: i32) -> Result<u32, Error> {
        value.try_into().map_err(|_| Error::ScoreOutOfRange(name))
    }

    let mut state = LiveScoreState {
        accuracy: value.accuracy,
        is_legacy_score: value.is_legacy_score,
        ..LiveScoreState::default()
    };

    match mode {
        Ruleset::Osu => {
            state.total_score = match value.total_score.try_into() {
                Ok(score) => score,
                Err(_) if !value.is_legacy_score => 0,
                Err(_) => return Err(Error::ScoreOutOfRange("totalScore")),
            };
            state.max_combo = unsigned("maxCombo", value.max_combo)?;
            state.slider_end_hits = unsigned("sliderEndHits", value.slider_end_hits)?;
            state.combo_breaks = unsigned("comboBreaks", value.combo_breaks)?;
            state.ignore_hits = unsigned("ignoreHits", value.ignore_hits)?;
            state.ignore_misses = unsigned("ignoreMisses", value.ignore_misses)?;
            state.large_bonuses = unsigned("largeBonuses", value.large_bonuses)?;
            state.small_bonuses = unsigned("smallBonuses", value.small_bonuses)?;
            state.large_tick_hits = unsigned("largeTickHits", value.large_tick_hits)?;
            state.large_tick_misses = unsigned("largeTickMisses", value.large_tick_misses)?;
            state.small_tick_hits = unsigned("smallTickHits", value.small_tick_hits)?;
            state.small_tick_misses = unsigned("smallTickMisses", value.small_tick_misses)?;
            state.n300 = unsigned("greats", value.greats)?;
            state.n100 = unsigned("oks", value.oks)?;
            state.n50 = unsigned("mehs", value.mehs)?;
            state.misses = unsigned("misses", value.misses)?;
        }
        Ruleset::Taiko => {
            state.n300 = unsigned("greats", value.greats)?;
            state.n100 = unsigned("oks", value.oks)?;
            state.n50 = unsigned("mehs", value.mehs)?;
            state.misses = unsigned("misses", value.misses)?;
        }
        Ruleset::Catch => {
            state.max_combo = unsigned("maxCombo", value.max_combo)?;
            state.n300 = unsigned("greats", value.greats)?;
            state.large_tick_hits = unsigned("largeTickHits", value.large_tick_hits)?;
            state.large_tick_misses = unsigned("largeTickMisses", value.large_tick_misses)?;
            state.small_tick_hits = unsigned("smallTickHits", value.small_tick_hits)?;
            state.small_tick_misses = unsigned("smallTickMisses", value.small_tick_misses)?;
            state.misses = unsigned("misses", value.misses)?;
        }
        Ruleset::Mania => {
            state.n_geki = unsigned("perfects", value.perfects)?;
            state.n300 = unsigned("greats", value.greats)?;
            state.n_katu = unsigned("goods", value.goods)?;
            state.n100 = unsigned("oks", value.oks)?;
            state.n50 = unsigned("mehs", value.mehs)?;
            state.misses = unsigned("misses", value.misses)?;
        }
    }

    Ok(state)
}

fn performance_score_state(mode: Ruleset, value: LiveScoreState) -> ScoreState {
    let mut state = ScoreState::from(value);

    match mode {
        Ruleset::Catch => {
            state.n100 = value.large_tick_hits;
            state.n50 = value.small_tick_hits;
            state.n_katu = value.small_tick_misses;
            state.misses = value.misses + value.large_tick_misses;
        }
        Ruleset::Taiko => {
            state.n50 = value.n50;
        }
        Ruleset::Osu | Ruleset::Mania => {}
    }

    state
}

fn use_signed_performance(mode: Ruleset, score: &ScoreInfoData) -> bool {
    matches!(mode, Ruleset::Taiko | Ruleset::Catch) || score_has_negative_results(mode, score)
}

fn score_has_negative_results(mode: Ruleset, score: &ScoreInfoData) -> bool {
    match mode {
        Ruleset::Osu => {
            [
                score.max_combo,
                score.slider_end_hits,
                score.large_tick_hits,
                score.large_tick_misses,
                score.small_tick_hits,
                score.greats,
                score.oks,
                score.mehs,
                score.misses,
            ]
            .into_iter()
            .any(|value| value < 0)
                || (score.is_legacy_score && score.total_score < 0)
        }
        Ruleset::Taiko => [
            score.max_combo,
            score.greats,
            score.oks,
            score.mehs,
            score.misses,
        ]
        .into_iter()
        .any(|value| value < 0),
        Ruleset::Catch => [
            score.max_combo,
            score.greats,
            score.large_tick_hits,
            score.large_tick_misses,
            score.small_tick_hits,
            score.small_tick_misses,
            score.misses,
        ]
        .into_iter()
        .any(|value| value < 0),
        Ruleset::Mania => [
            score.perfects,
            score.greats,
            score.goods,
            score.oks,
            score.mehs,
            score.misses,
        ]
        .into_iter()
        .any(|value| value < 0),
    }
}

fn difficulty_attrs_mode(attrs: &DifficultyAttributes) -> Ruleset {
    match attrs {
        DifficultyAttributes::Osu(_) => Ruleset::Osu,
        DifficultyAttributes::Taiko(_) => Ruleset::Taiko,
        DifficultyAttributes::Catch(_) => Ruleset::Catch,
        DifficultyAttributes::Mania(_) => Ruleset::Mania,
    }
}

fn validate_difficulty_attrs_mode(
    map: &ParsedBeatmap,
    attrs: &DifficultyAttributes,
) -> Result<(), Error> {
    let actual_mode = difficulty_attrs_mode(attrs);

    if actual_mode != map.mode() {
        return Err(Error::DifficultyModeMismatch {
            expected: map.mode(),
            actual: actual_mode,
        });
    }

    Ok(())
}

fn difficulty_attrs_from_data<'a>(
    map: &ParsedBeatmap,
    attrs: &'a DifficultyAttrsData,
) -> Result<&'a DifficultyAttributes, Error> {
    let attrs = attrs
        .inner
        .as_ref()
        .ok_or(Error::MissingDifficultyAttributes)?;

    validate_difficulty_attrs_mode(map, attrs)?;

    Ok(attrs)
}

fn calculate_accuracy_from_difficulty_attrs(
    map: &ParsedBeatmap,
    attrs: &DifficultyAttributes,
    score: &ScoreInfoData,
) -> Result<f64, Error> {
    validate_difficulty_attrs_mode(map, attrs)?;

    Ok(calculate_accuracy(
        map.mode(),
        score,
        config_classic(map, &DifficultyConfig::default()),
        ScoreTotals::from_difficulty(attrs),
    ))
}

fn performance_data_from_difficulty_attrs(
    map: &ParsedBeatmap,
    retained_attrs: &DifficultyAttributes,
    current_attrs: &DifficultyAttributes,
    mods: &GameMods,
    classic: bool,
    score: ScoreInfoData,
) -> Result<PerformanceAttrsData, Error> {
    validate_difficulty_attrs_mode(map, retained_attrs)?;
    validate_difficulty_attrs_mode(map, current_attrs)?;

    let attrs = apply_current_performance_context(retained_attrs, current_attrs);

    if use_signed_performance(map.mode(), &score) {
        return Ok(signed_performance_data(
            &attrs, mods, classic, &score,
        ));
    }

    let state = score_to_live(map.mode(), score)?;
    let performance = Performance::new(attrs)
        .mods(mods.clone())
        .state(performance_score_state(map.mode(), state))
        .lazer(!classic);
    let performance = if map.mode() == Ruleset::Osu {
        performance.accuracy(state.accuracy * 100.0)
    } else {
        performance
    };

    Ok(PerformanceAttrsData::from(&performance.calculate()))
}

fn apply_current_performance_context(
    retained: &DifficultyAttributes,
    current: &DifficultyAttributes,
) -> DifficultyAttributes {
    let mut effective = retained.clone();

    match (&mut effective, current) {
        (DifficultyAttributes::Osu(retained), DifficultyAttributes::Osu(current)) => {
            retained.ar = current.ar;
            retained.hp = current.hp;
            retained.great_hit_window = current.great_hit_window;
            retained.ok_hit_window = current.ok_hit_window;
            retained.meh_hit_window = current.meh_hit_window;
        }
        (DifficultyAttributes::Taiko(retained), DifficultyAttributes::Taiko(current)) => {
            retained.great_hit_window = current.great_hit_window;
            retained.ok_hit_window = current.ok_hit_window;
        }
        (DifficultyAttributes::Catch(retained), DifficultyAttributes::Catch(current)) => {
            retained.preempt = current.preempt;
        }
        _ => {}
    }

    effective
}

fn signed_performance_data(
    attrs: &DifficultyAttributes,
    mods: &GameMods,
    classic: bool,
    score: &ScoreInfoData,
) -> PerformanceAttrsData {
    match attrs {
        DifficultyAttributes::Osu(attrs) => {
            signed_osu::calculate(attrs, mods, mods.no_slider_head_acc(true), score)
        }
        DifficultyAttributes::Taiko(attrs) => signed_taiko::calculate(attrs, mods, classic, score),
        DifficultyAttributes::Catch(attrs) => {
            let combined_misses = score.misses.wrapping_add(score.large_tick_misses);
            let combo_hits = f64::from(
                score
                    .greats
                    .wrapping_add(score.large_tick_hits)
                    .wrapping_add(combined_misses),
            );
            let mut pp = (5.0 * (attrs.stars / 0.0049).max(1.0) - 4.0).powf(2.0) / 100_000.0;
            let mut length_bonus = 0.95 + 0.3 * (combo_hits / 2500.0).min(1.0);

            if combo_hits > 2500.0 {
                length_bonus += (combo_hits / 2500.0).log10() * 0.475;
            }

            pp *= length_bonus;
            pp *= 0.97_f64.powf(f64::from(combined_misses));

            if attrs.max_combo() > 0 {
                pp *= (f64::from(score.max_combo).powf(0.35)
                    / f64::from(attrs.max_combo()).powf(0.35))
                .min(1.0);
            }

            let ar = if attrs.preempt > 1200.0 {
                -(attrs.preempt - 1800.0) / 120.0
            } else {
                -(attrs.preempt - 1200.0) / 150.0 + 5.0
            };
            let mut ar_factor = 1.0;

            if ar > 9.0 {
                ar_factor += 0.1 * (ar - 9.0) + f64::from(u8::from(ar > 10.0)) * 0.1 * (ar - 10.0);
            } else if ar < 8.0 {
                ar_factor += 0.025 * (8.0 - ar);
            }

            pp *= ar_factor;

            if mods.hd() {
                pp *= if ar <= 10.0 {
                    1.05 + 0.075 * (10.0 - ar)
                } else {
                    1.01 + 0.04 * (11.0 - ar.min(11.0))
                };
            }

            if mods.fl() {
                pp *= 1.35 * length_bonus;
            }

            let hits = f64::from(
                score
                    .greats
                    .wrapping_add(score.large_tick_hits)
                    .wrapping_add(score.small_tick_hits),
            );
            let total = hits + f64::from(combined_misses.wrapping_add(score.small_tick_misses));
            let accuracy = if total == 0.0 {
                0.0
            } else {
                (hits / total).clamp(0.0, 1.0)
            };
            pp *= accuracy.powf(5.5);

            if mods.nf() {
                pp *= (1.0 - 0.02 * f64::from(combined_misses)).max(0.9);
            }

            PerformanceAttrsData {
                pp,
                ..PerformanceAttrsData::default()
            }
        }
        DifficultyAttributes::Mania(attrs) => {
            let total_hits_i32 = score
                .perfects
                .wrapping_add(score.greats)
                .wrapping_add(score.goods)
                .wrapping_add(score.oks)
                .wrapping_add(score.mehs)
                .wrapping_add(score.misses);
            let total_hits = f64::from(total_hits_i32);
            let custom_accuracy = if total_hits == 0.0 {
                0.0
            } else {
                let numerator = score
                    .perfects
                    .wrapping_mul(320)
                    .wrapping_add(score.greats.wrapping_mul(300))
                    .wrapping_add(score.goods.wrapping_mul(200))
                    .wrapping_add(score.oks.wrapping_mul(100))
                    .wrapping_add(score.mehs.wrapping_mul(50));

                f64::from(numerator) / (total_hits * 320.0)
            };
            let difficulty = 8.0
                * (attrs.stars - 0.15).max(0.05).powf(2.2)
                * (5.0 * custom_accuracy - 4.0).max(0.0)
                * (1.0 + 0.1 * (total_hits / 1500.0).min(1.0));
            let mut multiplier = 1.0;

            if mods.nf() {
                multiplier *= 0.75;
            }

            if mods.ez() {
                multiplier *= 0.5;
            }

            let _ = classic;

            PerformanceAttrsData {
                pp: difficulty * multiplier,
                pp_difficulty: difficulty,
                ..PerformanceAttrsData::default()
            }
        }
    }
}

