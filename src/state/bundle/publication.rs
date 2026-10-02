use super::*;

impl StateBundle {
    pub fn snapshot(&self) -> Arc<StateBundleSnapshot> {
        Arc::new(StateBundleSnapshot {
            source: self.source,
            generation: self.generation,
            reset_epoch: self.reset_epoch,
            regular: self
                .published_regular
                .clone()
                .unwrap_or_else(|| Arc::new(self.build_regular_state())),
            precise: self
                .published_precise
                .clone()
                .unwrap_or_else(|| Arc::new(self.build_precise_state(None))),
        })
    }

    fn regular_matches_current(&self, state: &RegularState) -> bool {
        state.global == self.global
            && state.settings == self.settings
            && state.beatmap == self.beatmap
            && state.menu == self.menu
            && state.bass_density == self.bass_density
            && state.gameplay == self.gameplay
            && state.result_screen == self.result_screen
            && state.tourney == self.tourney
            && state.user == self.user
            && state.lazer_multi_spectating == self.lazer_multi_spectating
            && state.ranked_play == self.ranked_play
            && state.room == self.room
    }

    fn precise_matches_current(&self, state: &PreciseState) -> bool {
        state.status == self.global.status
            && state.play_time == self.global.play_time
            && state.mode == self.gameplay.keypress_mode
            && state.key_overlay.as_ref() == &self.gameplay.key_overlay
            && state.keypresses.as_ref() == &self.gameplay.keypresses
            && state.hit_errors.as_ref() == &self.gameplay.hit_errors
            && state.total_hit_errors == self.gameplay.total_hit_errors
            && state.committed_hit_error_index == self.gameplay.previous_hit_error_index
            && state.unstable_rate == self.gameplay.unstable_rate
    }

    pub(super) fn materialize_current_reusing(
        &self,
        prior: Option<&StateBundleSnapshot>,
    ) -> Arc<StateBundleSnapshot> {
        let regular = prior
            .map(|snapshot| &snapshot.regular)
            .filter(|state| self.regular_matches_current(state))
            .cloned()
            .or_else(|| {
                self.published_regular
                    .as_ref()
                    .filter(|state| self.regular_matches_current(state))
                    .cloned()
            })
            .unwrap_or_else(|| Arc::new(self.build_regular_state()));
        let prior_precise = prior
            .map(|snapshot| snapshot.precise.as_ref())
            .or(self.published_precise.as_deref());
        let precise = prior
            .map(|snapshot| &snapshot.precise)
            .filter(|state| self.precise_matches_current(state))
            .cloned()
            .or_else(|| {
                self.published_precise
                    .as_ref()
                    .filter(|state| self.precise_matches_current(state))
                    .cloned()
            })
            .unwrap_or_else(|| Arc::new(self.build_precise_state(prior_precise)));
        Arc::new(StateBundleSnapshot {
            source: self.source,
            generation: self.generation,
            reset_epoch: self.reset_epoch,
            regular,
            precise,
        })
    }

    pub(super) fn materialize_current(&self) -> Arc<StateBundleSnapshot> {
        self.materialize_current_reusing(None)
    }

    pub(super) fn build_regular_state(&self) -> RegularState {
        RegularState {
            global: self.global.clone(),
            settings: self.settings.clone(),
            beatmap: self.beatmap.clone(),
            menu: self.menu.clone(),
            bass_density: self.bass_density.clone(),
            gameplay: self.gameplay.clone(),
            result_screen: self.result_screen.clone(),
            tourney: self.tourney.clone(),
            user: self.user.clone(),
            lazer_multi_spectating: self.lazer_multi_spectating.clone(),
            ranked_play: self.ranked_play.clone(),
            room: self.room.clone(),
        }
    }

    pub(super) fn build_precise_state(&self, prior: Option<&PreciseState>) -> PreciseState {
        let key_overlay = prior
            .filter(|state| state.key_overlay.as_ref() == &self.gameplay.key_overlay)
            .map_or_else(
                || Arc::new(self.gameplay.key_overlay.clone()),
                |state| Arc::clone(&state.key_overlay),
            );
        let keypresses = prior
            .filter(|state| state.keypresses.as_ref() == &self.gameplay.keypresses)
            .map_or_else(
                || Arc::new(self.gameplay.keypresses.clone()),
                |state| Arc::clone(&state.keypresses),
            );
        let hit_errors = prior
            .filter(|state| state.hit_errors.as_ref() == &self.gameplay.hit_errors)
            .map_or_else(
                || Arc::new(self.gameplay.hit_errors.clone()),
                |state| Arc::clone(&state.hit_errors),
            );

        PreciseState {
            status: self.global.status,
            play_time: self.global.play_time,
            mode: self.gameplay.keypress_mode,
            key_overlay,
            keypresses,
            hit_errors,
            total_hit_errors: self.gameplay.total_hit_errors,
            committed_hit_error_index: self.gameplay.previous_hit_error_index,
            unstable_rate: self.gameplay.unstable_rate,
        }
    }

    pub(super) fn publish_regular_state(&mut self) -> Arc<StateBundleSnapshot> {
        let next = self.build_regular_state();
        if self.published_regular.as_deref() != Some(&next) {
            self.published_regular = Some(Arc::new(next));
        }
        if self.precise_dirty || self.published_precise.is_none() {
            let next = self.build_precise_state(self.published_precise.as_deref());
            self.published_precise = Some(Arc::new(next));
        }
        self.precise_dirty = false;
        self.snapshot()
    }

    pub(super) fn publish_precise_state(&mut self) -> Arc<StateBundleSnapshot> {
        if self.published_regular.is_none() {
            self.published_regular = Some(Arc::new(self.build_regular_state()));
        }
        let next = self.build_precise_state(self.published_precise.as_deref());
        self.published_precise = Some(Arc::new(next));
        self.precise_dirty = false;
        self.snapshot()
    }
}
