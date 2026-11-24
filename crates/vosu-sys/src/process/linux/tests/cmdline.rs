//! F1 review P2: `command_line_from_bytes` must strip every trailing
//! empty split piece, not just the final NUL terminator's own, because
//! Wine zero-fills the leftover tail after rewriting its own argv in
//! place. Declared from `linux.rs` itself as a sibling of `tests`
//! (`tests::fixture`/`tests::add_process` are `pub(super)` for this),
//! rather than as `tests`'s own child, to keep that file at its
//! baselined line count (`scripts/file-length-baseline.txt`).

use super::enumerate_processes_at;
use super::tests::{add_process, fixture};

/// F1 review P2: Wine's own `rebuild_argv`/`prepend_argv`
/// (`dlls/ntdll/unix/env.c`) zero-fill the leftover tail after
/// rewriting argv in place, leaving several trailing NUL bytes past the
/// last real argument in `/proc/<pid>/cmdline` -- every one of those
/// must be dropped, not just the final NUL terminator's own split
/// piece, while the legitimate interior empty argument still survives.
#[test]
fn command_line_drops_a_wine_style_zero_filled_tail() {
    let fixture = fixture();
    add_process(
        fixture.path(),
        10,
        2,
        b"osu!\0\0--x\0\0\0\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );

    let found = enumerate_processes_at(fixture.path()).expect("enumeration");
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].command_line.len(),
        3,
        "every padding NUL past the last real argument must be dropped"
    );
    assert_eq!(found[0].command_line[0], "osu!");
    assert_eq!(found[0].command_line[1], "");
    assert_eq!(found[0].command_line[2], "--x");
}
