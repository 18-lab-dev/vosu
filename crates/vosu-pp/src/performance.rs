use rosu_pp::{
    Performance,
    any::{DifficultyAttributes, PerformanceAttributes, ScoreState},
};

use crate::{
    DifficultyAttrsData, DifficultyConfig, Error, HitResult, LiveLazerEngine, LiveScoreState,
    ParsedBeatmap, PerformanceAttrsData, Ruleset, ScoreInfoData, checked, configured, hit_window,
};

impl LiveLazerEngine {
    pub fn performance(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        state: LiveScoreState,
    ) -> Result<PerformanceAttributes, Error> {
        self.performance_inner(map, config, None, ScoreInfoData::from(state))
    }

    pub fn performance_data(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        self.performance_data_inner(map, config, None, score, true, None)
    }

    pub fn performance_data_from_attrs(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        attrs: &DifficultyAttrsData,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        self.performance_data_inner(
            map,
            config,
            Some(attrs.inner_for(map.mode())?),
            score,
            true,
            None,
        )
    }

    pub fn performance_data_from_attrs_with_context(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        attrs: &DifficultyAttrsData,
        _full_attrs: &DifficultyAttrsData,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        // Validate both retained layers before calculation so a context from
        // another ruleset cannot silently influence a valid retained result.
        let _ = _full_attrs.inner_for(map.mode())?;
        let context_difficulty = _full_attrs.context.clone();

        self.performance_data_inner(
            map,
            config,
            Some(attrs.inner_for(map.mode())?),
            score,
            false,
            context_difficulty,
        )
    }

    fn performance_data_inner(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        attrs: Option<DifficultyAttributes>,
        score: ScoreInfoData,
        apply_playable_context: bool,
        context_difficulty: Option<rosu_pp::Difficulty>,
    ) -> Result<PerformanceAttrsData, Error> {
        let (state, difficulty, playable, context_mods) =
            if let Some(difficulty) = context_difficulty.clone() {
                let context_mods = difficulty.clone().inspect().mods;
                let state = map.state.clone();
                let playable = state.playable.clone();
                (state, difficulty, playable, Some(context_mods))
            } else {
                let (state, difficulty, playable) = configured(map, config)?;
                (state, difficulty, playable, None)
            };

        let mut attrs = match attrs {
            Some(mut attrs) => {
                if apply_playable_context {
                    apply_playable_performance_context(&mut attrs, &playable, &difficulty);
                }

                attrs
            }
            None => {
                let mut core = rosu_pp::GradualDifficulty::new(difficulty, &playable);
                core.skip_batch_to_end();
                core.attributes().ok_or(Error::DifficultyUnavailable)?
            }
        };

        if state.mode == Ruleset::Taiko {
            apply_original_taiko_hit_windows(&mut attrs, &state, config);
        }

        if let DifficultyAttributes::Catch(attrs) = &attrs {
            let mods = context_mods.as_ref().unwrap_or(&state.game_mods);
            return Ok(catch_performance_data(attrs, mods, score));
        }

        self.performance_inner_with_context(
            map,
            config,
            Some(attrs),
            score,
            false,
            context_difficulty,
        )
        .map(Into::into)
    }

