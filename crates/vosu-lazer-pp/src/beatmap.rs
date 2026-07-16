#[derive(Clone)]
struct PlayBeatmapContext {
    mods: GameMods,
    classic: bool,
}

pub struct ParsedBeatmap {
    original: Beatmap,
    map: Beatmap,
    mode: Ruleset,
    mods: GameMods,
    structured_mods: Vec<StructuredMod>,
    classic: bool,
    mania_score_v2: bool,
    config_mod_cache: Arc<Mutex<BTreeMap<String, Vec<StructuredMod>>>>,
    context: Arc<RwLock<PlayBeatmapContext>>,
}

impl Clone for ParsedBeatmap {
    fn clone(&self) -> Self {
        let cached_config_mods = self
            .config_mod_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let context = self
            .context
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        Self {
            original: self.original.clone(),
            map: self.map.clone(),
            mode: self.mode,
            mods: self.mods.clone(),
            structured_mods: self.structured_mods.clone(),
            classic: self.classic,
            mania_score_v2: self.mania_score_v2,
            config_mod_cache: Arc::new(Mutex::new(cached_config_mods)),
            context: Arc::new(RwLock::new(context)),
        }
    }
}

impl std::fmt::Debug for ParsedBeatmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedBeatmap")
            .field("mode", &self.mode())
            .field("objects", &self.map.hit_objects.len())
            .finish()
    }
}

impl ParsedBeatmap {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        Beatmap::from_bytes(bytes)
            .map(|map| {
                let mode = Ruleset::from_game_mode(map.mode);

                Self {
                    original: map.clone(),
                    map,
                    mode,
                    mods: GameMods::default(),
                    structured_mods: Vec::new(),
                    classic: false,
                    mania_score_v2: false,
                    config_mod_cache: Arc::new(Mutex::new(BTreeMap::new())),
                    context: Arc::new(RwLock::new(PlayBeatmapContext {
                        mods: GameMods::default(),
                        classic: false,
                    })),
                }
            })
            .map_err(|err| Error::Parse(err.to_string()))
    }

    pub fn parse_str(content: &str) -> Result<Self, Error> {
        Self::parse(content.as_bytes())
    }

    pub const fn mode(&self) -> Ruleset {
        self.mode
    }
    pub const fn mode_id(&self) -> i32 {
        self.mode.mode_id()
    }
    pub fn object_count(&self) -> u32 {
        self.map.hit_objects.len().try_into().unwrap_or(u32::MAX)
    }
    pub fn structured_mods(&self) -> &[StructuredMod] {
        &self.structured_mods
    }
    pub const fn classic_scoring(&self) -> bool {
        self.classic
    }

    fn resolved_config_mods(&self, mods: &[StructuredMod]) -> Vec<StructuredMod> {
        let key = serde_json::to_string(mods)
            .expect("structured mods always serialize into a stable cache key");
        let mut cache = self
            .config_mod_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        cache
            .entry(key)
            .or_insert_with(|| materialize_random_seeds(self.mode, mods))
            .clone()
    }

    /// Development-only projection hook for committed conversion provenance
    /// tests. Release builds do not expose the playable map internals.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn debug_playable_map(&self) -> &Beatmap {
        &self.map
    }

    /// Development-only access to the typed mods paired with
    /// [`Self::debug_playable_map`].
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn debug_game_mods(&self) -> &GameMods {
        &self.mods
    }

    /// Development-only access to the unconverted source map.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn debug_original_map(&self) -> &Beatmap {
        &self.original
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn debug_resolved_config_mods(&self, mods: &[StructuredMod]) -> Vec<StructuredMod> {
        self.resolved_config_mods(mods)
    }

    pub fn convert(&self, mode: Ruleset) -> Result<Self, Error> {
        let from = self.mode();
        let playable = playable_map(&self.original, mode, &GameMods::default(), &[])
            .map_err(|_| Error::Convert { from, to: mode })?;

        Ok(Self {
            original: self.original.clone(),
            map: playable.map,
            mode,
            mods: GameMods::default(),
            config_mod_cache: Arc::new(Mutex::new(BTreeMap::new())),
            structured_mods: Vec::new(),
            classic: playable.mania_classic,
            mania_score_v2: playable.mania_score_v2,
            context: Arc::new(RwLock::new(PlayBeatmapContext {
                mods: GameMods::default(),
                classic: playable.mania_classic,
            })),
        })
    }

    /// Replace applied mods on this same play-beatmap handle.
    ///
    /// Rebuilding from the flat original invalidates every converted/nested
    /// playable object derived for the previous mod set.
    pub fn apply_mods(&mut self, mods: &[StructuredMod]) -> Result<(), Error> {
        let resolved_mods = materialize_random_seeds(self.mode(), mods);
        let parsed = deserialize_mods(self.mode(), &resolved_mods)?;
        let playable =
            playable_map(&self.original, self.mode, &parsed, &resolved_mods).map_err(|_| {
                Error::Convert {
                    from: Ruleset::from_game_mode(self.original.mode),
                    to: self.mode,
                }
            })?;
        let calculation_mods = calculation_mods(self.mode(), &resolved_mods, parsed)?;

        self.map = playable.map;
        self.mods = calculation_mods;
        self.structured_mods = resolved_mods;
        self.classic = playable.mania_classic || has_mod(mods, "CL");
        self.mania_score_v2 = playable.mania_score_v2;
        *self
            .context
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = PlayBeatmapContext {
            mods: self.mods.clone(),
            classic: self.classic,
        };

        Ok(())
    }
}
fn materialize_random_seeds(mode: Ruleset, mods: &[StructuredMod]) -> Vec<StructuredMod> {
    mods.iter()
        .cloned()
        .map(|mut game_mod| {
            let is_random = game_mod.acronym.eq_ignore_ascii_case("RD")
                && matches!(mode, Ruleset::Osu | Ruleset::Taiko | Ruleset::Mania);
            let is_target_practice =
                mode == Ruleset::Osu && game_mod.acronym.eq_ignore_ascii_case("TP");

            if (is_random || is_target_practice)
                && !matches!(
                    game_mod.settings.get("seed"),
                    Some(ModValue::Number(_) | ModValue::String(_))
                )
            {
                let seed = (RandomState::new().build_hasher().finish() % i32::MAX as u64) as f64;
                game_mod
                    .settings
                    .insert("seed".to_owned(), ModValue::Number(seed));
            }

            game_mod
        })
        .collect()
}

