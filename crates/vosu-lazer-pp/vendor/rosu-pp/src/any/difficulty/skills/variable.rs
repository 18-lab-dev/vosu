use std::collections::VecDeque;

use crate::any::difficulty::object::{HasStartTime, IDifficultyObject};

/// Exact state shared by variable-length strain skills.
#[derive(Clone, Debug)]
pub struct VariableLengthStrain {
    decay_weight: f64,
    max_section_length: f64,
    max_stored_length: f64,
    current_section_peak: f64,
    current_section_begin: f64,
    current_section_end: f64,
    strain_peaks: Vec<StrainPeak>,
    total_length: f64,
    queued_strains: VecDeque<QueuedStrain>,
    object_difficulties: Vec<f64>,
    peaks_finalised: bool,
}

impl VariableLengthStrain {
    pub fn new(decay_weight: f64, max_section_length: u32) -> Self {
        Self {
            decay_weight,
            max_section_length: f64::from(max_section_length),
            max_stored_length: 11.0 / (1.0 - decay_weight),
            current_section_peak: 0.0,
            current_section_begin: 0.0,
            current_section_end: 0.0,
            strain_peaks: Vec::with_capacity(256),
            total_length: 0.0,
            queued_strains: VecDeque::with_capacity(64),
            object_difficulties: Vec::with_capacity(256),
            peaks_finalised: false,
        }
    }

    pub fn decay_weight(&self) -> f64 {
        self.decay_weight
    }

    pub fn max_section_length(&self) -> f64 {
        self.max_section_length
    }

    pub fn object_difficulties(&self) -> &[f64] {
        &self.object_difficulties
    }

    /// Returns the target's live peak snapshot. The first call finalises the
    /// current peak; later processing may append completed peaks but does not
    /// add another live peak.
    pub fn current_strain_peaks(&mut self) -> Vec<StrainPeak> {
        if !self.peaks_finalised {
            let section_length = self.current_section_end - self.current_section_begin;
            self.save_current_peak(section_length);
            self.peaks_finalised = true;
        }

        self.strain_peaks.clone()
    }

    pub fn count_top_weighted_strains(&self, difficulty_value: f64) -> f64 {
        if self.object_difficulties.is_empty() {
            return 0.0;
        }

        let consistent_top_strain = difficulty_value * (1.0 - self.decay_weight);

        if consistent_top_strain == 0.0 {
            return self.object_difficulties.len() as f64;
        }

        self.object_difficulties
            .iter()
            .map(|&strain| 1.1 / (1.0 + (-10.0 * (strain / consistent_top_strain - 0.88)).exp()))
            .sum()
    }

    fn save_current_peak(&mut self, section_length: f64) {
        insert_peak(
            &mut self.strain_peaks,
            StrainPeak::new(self.current_section_peak, section_length),
        );
        self.total_length += section_length;

        while self.total_length > self.max_stored_length * self.max_section_length {
            let Some(peak) = self.strain_peaks.pop() else {
                break;
            };

            self.total_length -= peak.section_length;
        }
    }
}

impl Default for VariableLengthStrain {
    fn default() -> Self {
        Self::new(0.9, 400)
    }
}

/// Behaviour supplied by a concrete skill such as osu! Aim.
pub trait VariableLengthStrainSkill {
    type DifficultyObject<'a>: IDifficultyObject + HasStartTime;
    type DifficultyObjects<'a>: ?Sized;

    fn variable_length_strain(&self) -> &VariableLengthStrain;
    fn variable_length_strain_mut(&mut self) -> &mut VariableLengthStrain;

