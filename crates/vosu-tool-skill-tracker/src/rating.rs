use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SKILLS: [&str; 8] = [
    "overall",
    "stream",
    "jumpstream",
    "handstream",
    "stamina",
    "jackspeed",
    "chordjack",
    "technical",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Wife {
    pub value: f64,
    pub model: String,
}

// Port of Etterna 0.75.1 RageUtil.h wife3 at J4; upstream seconds become ms here.
pub fn wife3(ms: f64) -> f64 {
    let ms = ms.abs();
    if ms <= 5.0 {
        2.0
    } else if ms <= 65.0 {
        let x = (65.0 - ms) / 22.7;
        let t = 1.0 / (1.0 + 0.3275911 * x);
        2.0 * (1.0
            - (((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
                + 0.254829592)
                * t)
                * (-x * x).exp())
    } else if ms <= 180.0 {
        (ms - 65.0) * -5.5 / 115.0
    } else {
        -5.5
    }
}

pub fn count(hits: &Value, key: &str) -> u64 {
    hits[key].as_u64().unwrap_or(0)
}
pub fn total(hits: &Value) -> u64 {
    ["geki", "300", "katu", "100", "50", "0"]
        .iter()
        .map(|k| count(hits, k))
        .sum()
}

pub fn wife(hits: &Value, offsets: Option<&Value>, windows: Option<&Value>) -> Wife {
    let n = total(hits);
    let successful = n.saturating_sub(count(hits, "0"));
    if let Some(offsets) = offsets.and_then(Value::as_array)
        && successful > 0
        && offsets.len() as u64 == successful
        && offsets
            .iter()
            .all(|x| x.as_f64().is_some_and(f64::is_finite))
    {
        let points: f64 = offsets.iter().map(|x| wife3(x.as_f64().unwrap())).sum();
        return Wife {
            value: ((points - count(hits, "0") as f64 * 5.5) / (2.0 * n as f64)).clamp(0.0, 1.0),
            model: "wife3-j4-offsets".into(),
        };
    }
    // Judgment bins contain no timing distribution: use the midpoint of each
    // actual window when supplied, or unmodded OD8 mania boundaries otherwise.
    let mut previous = 0.0;
    let mut points = -(count(hits, "0") as f64) * 5.5;
    for (key, boundary, window) in [
        ("geki", 16.0_f64, "perfect"),
        ("300", 40.0, "great"),
        ("katu", 73.0, "good"),
        ("100", 103.0, "ok"),
        ("50", 127.0, "meh"),
    ] {
        let upper = windows
            .and_then(|w| w[window].as_f64())
            .filter(|x| x.is_finite() && *x >= previous)
            .unwrap_or(boundary.max(previous));
        points += count(hits, key) as f64 * wife3((previous + upper) / 2.0);
        previous = upper;
    }
    Wife {
        value: if n == 0 {
            0.0
        } else {
            (points / (2.0 * n as f64)).clamp(0.0, 1.0)
        },
        model: if windows.is_some() {
            "wife3-j4-judgment-midpoints"
        } else {
            "wife3-j4-judgment-midpoints-od8"
        }
        .into(),
    }
}

// Etterna 0.75.1 MinaCalcHelpers.h aggregate_skill (float rating/resolution).
pub fn aggregate(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut rating = 0.0_f32;
    let mut resolution = 10.24_f32;
    for _ in 0..11 {
        loop {
            rating += resolution;
            let sum: f64 = values
                .iter()
                .map(|v| (2.0 / libm::erfc(0.1 * (*v as f32 - rating) as f64) - 2.0).max(0.0))
                .sum();
            if 2.0_f64.powf(rating as f64 * 0.1) >= sum {
                break;
            }
        }
        rating -= resolution;
        resolution /= 2.0;
    }
    ((rating + resolution * 2.0) * 1.05).clamp(0.0, 100.0) as f64
}

pub fn rate(mods: &Value) -> f64 {
    if let Some(r) = mods["rate"].as_f64().filter(|x| x.is_finite() && *x > 0.0) {
        return r;
    }
    if let Some(array) = mods["array"].as_array() {
        for m in array {
            if let Some(r) = m
                .pointer("/settings/speed_change")
                .and_then(Value::as_f64)
                .filter(|x| x.is_finite() && *x > 0.0)
            {
                return r;
            }
        }
        if array
            .iter()
            .any(|m| matches!(m["acronym"].as_str(), Some("DT" | "NC")))
        {
            return 1.5;
        }
        if array
            .iter()
            .any(|m| matches!(m["acronym"].as_str(), Some("HT" | "DC")))
        {
            return 0.75;
        }
    }
    let bits = mods["number"].as_u64().unwrap_or(0);
    if bits & (64 | 512) != 0 {
        1.5
    } else if bits & 256 != 0 {
        0.75
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rates() {
        assert_eq!(rate(&json!({"number":64})), 1.5);
        assert_eq!(rate(&json!({"number":256})), 0.75);
        assert_eq!(
            rate(&json!({"array":[{"acronym":"DT","settings":{"speed_change":1.2}}]})),
            1.2
        );
        assert_eq!(rate(&json!({"rate":1.35,"number":64})), 1.35);
    }
    #[test]
    fn wife_curve() {
        assert_eq!(wife3(0.0), 2.0);
        assert!(wife3(65.0).abs() < 0.000001);
        assert_eq!(wife3(180.0), -5.5);
        assert_eq!(wife3(-180.0), -5.5);
        let w = wife(&json!({"geki":2,"0":1}), Some(&json!([0, 0])), None);
        assert_eq!(w.value, 0.0);
        assert_eq!(w.model, "wife3-j4-offsets");
        assert!(wife(&json!({"geki":100}), None, None).value > 0.99);
        assert!(
            wife(&json!({"geki":100}), Some(&json!([0])), None)
                .model
                .contains("midpoints")
        );
    }
    #[test]
    fn reference_aggregation() {
        // Independent C++ reference using the pinned upstream algorithm; README.
        assert_eq!(aggregate(&[]), 0.0);
        assert!((aggregate(&[20.0]) - 15.077999115).abs() < 0.0001);
        assert!((aggregate(&[20.0, 20.0]) - 16.779001236).abs() < 0.0001);
        assert!((aggregate(&[10.0, 20.0, 30.0]) - 23.729999542).abs() < 0.0001);
    }
}
