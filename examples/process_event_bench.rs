#![recursion_limit = "256"]

#[cfg(target_os = "linux")]
include!("support/process_event_bench.rs");

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("process_event_bench is only available on Linux");
}