    fn calculate_initial_strain<'a>(
        &self,
        time: f64,
        current: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) -> f64;

    fn strain_value_at<'a>(
        &mut self,
        current: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) -> f64;

    fn process<'a>(
        &mut self,
        current: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) {
        let start_time = current.start_time();

        let current_strain = if current.idx() == 0 {
            let strain = self.strain_value_at(current, objects);
            let state = self.variable_length_strain_mut();
            state.current_section_begin = start_time;
            state.current_section_end = start_time + state.max_section_length;
            state.current_section_peak = strain;
            strain
        } else {
            self.backfill_peaks(current, objects);
            let strain = self.strain_value_at(current, objects);
            let current_peak = self.variable_length_strain().current_section_peak;

            if strain > current_peak {
                let state = self.variable_length_strain_mut();
                state.queued_strains.clear();
                let section_length = start_time - state.current_section_begin;
                state.save_current_peak(section_length);
                state.current_section_begin = start_time;
                state.current_section_end = start_time + state.max_section_length;
                state.current_section_peak = strain;
            } else {
                let state = self.variable_length_strain_mut();

                while state
                    .queued_strains
                    .back()
                    .is_some_and(|queued| queued.strain_value < strain)
                {
                    state.queued_strains.pop_back();
                }

                state.queued_strains.push_back(QueuedStrain {
                    strain_value: strain,
                    start_time,
                });
            }

            strain
        };

        self.variable_length_strain_mut()
            .object_difficulties
            .push(current_strain);
    }

    fn backfill_peaks<'a>(
        &mut self,
        current: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) {
        let current_start_time = current.start_time();

        while current_start_time > self.variable_length_strain().current_section_end {
            let (section_begin, section_end, max_section_length) = {
                let state = self.variable_length_strain();
                (
                    state.current_section_begin,
                    state.current_section_end,
                    state.max_section_length,
                )
            };

            {
                let state = self.variable_length_strain_mut();
                state.save_current_peak(section_end - section_begin);
                state.current_section_begin = section_end;
            }

            let queued = self.variable_length_strain_mut().queued_strains.pop_front();

            if let Some(queued) = queued {
                self.variable_length_strain_mut().current_section_end =
                    queued.start_time + max_section_length;

                let section_begin = self.variable_length_strain().current_section_begin;
                let initial = self.calculate_initial_strain(section_begin, current, objects);
                self.variable_length_strain_mut().current_section_peak =
                    initial.max(queued.strain_value);
            } else {
                let section_begin = self.variable_length_strain().current_section_begin;
                self.variable_length_strain_mut().current_section_end =
                    section_begin + max_section_length;
                let initial = self.calculate_initial_strain(section_begin, current, objects);
                self.variable_length_strain_mut().current_section_peak = initial;
            }
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StrainPeak {
    pub value: f64,
    pub section_length: f64,
}

impl StrainPeak {
    pub fn new(value: f64, section_length: f64) -> Self {
        Self {
            value,
            // C# Math.Round(double) is midpoint-to-even.
            section_length: section_length.round_ties_even(),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct QueuedStrain {
    strain_value: f64,
    start_time: f64,
}

fn insert_peak(peaks: &mut Vec<StrainPeak>, peak: StrainPeak) {
    // StrainPeak.CompareTo in the target is reverse-by-value. Insert after
    // equal values; equal-valued order cannot affect any target summation.
    let index = peaks.partition_point(|existing| existing.value >= peak.value);
    peaks.insert(index, peak);
}

#[cfg(test)]
mod tests {
    use super::VariableLengthStrain;

    #[test]
    fn current_peaks_finalise_live_section_only_once() {
        let mut strain = VariableLengthStrain::new(0.9, 400);
        strain.current_section_begin = 100.0;
        strain.current_section_end = 500.0;
        strain.current_section_peak = 7.0;

        let first = strain.current_strain_peaks();
        let repeated = strain.current_strain_peaks();

        assert_eq!(first.len(), 1);
        assert_eq!(repeated, first);
        assert_eq!(first[0].value, 7.0);
        assert_eq!(first[0].section_length, 400.0);

        // Processing is allowed to append subsequently completed sections, but
        // querying again must not reinsert the current live section.
        strain.current_section_peak = 3.0;
        strain.save_current_peak(250.0);

        let after_completed_section = strain.current_strain_peaks();

        assert_eq!(after_completed_section.len(), 2);
        assert_eq!(after_completed_section[0].value, 7.0);
        assert_eq!(after_completed_section[1].value, 3.0);
        assert_eq!(after_completed_section[1].section_length, 250.0);
    }

    #[test]
    fn top_weighted_strains_only_short_circuit_at_exact_zero() {
        let mut strain = VariableLengthStrain::new(0.9, 400);
        strain.object_difficulties.extend([1.0, 0.5]);

        assert_eq!(strain.count_top_weighted_strains(0.0), 2.0);
        assert_eq!(strain.count_top_weighted_strains(1.0e-15), 2.2);
    }
}
