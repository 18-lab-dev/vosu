#![recursion_limit = "256"]
#![cfg_attr(
    target_os = "linux",
    expect(
        clippy::disallowed_methods,
        reason = "benchmark harness: spawns its own workload children; no plugin host exists in this process"
    )
)]

#[cfg(target_os = "linux")]
include!("support/process_event_bench.rs");

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("process_event_bench is only available on Linux");
}