fn calculation_mods(
    mode: Ruleset,
    structured_mods: &[StructuredMod],
    parsed: GameMods,
) -> Result<GameMods, Error> {
    let consumes_map_mod = |game_mod: &&StructuredMod| match mode {
        Ruleset::Taiko => matches!(
            game_mod.acronym.to_ascii_uppercase().as_str(),
            "RD" | "SR" | "SW"
        ),
        Ruleset::Mania => matches!(
            game_mod.acronym.to_ascii_uppercase().as_str(),
            "HO" | "IN" | "MR" | "RD"
        ),
        Ruleset::Osu | Ruleset::Catch => false,
    };

    if structured_mods
        .iter()
        .any(|game_mod| consumes_map_mod(&game_mod))
    {
        let retained = structured_mods
            .iter()
            .filter(|game_mod| !consumes_map_mod(game_mod))
            .cloned()
            .collect::<Vec<_>>();

        game_mods(mode, &retained)
    } else {
        Ok(parsed)
    }
}

struct PlayableMapResult {
    map: Beatmap,
    mania_score_v2: bool,
    mania_classic: bool,
}

fn playable_map(
    original: &Beatmap,
    mode: Ruleset,
    mods: &GameMods,
    ordered_mods: &[StructuredMod],
) -> Result<PlayableMapResult, ()> {
    let mut map = if original.mode == mode.game_mode() {
        original.clone()
    } else {
        original
            .clone()
            .convert(mode.game_mode(), mods)
            .map_err(|_| ())?
    };
    let mut mania_score_v2 = false;
    let mut mania_classic = false;

    match mode {
        Ruleset::Osu => {
            for structured in ordered_mods {
                let parsed = game_mods(mode, std::slice::from_ref(structured)).map_err(|_| ())?;
                let GameMods::Lazer(mods) = parsed else {
                    continue;
                };

                for game_mod in mods.iter() {
                    match game_mod {
                        rosu_mods::GameMod::TargetPracticeOsu(settings) => {
                            let seed = settings
                                .seed
                                .map(rosu_pp::osu::seed_to_i32)
                                .expect("random seeds are materialized before map preprocessing");
                            rosu_pp::osu::apply_target_practice(&mut map, seed);
                        }
                        rosu_mods::GameMod::RandomOsu(settings) => {
                            let seed = settings
                                .seed
                                .map(rosu_pp::osu::seed_to_i32)
                                .expect("random seeds are materialized before map preprocessing");
                            rosu_pp::osu::apply_random(
                                &mut map,
                                seed,
                                settings.angle_sharpness.unwrap_or_default() as f32,
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        Ruleset::Taiko => {
            for structured in ordered_mods {
                let parsed = game_mods(mode, std::slice::from_ref(structured)).map_err(|_| ())?;
                let GameMods::Lazer(mods) = parsed else {
                    continue;
                };

                for game_mod in mods.iter() {
                    let _ = rosu_pp::taiko::apply_playable_mod(&mut map, game_mod);
                }
            }
        }
        Ruleset::Mania => {
            for structured in ordered_mods {
                if structured.acronym.eq_ignore_ascii_case("SV2") {
                    mania_score_v2 = true;
                } else if structured.acronym.eq_ignore_ascii_case("CL") {
                    mania_classic = true;
                } else if ["HO", "IN", "MR", "RD"]
                    .iter()
                    .any(|acronym| structured.acronym.eq_ignore_ascii_case(acronym))
                {
                    let parsed =
                        game_mods(mode, std::slice::from_ref(structured)).map_err(|_| ())?;
                    let effects = rosu_pp::mania::apply_map_mods(&mut map, &parsed);
                    mania_score_v2 |= effects.score_v2_active;
                    mania_classic |= effects.classic_active;
                }
            }
        }
        Ruleset::Catch => {}
    }

    let adjusted = ordered_beatmap_difficulty(mode, &map, ordered_mods);
    map.ar = adjusted.approach_rate;
    map.cs = adjusted.circle_size;
    map.od = adjusted.overall_difficulty;
    map.hp = adjusted.drain_rate;

    Ok(PlayableMapResult {
        map,
        mania_score_v2,
        mania_classic,
    })
}

