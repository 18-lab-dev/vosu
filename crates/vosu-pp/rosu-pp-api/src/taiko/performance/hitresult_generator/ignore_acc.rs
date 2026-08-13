use std::{cmp, mem};

use crate::{
    any::{HitResultGenerator, HitResultPriority, hitresult_generator::IgnoreAccuracy},
    taiko::{InspectTaikoPerformance, Taiko, TaikoHitResults},
};

impl HitResultGenerator<Taiko> for IgnoreAccuracy {
    fn generate_hitresults(inspect: InspectTaikoPerformance<'_>) -> TaikoHitResults {
        let total_hits = inspect.total_hits();
        let misses = inspect.misses();
        let mut remain = total_hits - misses;

        // Helper to assign a specified value
        let mut assign_specified = |specified: Option<u32>| -> Option<u32> {
            let assigned = cmp::min(specified?, remain);
            remain -= assigned;

            Some(assigned)
        };

        let (n300, n100, n50) = match inspect.hitresult_priority {
            HitResultPriority::BestCase => {
                let n300 = assign_specified(inspect.n300);
                let n100 = assign_specified(inspect.n100);
                let n50 = assign_specified(inspect.n50);

                let mut n300 = n300.unwrap_or_else(|| mem::replace(&mut remain, 0));
                let n100 = n100.unwrap_or_else(|| mem::replace(&mut remain, 0));
                let n50 = n50.unwrap_or_else(|| mem::replace(&mut remain, 0));

                if remain > 0 {
                    n300 += remain;
                }

                (n300, n100, n50)
            }
            HitResultPriority::WorstCase => {
                let n50 = assign_specified(inspect.n50);
                let n100 = assign_specified(inspect.n100);
                let n300 = assign_specified(inspect.n300);

                let mut n50 = n50.unwrap_or_else(|| mem::replace(&mut remain, 0));
                let n100 = n100.unwrap_or_else(|| mem::replace(&mut remain, 0));
                let n300 = n300.unwrap_or_else(|| mem::replace(&mut remain, 0));

                if remain > 0 {
                    n50 += remain;
                }

                (n300, n100, n50)
            }
        };

        TaikoHitResults {
            n300,
            n100,
            n50,
            misses,
        }
    }
}
