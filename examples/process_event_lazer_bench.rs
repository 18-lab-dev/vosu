#[cfg(target_os = "linux")]
include!("support/process_event_lazer_bench.rs");

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("process_event_lazer_bench is only available on Linux");
}
