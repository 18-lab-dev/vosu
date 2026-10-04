const MAX_BANANAS: usize = 100_000;

pub struct BananaShower {
    pub n_bananas: usize,
}

impl BananaShower {
    pub fn new(start_time: f64, end_time: f64) -> Self {
        // * Int truncation added to match osu!stable.
        let start_time = start_time as i32;
        let end_time = end_time as i32;
        let mut spacing = (i64::from(end_time) - i64::from(start_time)) as f32;

        while spacing > 100.0 {
            spacing /= 2.0;
        }

        let n_bananas = if spacing <= 0.0 {
            0
        } else {
            let end_time = end_time as f32;
            let mut time = start_time as f32;
            let mut count = 0;

            while time <= end_time {
                if count == MAX_BANANAS {
                    break;
                }

                let next_time = time + spacing;
                count += 1;

                if next_time <= time {
                    break;
                }

                time = next_time;
            }

            count
        };

        Self { n_bananas }
    }
}

#[cfg(test)]
mod tests {
    use super::BananaShower;

    #[test]
    fn banana_count_preserves_normal_spacing() {
        assert_eq!(BananaShower::new(0.0, 250.0).n_bananas, 5);
    }

    #[test]
    fn large_timestamp_with_tiny_spacing_terminates() {
        assert_eq!(
            BananaShower::new(1_073_741_824.0, 1_073_741_825.0).n_bananas,
            1
        );
    }
    #[test]
    fn huge_interval_respects_banana_budget() {
        assert_eq!(
            BananaShower::new(0.0, i32::MAX as f64).n_bananas,
            super::MAX_BANANAS
        );
        // This span exceeds i32 subtraction, but its initial f32 timestamp
        // cannot advance at the derived spacing and must stop immediately.
        assert_eq!(
            BananaShower::new(i32::MIN as f64, i32::MAX as f64).n_bananas,
            1
        );
    }
}
