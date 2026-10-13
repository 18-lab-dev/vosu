#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
// Keep benchmark and correctness fixture bytes identical; no second decoder or graph.
#[allow(dead_code)]
#[path = "../benches/autoresearch_regular_reader.rs"]
mod benchmark;

#[test]
fn native_regular_capture_decodes_all_selected_states() {
    for lazer in [true, false] {
        for status in [0, 2, 7] {
            let mut fixture = benchmark::fixture::Fixture::new(lazer, status);
            let snapshot = fixture.capture();
            fixture.assert_snapshot(&snapshot);
            let (snapshot, calls, bytes) = fixture.measured_capture();
            fixture.assert_snapshot(&snapshot);
            assert!(calls > 20, "successful capture must read a populated graph");
            assert!(bytes > 100, "successful capture must read populated fields");
        }
    }
}
