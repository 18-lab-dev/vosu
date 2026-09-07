use crate::{
    Difficulty,
    any::{HitResultGenerator, HitResultPriority},
    mania::{Mania, ManiaDifficultyAttributes, ManiaHitResults},
};

use super::*;

/// Helper function to verify that a result is truly the closest to target accuracy.
/// Tests neighboring states to ensure none are closer.
fn verify_is_closest(inspect: &InspectManiaPerformance, result: &ManiaHitResults) {
    let is_classic = inspect.is_classic();
    let actual_acc = result.accuracy(is_classic);
    let target_acc = inspect.acc.unwrap();
    let current_dist = (actual_acc - target_acc).abs();

    let total = result.total_hits();

    // Test all possible single-step variations
    let variations = [
        // Increase n320, decrease others
        (1, 0, 0, 0, -1),
        (1, -1, 0, 0, 0),
        (1, 0, -1, 0, 0),
        (1, 0, 0, -1, 0),
        // Increase n300, decrease others
        (0, 1, 0, 0, -1),
        (0, 1, -1, 0, 0),
        (0, 1, 0, -1, 0),
        (-1, 1, 0, 0, 0),
        // Increase n200, decrease others
        (0, 0, 1, 0, -1),
        (0, 0, 1, -1, 0),
        (0, -1, 1, 0, 0),
        (-1, 0, 1, 0, 0),
        // Increase n100, decrease others
        (0, 0, 0, 1, -1),
        (0, 0, -1, 1, 0),
        (0, -1, 0, 1, 0),
        (-1, 0, 0, 1, 0),
        // Decrease n320, increase others
        (-1, 1, 0, 0, 0),
        (-1, 0, 1, 0, 0),
        (-1, 0, 0, 1, 0),
        (-1, 0, 0, 0, 1),
    ];

    for (d320, d300, d200, d100, d50) in variations {
        let new320 = (result.n320 as i32 + d320).max(0) as u32;
        let new300 = (result.n300 as i32 + d300).max(0) as u32;
        let new200 = (result.n200 as i32 + d200).max(0) as u32;
        let new100 = (result.n100 as i32 + d100).max(0) as u32;
        let new50 = (result.n50 as i32 + d50).max(0) as u32;

        // Skip if total doesn't match
        if new320 + new300 + new200 + new100 + new50 != total {
            continue;
        }

        // Skip if this violates user constraints
        if let Some(n) = inspect.n320 {
            if new320 != n {
                continue;
            }
        }
        if let Some(n) = inspect.n300 {
            if new300 != n {
                continue;
            }
        }
        if let Some(n) = inspect.n200 {
            if new200 != n {
                continue;
            }
        }
        if let Some(n) = inspect.n100 {
            if new100 != n {
                continue;
            }
        }
        if let Some(n) = inspect.n50 {
            if new50 != n {
                continue;
            }
        }

        let neighbor = ManiaHitResults {
            n320: new320,
            n300: new300,
            n200: new200,
            n100: new100,
            n50: new50,
            misses: result.misses,
        };

        let neighbor_acc = neighbor.accuracy(is_classic);
        let neighbor_dist = (neighbor_acc - target_acc).abs();

        assert!(
            current_dist <= neighbor_dist + 1e-10,
            "Found closer neighbor! \
            Current: {result:?} (acc={actual_acc}, dist={current_dist}), \
            Neighbor: {neighbor:?} (acc={neighbor_acc}, dist={neighbor_dist})",
        );
    }
}

#[test]
fn test_perfect_accuracy() {
    const N_OBJECTS: u32 = 1000;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(1.0),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(0),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n320, N_OBJECTS);
    assert_eq!(result.n300, 0);
    assert_eq!(result.n200, 0);
    assert_eq!(result.n100, 0);
    assert_eq!(result.n50, 0);
    assert_eq!(result.misses, 0);
    assert_eq!(result.accuracy(false), 1.0);
}

#[test]
fn test_all_missing_high_accuracy() {
    const N_OBJECTS: u32 = 500;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.95),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(10),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 10);

    let actual_acc = result.accuracy(false);
    assert!(
        (actual_acc - 0.95).abs() < 0.001,
        "Expected ~0.95, got {actual_acc}"
    );

    verify_is_closest(&inspect, &result);
}

#[test]
fn test_all_missing_medium_accuracy() {
    const N_OBJECTS: u32 = 300;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.75),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(15),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 15);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_all_missing_low_accuracy() {
    const N_OBJECTS: u32 = 200;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.55),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(20),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 20);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_classic_mode() {
    const N_OBJECTS: u32 = 400;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new().lazer(false),
        acc: Some(0.88),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(8),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 8);

    // Verify it uses classic scoring (weight 60 for 320s)
    let actual_acc = result.accuracy(true);
    assert!(
        (actual_acc - 0.88).abs() < 0.001,
        "Expected ~0.88, got {actual_acc}"
    );

    verify_is_closest(&inspect, &result);
}

