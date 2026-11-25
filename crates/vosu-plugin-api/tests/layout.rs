//! Layout tests: every size and offset in `docs/v2/plugin-abi.md`'s §3
//! table, asserted on the Rust side with `core::mem::offset_of!` and, in
//! [`c_header_layout_matches_the_table`], on the C side against
//! `include/vosu_plugin.h` itself so the header and these Rust types
//! cannot silently drift apart.
#![expect(
    clippy::disallowed_methods,
    reason = "test code: runs the C compiler directly; no plugin host exists in this process"
)]

use core::mem::{offset_of, size_of};
use std::process::Command;

use vosu_plugin_api::{
    vosu_demand_changed_header, vosu_field_publish_header, vosu_plan_base_flat, vosu_plan_op,
    vosu_pointer_ref, vosu_publish_batch_header, vosu_read_plan_result_header,
    vosu_read_plan_submit_header, vosu_read_result_flat, vosu_stage_base, vosu_stage_header,
};

#[test]
fn vosu_field_publish_header_layout() {
    assert_eq!(size_of::<vosu_field_publish_header>(), 48);
    assert_eq!(offset_of!(vosu_field_publish_header, handle), 0);
    assert_eq!(offset_of!(vosu_field_publish_header, ns_offset), 8);
    assert_eq!(offset_of!(vosu_field_publish_header, ns_len), 12);
    assert_eq!(offset_of!(vosu_field_publish_header, pointer_offset), 16);
    assert_eq!(offset_of!(vosu_field_publish_header, pointer_len), 20);
    assert_eq!(offset_of!(vosu_field_publish_header, value_kind), 24);
    assert_eq!(offset_of!(vosu_field_publish_header, reserved), 28);
    assert_eq!(offset_of!(vosu_field_publish_header, scalar), 32);
    assert_eq!(offset_of!(vosu_field_publish_header, data_offset), 40);
    assert_eq!(offset_of!(vosu_field_publish_header, data_len), 44);
}

#[test]
fn vosu_pointer_ref_layout() {
    assert_eq!(size_of::<vosu_pointer_ref>(), 16);
    assert_eq!(offset_of!(vosu_pointer_ref, ns_offset), 0);
    assert_eq!(offset_of!(vosu_pointer_ref, ns_len), 4);
    assert_eq!(offset_of!(vosu_pointer_ref, pointer_offset), 8);
    assert_eq!(offset_of!(vosu_pointer_ref, pointer_len), 12);
}

#[test]
fn vosu_demand_changed_header_layout() {
    assert_eq!(size_of::<vosu_demand_changed_header>(), 24);
    assert_eq!(offset_of!(vosu_demand_changed_header, handle), 0);
    assert_eq!(offset_of!(vosu_demand_changed_header, regular), 8);
    assert_eq!(offset_of!(vosu_demand_changed_header, precise), 9);
    assert_eq!(offset_of!(vosu_demand_changed_header, reserved), 10);
    assert_eq!(offset_of!(vosu_demand_changed_header, selectors_offset), 16);
    assert_eq!(offset_of!(vosu_demand_changed_header, selector_count), 20);
}

#[test]
fn vosu_plan_base_flat_layout() {
    assert_eq!(size_of::<vosu_plan_base_flat>(), 32);
    assert_eq!(offset_of!(vosu_plan_base_flat, kind), 0);
    assert_eq!(offset_of!(vosu_plan_base_flat, module_name_offset), 4);
    assert_eq!(offset_of!(vosu_plan_base_flat, module_name_len), 8);
    assert_eq!(offset_of!(vosu_plan_base_flat, reserved), 12);
    assert_eq!(offset_of!(vosu_plan_base_flat, address), 16);
    assert_eq!(offset_of!(vosu_plan_base_flat, module_offset), 24);
}

#[test]
fn vosu_plan_op_layout() {
    assert_eq!(size_of::<vosu_plan_op>(), 16);
    assert_eq!(offset_of!(vosu_plan_op, offset), 0);
    assert_eq!(offset_of!(vosu_plan_op, length), 8);
    assert_eq!(offset_of!(vosu_plan_op, reserved), 12);
}

#[test]
fn vosu_stage_base_layout() {
    assert_eq!(size_of::<vosu_stage_base>(), 24);
    assert_eq!(offset_of!(vosu_stage_base, kind), 0);
    assert_eq!(offset_of!(vosu_stage_base, ref_stage), 4);
    assert_eq!(offset_of!(vosu_stage_base, ref_op), 8);
    assert_eq!(offset_of!(vosu_stage_base, reserved), 12);
    assert_eq!(offset_of!(vosu_stage_base, offset), 16);
}

#[test]
fn vosu_stage_header_layout() {
    assert_eq!(size_of::<vosu_stage_header>(), 32);
    assert_eq!(offset_of!(vosu_stage_header, base), 0);
    assert_eq!(offset_of!(vosu_stage_header, ops_offset), 24);
    assert_eq!(offset_of!(vosu_stage_header, op_count), 28);
}

#[test]
fn vosu_read_plan_submit_header_layout() {
    assert_eq!(size_of::<vosu_read_plan_submit_header>(), 56);
    assert_eq!(offset_of!(vosu_read_plan_submit_header, plan), 0);
    assert_eq!(offset_of!(vosu_read_plan_submit_header, handle), 8);
    assert_eq!(offset_of!(vosu_read_plan_submit_header, root), 16);
    assert_eq!(offset_of!(vosu_read_plan_submit_header, stages_offset), 48);
    assert_eq!(offset_of!(vosu_read_plan_submit_header, stage_count), 52);
}

#[test]
fn vosu_read_result_flat_layout() {
    assert_eq!(size_of::<vosu_read_result_flat>(), 24);
    assert_eq!(offset_of!(vosu_read_result_flat, kind), 0);
    assert_eq!(offset_of!(vosu_read_result_flat, bytes_offset), 4);
    assert_eq!(offset_of!(vosu_read_result_flat, bytes_len), 8);
    assert_eq!(offset_of!(vosu_read_result_flat, error_code), 12);
    assert_eq!(offset_of!(vosu_read_result_flat, error_message_offset), 16);
    assert_eq!(offset_of!(vosu_read_result_flat, error_message_len), 20);
}

#[test]
fn vosu_read_plan_result_header_layout() {
    assert_eq!(size_of::<vosu_read_plan_result_header>(), 16);
    assert_eq!(offset_of!(vosu_read_plan_result_header, plan), 0);
    assert_eq!(offset_of!(vosu_read_plan_result_header, results_offset), 8);
    assert_eq!(offset_of!(vosu_read_plan_result_header, result_count), 12);
}

#[test]
fn vosu_publish_batch_header_layout() {
    assert_eq!(size_of::<vosu_publish_batch_header>(), 16);
    assert_eq!(offset_of!(vosu_publish_batch_header, handle), 0);
    assert_eq!(offset_of!(vosu_publish_batch_header, entries_offset), 8);
    assert_eq!(offset_of!(vosu_publish_batch_header, entry_count), 12);
}

/// Finds a C compiler to run `tests/layout_check.c`'s `_Static_assert`s
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

/// Compiles `tests/layout_check.c` with `-fsyntax-only` against
/// `include/vosu_plugin.h`: its `_Static_assert`s mirror this file's
/// `offset_of!` assertions, so a header edit that disagrees with
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
    let include_dir = format!("{manifest_dir}/../../include");
    let source = format!("{manifest_dir}/tests/layout_check.c");

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
