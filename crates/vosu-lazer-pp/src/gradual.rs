pub struct GradualDifficulty {
    inner: RosuGradualDifficulty,
    difficulty: Difficulty,
    map: Beatmap,
    mode: Ruleset,
    context: Arc<RwLock<PlayBeatmapContext>>,
    processed: usize,
    object_end_times: Vec<f64>,
    current: Option<DifficultyAttributes>,
}

impl GradualDifficulty {
    fn new(map: &ParsedBeatmap, config: &DifficultyConfig) -> Result<Self, Error> {
        let difficulty = build_difficulty(map, config)?;
        let input = gradual_calculation_map(map, config)?;
        let inner = RosuGradualDifficulty::new_with_mode(
            difficulty.clone(),
            input.as_ref(),
            map.mode.game_mode(),
        )
        .map_err(|_| Error::Convert {
            from: Ruleset::from_game_mode(input.mode),
            to: map.mode,
        })?;
        let object_end_times = match &inner {
            RosuGradualDifficulty::Osu(gradual) => gradual.object_end_times().to_vec(),
            RosuGradualDifficulty::Taiko(gradual) => gradual.object_end_times().to_vec(),
            RosuGradualDifficulty::Catch(gradual) => gradual.object_end_times().to_vec(),
            RosuGradualDifficulty::Mania(gradual) => gradual.object_end_times().collect(),
        };
        let context = if config.mods.is_empty() {
            map.context.clone()
        } else {
            Arc::new(RwLock::new(PlayBeatmapContext {
                mods: difficulty.get_mods().clone(),
                classic: config_classic(map, config),
            }))
        };

        Ok(Self {
            inner,
            difficulty,
            map: input.into_owned(),
            mode: map.mode(),
            context,
            processed: 0,
            object_end_times,
            current: None,
        })
    }

    pub fn processed_objects(&self) -> usize {
        self.processed
    }

    pub fn current(&self) -> Option<&DifficultyAttributes> {
        self.current.as_ref()
    }

    fn evaluate_current(&mut self) -> &DifficultyAttributes {
        self.current.get_or_insert_with(|| self.inner.attributes())
    }

    pub fn advance(&mut self) -> bool {
        if !self.inner.advance() {
            return false;
        }

        self.processed += 1;
        self.current = None;

        true
    }

    pub fn performance(&mut self, state: LiveScoreState) -> Result<PerformanceAttributes, Error> {
        let context = self
            .context
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let attrs = self.evaluate_current().clone();
        let performance = Performance::new(attrs)
            .mods(context.mods)
            .state(performance_score_state(self.mode, state))
            .lazer(!context.classic);
        let performance = if self.mode == Ruleset::Osu {
            performance.accuracy(state.accuracy * 100.0)
        } else {
            performance
        };

        Ok(performance.calculate())
    }

    pub fn create_difficulty_attrs(&mut self) -> Result<DifficultyAttrsData, Error> {
        let attrs = self.inner.attributes();
        self.current = Some(attrs.clone());

        Ok(DifficultyAttrsData::from(&attrs))
    }

    pub fn calculate_progressive_accuracy(&mut self, score: ScoreInfoData) -> Result<f64, Error> {
        let mode = self.mode;
        let classic = self
            .context
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .classic;
        let totals = ScoreTotals::from_difficulty(self.evaluate_current());

        Ok(calculate_accuracy(mode, &score, classic, totals))
    }

    pub fn create_progressive_score(
        &mut self,
        target_accuracy: f64,
    ) -> Result<ScoreInfoData, Error> {
        let mode = self.mode;
        let classic = self
            .context
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .classic;
        let totals = ScoreTotals::from_difficulty(self.evaluate_current());

        simulate_score(mode, totals, target_accuracy, classic)
    }

    pub fn get_current_strains(&self) -> Result<StrainsData, Error> {
        if self.mode == Ruleset::Osu && self.processed < self.object_end_times.len() {
            return Err(Error::IncompleteOsuStrains);
        }

        let strains = match &self.inner {
            RosuGradualDifficulty::Osu(gradual) => Strains::Osu(gradual.current_strains()),
            RosuGradualDifficulty::Taiko(gradual) => Strains::Taiko(gradual.current_strains()),
            RosuGradualDifficulty::Catch(gradual) => Strains::Catch(gradual.current_strains()),
            RosuGradualDifficulty::Mania(gradual) => Strains::Mania(gradual.current_strains()),
        };
        let mut data = StrainsData::from(strains);

        if self.mode == Ruleset::Osu && !self.difficulty.get_mods().fl() {
            data.flashlight = PeakStrains::default();
        }

        Ok(data)
    }

    pub fn performance_from_score(
        &mut self,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        let mode = self.mode;
        let context = self
            .context
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        if use_signed_performance(mode, &score) {
            let attrs = self.evaluate_current().clone();

            return Ok(signed_performance_data(
                &attrs,
                &context.mods,
                context.classic,
                &score,
            ));
        }

        let attrs = self.performance(score_to_live(mode, score)?)?;

        Ok(PerformanceAttrsData::from(&attrs))
    }

    pub fn skip(&mut self, count: i32) {
        let added = self.inner.skip_batch(count.max(0) as usize);

        if added > 0 {
            self.processed += added;
            self.current = None;
        }
    }

    pub fn skip_to_time(&mut self, time: f64) -> usize {
        let count = self.object_end_times[self.processed..]
            .iter()
            // Match the source loop's exact negation semantics. In
            // particular, `end_time > NaN` is false, so NaN skips every
            // remaining object rather than none.
            .take_while(|&&end_time| {
                end_time.partial_cmp(&time) != Some(std::cmp::Ordering::Greater)
            })
            .count();
        let added = self.inner.skip_batch(count);

        if added > 0 {
            self.processed += added;
            self.current = None;
        }

        added
    }

    pub fn skip_to_end(&mut self) {
        let added = self.inner.skip_batch_to_end();

        if added > 0 {
            self.processed += added;
            self.current = None;
        }
    }

    pub fn rewind(&mut self) {
        self.inner = RosuGradualDifficulty::new_with_mode(
            self.difficulty.clone(),
            &self.map,
            self.mode.game_mode(),
        )
        .expect("the gradual source was successfully prepared before");
        self.processed = 0;
        self.current = None;
    }
}