    pub(crate) fn performance_inner(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        attrs: Option<DifficultyAttributes>,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttributes, Error> {
        self.performance_inner_with_context(map, config, attrs, score, true, None)
    }

    pub(crate) fn performance_inner_with_context(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
        attrs: Option<DifficultyAttributes>,
        score: ScoreInfoData,
        apply_playable_context: bool,
        context_difficulty: Option<rosu_pp::Difficulty>,
    ) -> Result<PerformanceAttributes, Error> {
        let (state, difficulty, playable) = if let Some(difficulty) = context_difficulty {
            let state = map.state.clone();
            let playable = state.playable.clone();
            (state, difficulty, playable)
        } else {
            configured(map, config)?
        };
        let mut performance = match attrs {
            Some(attrs) if apply_playable_context => {
                let mut attrs = attrs;
                apply_playable_performance_context(&mut attrs, &playable, &difficulty);
                if state.mode == Ruleset::Taiko {
                    apply_original_taiko_hit_windows(&mut attrs, &state, config);
                }
                Performance::new(attrs).difficulty(difficulty.clone())
            }
            Some(attrs) => Performance::new(attrs).difficulty(difficulty.clone()),
            None if apply_playable_context => {
                let mut core = rosu_pp::GradualDifficulty::new(difficulty.clone(), &playable);
                core.skip_batch_to_end();
                let mut attrs = core.attributes().ok_or(Error::DifficultyUnavailable)?;
                if state.mode == Ruleset::Taiko {
                    apply_original_taiko_hit_windows(&mut attrs, &state, config);
                }
                Performance::new(attrs).difficulty(difficulty.clone())
            }
            None => Performance::new(&playable).difficulty(difficulty),
        };
        performance = performance.state(score_state(state.mode, score)?);
        if state.mode == Ruleset::Osu {
            performance = match performance {
                Performance::Osu(osu) => Performance::Osu(osu.score_accuracy(score.accuracy)),
                _ => unreachable!("ruleset and performance mode differ"),
            };
        }
        if score.is_legacy_score {
            performance = performance.legacy_total_score(score.total_score);
        }
        Ok(performance.calculate())
    }
}

fn apply_playable_performance_context(
    attrs: &mut DifficultyAttributes,
    map: &rosu_pp::Beatmap,
    difficulty: &rosu_pp::Difficulty,
) {
    let adjusted = map.attributes().difficulty(difficulty).build();
    let windows = adjusted.hit_windows();
    match attrs {
        DifficultyAttributes::Osu(attrs) => {
            attrs.ar = adjusted.apply_clock_rate().ar;
            attrs.hp = f64::from(adjusted.hp());
            attrs.great_hit_window = windows.od_great.unwrap_or(attrs.great_hit_window);
            attrs.ok_hit_window = windows.od_ok.unwrap_or(attrs.ok_hit_window);
            attrs.meh_hit_window = windows.od_meh.unwrap_or(attrs.meh_hit_window);
        }
        DifficultyAttributes::Taiko(attrs) => {
            attrs.great_hit_window = windows.od_great.unwrap_or(attrs.great_hit_window);
            attrs.ok_hit_window = windows.od_ok.unwrap_or(attrs.ok_hit_window);
        }
        DifficultyAttributes::Catch(attrs) => {
            attrs.preempt = windows.ar.unwrap_or(attrs.preempt);
        }

        DifficultyAttributes::Mania(_) => {}
    }
}
fn catch_performance_data(
    attrs: &rosu_pp::catch::CatchDifficultyAttributes,
    mods: &rosu_pp::GameMods,
    score: ScoreInfoData,
) -> PerformanceAttrsData {
    let misses = score.misses.wrapping_add(score.large_tick_misses);
    let combo_hits = score
        .greats
        .wrapping_add(score.large_tick_hits)
        .wrapping_add(misses);
    let combo_hits = f64::from(combo_hits);
    let mut pp = (5.0 * (attrs.stars / 0.0049).max(1.0) - 4.0).powf(2.0) / 100_000.0;
    let mut length_bonus = 0.95 + 0.3 * (combo_hits / 2500.0).min(1.0);

    if combo_hits > 2500.0 {
        length_bonus += (combo_hits / 2500.0).log10() * 0.475;
    }

    pp *= length_bonus;
    pp *= 0.97_f64.powf(f64::from(misses));

    if attrs.max_combo() > 0 {
        pp *= (f64::from(score.max_combo).powf(0.35) / f64::from(attrs.max_combo()).powf(0.35))
            .min(1.0);
    }

    let ar = if attrs.preempt > 1200.0 {
        -(attrs.preempt - 1800.0) / 120.0
    } else {
        -(attrs.preempt - 1200.0) / 150.0 + 5.0
    };
    let mut ar_factor = 1.0;

    if ar > 9.0 {
        ar_factor += 0.1 * (ar - 9.0);

        if ar > 10.0 {
            ar_factor += 0.1 * (ar - 10.0);
        }
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

    let successful = score
        .greats
        .wrapping_add(score.large_tick_hits)
        .wrapping_add(score.small_tick_hits);
    let total = successful.wrapping_add(misses.wrapping_add(score.small_tick_misses));
    let accuracy = if total == 0 {
        0.0
    } else {
        (f64::from(successful) / f64::from(total)).clamp(0.0, 1.0)
    };
    pp *= accuracy.powf(5.5);

    if mods.nf() {
        pp *= (1.0 - 0.02 * f64::from(misses)).max(0.9);
    }

    PerformanceAttrsData {
        pp,
        ..PerformanceAttrsData::default()
    }
}

fn apply_original_taiko_hit_windows(
    attrs: &mut DifficultyAttributes,
    state: &crate::beatmap::BeatmapState,
    config: &DifficultyConfig,
) {
    let DifficultyAttributes::Taiko(attrs) = attrs else {
        return;
    };

    let mut original = state.original.as_ref().clone();
    if let Some(od) = config.od {
        original.od = od.clamp(0.0, 10.0);
    }
    crate::beatmap::apply_mods_to_map(&mut original, state.mode, &state.mods);
    let od = f64::from(original.od);
    let rate = config
        .clock_rate
        .unwrap_or_else(|| crate::beatmap::clock_rate_for_mods(&state.mods));

    if let Ok(Some(window)) = hit_window(Ruleset::Taiko, od, HitResult::Great) {
        attrs.great_hit_window = window / rate;
    }
    if let Ok(Some(window)) = hit_window(Ruleset::Taiko, od, HitResult::Ok) {
        attrs.ok_hit_window = window / rate;
    }
}

pub(crate) fn score_state(mode: Ruleset, score: ScoreInfoData) -> Result<ScoreState, Error> {
    let zero = |v: i32| v.max(0) as u32;
    match mode {
        Ruleset::Osu => Ok(ScoreState {
            max_combo: checked("maxCombo", score.max_combo)?,
            osu_large_tick_hits: checked("largeTickHits", score.large_tick_hits)?,
            osu_large_tick_misses: checked("largeTickMisses", score.large_tick_misses)?,
            osu_small_tick_hits: zero(score.small_tick_hits),
            slider_end_hits: checked("sliderEndHits", score.slider_end_hits)?,
            n_geki: 0,
            n_katu: 0,
            n300: zero(score.greats),
            n100: zero(score.oks),
            n50: zero(score.mehs),
            misses: zero(score.misses),
            legacy_total_score: None,
        }),
        Ruleset::Taiko => Ok(ScoreState {
            max_combo: zero(score.max_combo),
            osu_large_tick_hits: 0,
            osu_large_tick_misses: 0,
            osu_small_tick_hits: 0,
            slider_end_hits: 0,
            n_geki: 0,
            n_katu: 0,
            n300: zero(score.greats),
            n100: zero(score.oks),
            n50: zero(score.mehs),
            misses: zero(score.misses),
            legacy_total_score: None,
        }),
        Ruleset::Catch => Ok(ScoreState {
            max_combo: zero(score.max_combo),
            osu_large_tick_hits: 0,
            osu_large_tick_misses: 0,
            osu_small_tick_hits: 0,
            slider_end_hits: 0,
            n_geki: 0,
            n_katu: zero(score.small_tick_misses),
            n300: zero(score.greats),
            n100: zero(score.large_tick_hits),
            n50: zero(score.small_tick_hits),
            misses: zero(score.misses + score.large_tick_misses),
            legacy_total_score: None,
        }),
        Ruleset::Mania => Ok(ScoreState {
            max_combo: 0,
            osu_large_tick_hits: 0,
            osu_large_tick_misses: 0,
            osu_small_tick_hits: 0,
            slider_end_hits: 0,
            n_geki: zero(score.perfects),
            n_katu: zero(score.goods),
            n300: zero(score.greats),
            n100: zero(score.oks),
            n50: zero(score.mehs),
            misses: zero(score.misses),
            legacy_total_score: None,
        }),
    }
}