#[test]
fn test_one_missing_n50() {
    const N_OBJECTS: u32 = 250;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.92),
        n320: Some(180),
        n300: Some(30),
        n200: Some(15),
        n100: Some(10),
        n50: None,
        misses: Some(5),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n320, 180);
    assert_eq!(result.n300, 30);
    assert_eq!(result.n200, 15);
    assert_eq!(result.n100, 10);
    assert_eq!(result.n50, 10); // N_OBJECTS - others
    assert_eq!(result.misses, 5);
    assert_eq!(result.total_hits(), N_OBJECTS);
}

#[test]
fn test_two_missing_n320_n300() {
    const N_OBJECTS: u32 = 180;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.85),
        n320: None,
        n300: None,
        n200: Some(25),
        n100: Some(20),
        n50: Some(15),
        misses: Some(10),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n200, 25);
    assert_eq!(result.n100, 20);
    assert_eq!(result.n50, 15);
    assert_eq!(result.misses, 10);
    assert_eq!(result.total_hits(), N_OBJECTS);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_two_missing_n100_n50() {
    const N_OBJECTS: u32 = 150;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.90),
        n320: Some(80),
        n300: Some(40),
        n200: Some(15),
        n100: None,
        n50: None,
        misses: Some(5),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n320, 80);
    assert_eq!(result.n300, 40);
    assert_eq!(result.n200, 15);
    assert_eq!(result.misses, 5);
    assert_eq!(result.total_hits(), N_OBJECTS);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_three_missing() {
    const N_OBJECTS: u32 = 220;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.78),
        n320: Some(100),
        n300: None,
        n200: None,
        n100: None,
        n50: Some(30),
        misses: Some(12),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n320, 100);
    assert_eq!(result.n50, 30);
    assert_eq!(result.misses, 12);
    assert_eq!(result.total_hits(), N_OBJECTS);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_four_missing() {
    const N_OBJECTS: u32 = 280;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.82),
        n320: None,
        n300: None,
        n200: Some(45),
        n100: None,
        n50: None,
        misses: Some(18),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n200, 45);
    assert_eq!(result.misses, 18);
    assert_eq!(result.total_hits(), N_OBJECTS);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_all_provided() {
    const N_OBJECTS: u32 = 100;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.85), // Accuracy doesn't matter when all are provided
        n320: Some(50),
        n300: Some(20),
        n200: Some(15),
        n100: Some(8),
        n50: Some(5),
        misses: Some(2),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n320, 50);
    assert_eq!(result.n300, 20);
    assert_eq!(result.n200, 15);
    assert_eq!(result.n100, 8);
    assert_eq!(result.n50, 5);
    assert_eq!(result.misses, 2);
}

#[test]
fn test_clamping_when_exceed_remain() {
    const N_OBJECTS: u32 = 100;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.90),
        n320: Some(200), // Exceeds total
        n300: Some(50),
        n200: Some(30),
        n100: Some(20),
        n50: Some(10),
        misses: Some(5),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    // Should clamp to total_hits
    assert_eq!(result.total_hits(), N_OBJECTS);
    assert!(result.n320 <= 95); // N_OBJECTS - misses
}

#[test]
fn test_all_misses() {
    const N_OBJECTS: u32 = 50;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.0),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(N_OBJECTS),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.n320, 0);
    assert_eq!(result.n300, 0);
    assert_eq!(result.n200, 0);
    assert_eq!(result.n100, 0);
    assert_eq!(result.n50, 0);
    assert_eq!(result.misses, N_OBJECTS);
}

#[test]
fn test_edge_case_very_high_accuracy() {
    const N_OBJECTS: u32 = 350;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.9875),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(2),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 2);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_edge_case_very_low_accuracy() {
    const N_OBJECTS: u32 = 120;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.25),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(30),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 30);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_small_map() {
    const N_OBJECTS: u32 = 20;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.80),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(3),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 3);
    verify_is_closest(&inspect, &result);
}

#[test]
fn test_large_map() {
    const N_OBJECTS: u32 = 5000;

    let inspect = InspectManiaPerformance {
        attrs: &ManiaDifficultyAttributes {
            n_objects: N_OBJECTS,
            ..Default::default()
        },
        difficulty: &Difficulty::new(),
        acc: Some(0.87),
        n320: None,
        n300: None,
        n200: None,
        n100: None,
        n50: None,
        misses: Some(42),
        hitresult_priority: HitResultPriority::BestCase,
    };

    let result = <Closest as HitResultGenerator<Mania>>::generate_hitresults(inspect.clone());

    assert_eq!(result.total_hits(), N_OBJECTS);
    assert_eq!(result.misses, 42);

    let actual_acc = result.accuracy(false);
    assert!(
        (actual_acc - 0.87).abs() < 0.001,
        "Expected ~0.87, got {actual_acc}"
    );
}
