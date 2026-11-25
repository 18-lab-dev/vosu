//! Latency summaries, Criterion estimate readback and `METRIC` output.

use std::path::Path;
use std::time::Duration;

use criterion::Criterion;

pub struct Percentiles {
    pub p50_ns: f64,
    pub p99_ns: f64,
    pub mean_ns: f64,
}

pub fn percentiles(mut samples: Vec<u128>) -> Percentiles {
    samples.sort_unstable();
    let len = samples.len();
    let p50 = samples[len / 2] as f64;
    let p99 = samples[(len * 99 / 100).min(len - 1)] as f64;
    let mean = samples.iter().sum::<u128>() as f64 / len as f64;
    Percentiles {
        p50_ns: p50,
        p99_ns: p99,
        mean_ns: mean,
    }
}

/// Criterion's median estimate with its 95% confidence interval.
pub struct CriterionMedian {
    pub median_ns: f64,
    pub low_ns: f64,
    pub high_ns: f64,
}

/// A library-driven Criterion (no command-line parsing: the runner takes no
/// Criterion flags) writing under `output`.
pub fn criterion(output: &Path, measurement: Duration) -> Criterion {
    Criterion::default()
        .output_directory(output)
        .without_plots()
        .with_output_color(false)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(measurement)
}

/// Reads benchmark `id`'s median estimate from a Criterion `output`
/// directory.
pub fn criterion_median(output: &Path, id: &str) -> CriterionMedian {
    let path = output.join(id).join("new").join("estimates.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read Criterion estimates {}: {error}", path.display()));
    let estimates: serde_json::Value =
        serde_json::from_str(&text).expect("Criterion estimates are JSON");
    let median = &estimates["median"];
    let number = |value: &serde_json::Value| value.as_f64().expect("Criterion estimate number");
    CriterionMedian {
        median_ns: number(&median["point_estimate"]),
        low_ns: number(&median["confidence_interval"]["lower_bound"]),
        high_ns: number(&median["confidence_interval"]["upper_bound"]),
    }
}

/// Prints one `METRIC {prefix}_{name}={value}` line.
pub fn metric(prefix: &str, name: &str, value: impl std::fmt::Display) {
    println!("METRIC {prefix}_{name}={value}");
}

pub fn print_percentiles(prefix: &str, timing: &Percentiles) {
    metric(prefix, "p50_ns", format_args!("{:.1}", timing.p50_ns));
    metric(prefix, "p99_ns", format_args!("{:.1}", timing.p99_ns));
    metric(prefix, "mean_ns", format_args!("{:.1}", timing.mean_ns));
}

pub fn print_criterion(prefix: &str, median: &CriterionMedian) {
    metric(
        prefix,
        "criterion_median_ns",
        format_args!("{:.1}", median.median_ns),
    );
    metric(
        prefix,
        "criterion_median_ci95_low_ns",
        format_args!("{:.1}", median.low_ns),
    );
    metric(
        prefix,
        "criterion_median_ci95_high_ns",
        format_args!("{:.1}", median.high_ns),
    );
}
