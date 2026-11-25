//! The C half of the SDK layout tests: `include/vosu_plugin.h` compiled
//! against `docs/v2/plugin-abi.md`'s §3 table. The header is a separate
//! release asset, not part of the published `vosu-plugin-api` package, so
//! this workspace-only check lives here (AF.9); the Rust half stays in the
//! SDK's own self-contained `tests/layout.rs`.
#![expect(
    clippy::disallowed_methods,
    reason = "test code: runs the C compiler directly; no plugin host exists in this process"
)]

use std::process::Command;

/// Finds a C compiler to run `tests/sdk/layout_check.c`'s `_Static_assert`s
/// through, without a build-time `cc` crate dependency: a dev/build
/// dependency on `cc` is built around `OUT_DIR`/`TARGET`/`HOST` being set by
/// cargo for a build script, none of which a `tests/*.rs` binary gets, so
/// invoking the platform compiler directly here is the cleaner fit for a
/// test-only check (see the commit message for the full tradeoff).
fn find_c_compiler() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(cc) = std::env::var("CC") {
        candidates.push(cc);
    }
    candidates.push("cc".to_owned());
    candidates.push("clang".to_owned());
    candidates.push("gcc".to_owned());
    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// Compiles `tests/sdk/layout_check.c` with `-fsyntax-only` against
/// `include/vosu_plugin.h`: its `_Static_assert`s mirror
/// `crates/vosu-plugin-api/tests/layout.rs`'s `offset_of!` assertions, so a header edit that disagrees with
/// `docs/v2/plugin-abi.md`'s §3 table fails here even if nobody touches
/// the Rust types.
#[test]
fn c_header_layout_matches_the_table() {
    let Some(compiler) = find_c_compiler() else {
        panic!(
            "no C compiler (checked $CC, cc, clang, gcc) found to verify include/vosu_plugin.h's layout against docs/v2/plugin-abi.md's §3 table"
        );
    };

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let include_dir = format!("{manifest_dir}/include");
    let source = format!("{manifest_dir}/tests/sdk/layout_check.c");

    let output = Command::new(&compiler)
        .args(["-fsyntax-only", "-std=c11", "-Wall", "-Wextra", "-I"])
        .arg(&include_dir)
        .arg(&source)
        .output()
        .unwrap_or_else(|error| panic!("failed to run {compiler} on {source}: {error}"));

    assert!(
        output.status.success(),
        "{compiler} rejected {source} (include/vosu_plugin.h disagrees with docs/v2/plugin-abi.md's §3 table):\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
