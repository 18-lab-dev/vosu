//! A bounded native process for the test-harness daemon to attach to. The
//! parent owns it directly; it has no protocol, port, stdin or output.
#![expect(
    clippy::disallowed_methods,
    reason = "fixture target keeps an OS process alive"
)]

fn main() {
    std::thread::sleep(std::time::Duration::from_secs(300));
}
