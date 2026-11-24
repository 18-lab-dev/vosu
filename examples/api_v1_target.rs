//! Portable discovery target. Its owner reads READY then terminates/reaps it.
//! The finite lifetime bounds abandoned smoke runs without a background worker.
use std::io::{self, Write};
use std::time::{Duration, Instant};

fn main() {
    println!("READY {}", std::process::id());
    io::stdout().flush().expect("flush fixture readiness");
    let deadline = Instant::now() + Duration::from_secs(120);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        std::thread::park_timeout(remaining);
    }
}
