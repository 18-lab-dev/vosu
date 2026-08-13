use crate::{
    BeatmapState, DifficultyAttrsData, DifficultyConfig, Error, LiveLazerEngine, LiveScoreState,
    ParsedBeatmap, PerformanceAttrsData, ScoreInfoData, ScoreTotals, StrainsData,
    accuracy_with_score_totals, configured,
};
use rosu_pp::any::{DifficultyAttributes, PerformanceAttributes};
use std::sync::{Arc, RwLock};
pub struct GradualDifficulty {
    shared: Arc<RwLock<BeatmapState>>,
    config: DifficultyConfig,
    core: rosu_pp::GradualDifficulty,
    core_map: rosu_pp::Beatmap,
    core_difficulty: rosu_pp::Difficulty,
    processed: usize,
    current: Option<DifficultyAttributes>,
    ends: Vec<f64>,
}
impl GradualDifficulty {
    fn map(&self) -> ParsedBeatmap {
        ParsedBeatmap::from_state(
            self.shared
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        )
    }

    fn reset_core(&mut self) {
        self.core = rosu_pp::GradualDifficulty::new(self.core_difficulty.clone(), &self.core_map);
    }

    fn refresh_current(&mut self) {
        self.current = self.core.attributes();
    }

    fn process_n(&mut self, count: usize) {
        if count == 0 {
            return;
        }

        self.core.skip_batch(count);
        self.processed += count;
        self.refresh_current();
    }

    pub fn advance(&mut self) -> bool {
        if !self.core.advance() {
            return false;
        }

        self.processed += 1;
        self.refresh_current();
        true
    }

    pub fn skip(&mut self, count: i32) -> usize {
        let before = self.processed;

        if count >= 0 {
            let count = (count as usize).min(self.ends.len().saturating_sub(self.processed));
            self.process_n(count);
        } else {
            let target = self.processed.saturating_sub(count.unsigned_abs() as usize);
            self.reset_core();
            self.processed = 0;
            self.current = None;
            self.process_n(target);
        }

        self.processed.abs_diff(before)
    }

    pub fn skip_to_end(&mut self) -> usize {
        let count = self.ends.len().saturating_sub(self.processed);
        if count != 0 {
            self.core.skip_batch_to_end();
            self.processed += count;
            self.refresh_current();
        }
        count
    }

    pub fn skip_to_time(&mut self, time: f64) -> usize {
        let before = self.processed;
        let target = if time.is_nan() {
            self.ends.len()
        } else {
            self.processed
                + self.ends[self.processed..]
                    .iter()
                    .take_while(|end| **end <= time)
                    .count()
        };

        self.process_n(target.saturating_sub(self.processed));
        self.processed - before
    }

    pub fn rewind(&mut self) {
        self.reset_core();
        self.processed = 0;
        self.current = None;
    }

    pub const fn processed_objects(&self) -> usize {
        self.processed
    }

    pub fn current(&self) -> Option<&DifficultyAttributes> {
        self.current.as_ref()
    }
    pub fn create_difficulty_attrs(&mut self) -> Result<DifficultyAttrsData, Error> {
        if self.processed == 0 {
            let map = self.map();
            return Ok(DifficultyAttrsData {
                mode: map.mode(),
                ..Default::default()
            });
        }

        self.current
            .clone()
            .map(|inner| {
                let mut attrs = DifficultyAttrsData::from_inner(inner);
                attrs.context = Some(self.core_difficulty.clone());
                attrs
            })
            .ok_or(Error::DifficultyUnavailable)
    }
    pub fn get_current_strains(&self) -> Result<StrainsData, Error> {
        let map = self.map();
        if map.mode() == crate::Ruleset::Osu && self.processed < self.ends.len() {
            return Err(Error::DifficultyUnavailable);
        }

        Ok(self.core.current_strains().into())
    }
    pub fn performance(&mut self, state: LiveScoreState) -> Result<PerformanceAttributes, Error> {
        let map = self.map();
        let score = ScoreInfoData::from(state);
        let attrs = self.create_difficulty_attrs()?;
        LiveLazerEngine.performance_inner(
            &map,
            &DifficultyConfig {
                passed_objects: Some(self.processed as u32),
                ..self.config.clone()
            },
            Some(attrs.inner_for(map.mode())?),
            score,
        )
    }
    pub fn performance_from_score(
        &mut self,
        score: ScoreInfoData,
    ) -> Result<PerformanceAttrsData, Error> {
        let map = self.map();
        let attrs = self.create_difficulty_attrs()?;
        LiveLazerEngine.performance_data_from_attrs(
            &map,
            &DifficultyConfig {
                passed_objects: Some(self.processed as u32),
                ..self.config.clone()
            },
            &attrs,
            score,
        )
    }
    pub fn create_progressive_score(&mut self, target: f64) -> Result<ScoreInfoData, Error> {
        let map = self.map();
        let attrs = self.create_difficulty_attrs()?;
        crate::simulate_score(
            map.mode(),
            ScoreTotals::from_data(&attrs),
            target,
            map.classic_scoring(),
        )
    }
    pub fn calculate_progressive_accuracy(&self, score: ScoreInfoData) -> Result<f64, Error> {
        let map = self.map();
        let attrs = match self.current.clone() {
            Some(inner) => DifficultyAttrsData::from_inner(inner),
            None if self.processed == 0 => DifficultyAttrsData {
                mode: map.mode(),
                ..DifficultyAttrsData::default()
            },
            None => return Err(Error::DifficultyUnavailable),
        };
        accuracy_with_score_totals(
            map.mode(),
            score,
            map.classic_scoring(),
            ScoreTotals::from_data(&attrs),
        )
    }
}
impl LiveLazerEngine {
    pub fn gradual_difficulty(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<GradualDifficulty, Error> {
        let (_, core_difficulty, core_map) = configured(
            map,
            &DifficultyConfig {
                passed_objects: None,
                ..config.clone()
            },
        )?;
        let core = rosu_pp::GradualDifficulty::new(core_difficulty.clone(), &core_map);
        let ends = core.object_end_times().to_vec();

        Ok(GradualDifficulty {
            shared: Arc::clone(&map.shared),
            config: config.clone(),
            core,
            core_map,
            core_difficulty,
            processed: 0,
            current: None,
            ends,
        })
    }
}
