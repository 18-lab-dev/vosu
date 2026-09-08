use rosu_pp::any::{DifficultyAttributes, Strains};

use crate::{
    DifficultyAttrsData, DifficultyConfig, Error, LiveLazerEngine, ParsedBeatmap, StrainsData,
    configured_difficulty,
};

impl LiveLazerEngine {
    fn full_core(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<(rosu_pp::Difficulty, rosu_pp::GradualDifficulty), Error> {
        let (difficulty, playable) = configured_difficulty(map, config)?;
        let core = rosu_pp::GradualDifficulty::new(difficulty.clone(), &playable);
        Ok((difficulty, core))
    }
    fn finish_core(core: &mut rosu_pp::GradualDifficulty, passed_objects: Option<u32>) {
        if let Some(passed_objects) = passed_objects {
            let count = (passed_objects as usize).min(core.object_end_times().len());
            core.skip_batch(count);
        } else {
            core.skip_batch_to_end();
        }
    }

    pub fn difficulty(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<DifficultyAttributes, Error> {
        let (_, mut core) = self.full_core(map, config)?;
        Self::finish_core(&mut core, config.passed_objects);
        core.attributes().ok_or(Error::DifficultyUnavailable)
    }

    pub fn difficulty_data(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<DifficultyAttrsData, Error> {
        let (difficulty, mut core) = self.full_core(map, config)?;
        Self::finish_core(&mut core, config.passed_objects);
        let inner = core.attributes().ok_or(Error::DifficultyUnavailable)?;
        let mut attrs = DifficultyAttrsData::from_inner(inner);
        attrs.context = Some(difficulty);
        Ok(attrs)
    }

    /// Calculate attributes and strains from one completed difficulty traversal.
    pub fn difficulty_and_strains_data(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<(DifficultyAttrsData, StrainsData), Error> {
        let (difficulty, mut core) = self.full_core(map, config)?;
        Self::finish_core(&mut core, config.passed_objects);
        let inner = core.attributes().ok_or(Error::DifficultyUnavailable)?;
        let mut attrs = DifficultyAttrsData::from_inner(inner);
        attrs.context = Some(difficulty);
        Ok((attrs, core.current_strains().into()))
    }

    pub fn strains(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<Strains, Error> {
        let (_, mut core) = self.full_core(map, config)?;
        Self::finish_core(&mut core, config.passed_objects);
        Ok(core.current_strains())
    }
    pub fn strains_data(
        &self,
        map: &ParsedBeatmap,
        config: &DifficultyConfig,
    ) -> Result<StrainsData, Error> {
        self.strains(map, config).map(Into::into)
    }
}
