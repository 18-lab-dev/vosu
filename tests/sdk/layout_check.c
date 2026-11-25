/* Static-assert check that include/vosu_plugin.h's flat struct layouts
 * match docs/v2/plugin-abi.md §3's table exactly. Compiled with
 * `-fsyntax-only` by tests/layout.rs so a header edit that disagrees with
 * the spec table fails this crate's test suite, the same way a Rust-side
 * layout drift does (tests/layout.rs's own `offset_of!` assertions). This
 * file asserts nothing the Rust side does not already assert; it exists so
 * the C header itself — not just vosu-plugin-api's Rust mirror of it —
 * cannot drift from the table unnoticed. */

#include "vosu_plugin.h"
#include <stddef.h>

_Static_assert(sizeof(vosu_field_publish_header) == 48, "vosu_field_publish_header size");
_Static_assert(offsetof(vosu_field_publish_header, handle) == 0, "vosu_field_publish_header::handle");
_Static_assert(offsetof(vosu_field_publish_header, ns_offset) == 8, "vosu_field_publish_header::ns_offset");
_Static_assert(offsetof(vosu_field_publish_header, ns_len) == 12, "vosu_field_publish_header::ns_len");
_Static_assert(offsetof(vosu_field_publish_header, pointer_offset) == 16, "vosu_field_publish_header::pointer_offset");
_Static_assert(offsetof(vosu_field_publish_header, pointer_len) == 20, "vosu_field_publish_header::pointer_len");
_Static_assert(offsetof(vosu_field_publish_header, value_kind) == 24, "vosu_field_publish_header::value_kind");
_Static_assert(offsetof(vosu_field_publish_header, reserved) == 28, "vosu_field_publish_header::reserved");
_Static_assert(offsetof(vosu_field_publish_header, scalar) == 32, "vosu_field_publish_header::scalar");
_Static_assert(offsetof(vosu_field_publish_header, data_offset) == 40, "vosu_field_publish_header::data_offset");
_Static_assert(offsetof(vosu_field_publish_header, data_len) == 44, "vosu_field_publish_header::data_len");

_Static_assert(sizeof(vosu_pointer_ref) == 16, "vosu_pointer_ref size");
_Static_assert(offsetof(vosu_pointer_ref, ns_offset) == 0, "vosu_pointer_ref::ns_offset");
_Static_assert(offsetof(vosu_pointer_ref, ns_len) == 4, "vosu_pointer_ref::ns_len");
_Static_assert(offsetof(vosu_pointer_ref, pointer_offset) == 8, "vosu_pointer_ref::pointer_offset");
_Static_assert(offsetof(vosu_pointer_ref, pointer_len) == 12, "vosu_pointer_ref::pointer_len");

_Static_assert(sizeof(vosu_demand_changed_header) == 24, "vosu_demand_changed_header size");
_Static_assert(offsetof(vosu_demand_changed_header, handle) == 0, "vosu_demand_changed_header::handle");
_Static_assert(offsetof(vosu_demand_changed_header, regular) == 8, "vosu_demand_changed_header::regular");
_Static_assert(offsetof(vosu_demand_changed_header, precise) == 9, "vosu_demand_changed_header::precise");
_Static_assert(offsetof(vosu_demand_changed_header, reserved) == 10, "vosu_demand_changed_header::reserved");
_Static_assert(offsetof(vosu_demand_changed_header, selectors_offset) == 16, "vosu_demand_changed_header::selectors_offset");
_Static_assert(offsetof(vosu_demand_changed_header, selector_count) == 20, "vosu_demand_changed_header::selector_count");

_Static_assert(sizeof(vosu_plan_base_flat) == 32, "vosu_plan_base_flat size");
_Static_assert(offsetof(vosu_plan_base_flat, kind) == 0, "vosu_plan_base_flat::kind");
_Static_assert(offsetof(vosu_plan_base_flat, module_name_offset) == 4, "vosu_plan_base_flat::module_name_offset");
_Static_assert(offsetof(vosu_plan_base_flat, module_name_len) == 8, "vosu_plan_base_flat::module_name_len");
_Static_assert(offsetof(vosu_plan_base_flat, reserved) == 12, "vosu_plan_base_flat::reserved");
_Static_assert(offsetof(vosu_plan_base_flat, address) == 16, "vosu_plan_base_flat::address");
_Static_assert(offsetof(vosu_plan_base_flat, module_offset) == 24, "vosu_plan_base_flat::module_offset");

_Static_assert(sizeof(vosu_plan_op) == 16, "vosu_plan_op size");
_Static_assert(offsetof(vosu_plan_op, offset) == 0, "vosu_plan_op::offset");
_Static_assert(offsetof(vosu_plan_op, length) == 8, "vosu_plan_op::length");
_Static_assert(offsetof(vosu_plan_op, reserved) == 12, "vosu_plan_op::reserved");

_Static_assert(sizeof(vosu_stage_base) == 24, "vosu_stage_base size");
_Static_assert(offsetof(vosu_stage_base, kind) == 0, "vosu_stage_base::kind");
_Static_assert(offsetof(vosu_stage_base, ref_stage) == 4, "vosu_stage_base::ref_stage");
_Static_assert(offsetof(vosu_stage_base, ref_op) == 8, "vosu_stage_base::ref_op");
_Static_assert(offsetof(vosu_stage_base, reserved) == 12, "vosu_stage_base::reserved");
_Static_assert(offsetof(vosu_stage_base, offset) == 16, "vosu_stage_base::offset");

_Static_assert(sizeof(vosu_stage_header) == 32, "vosu_stage_header size");
_Static_assert(offsetof(vosu_stage_header, base) == 0, "vosu_stage_header::base");
_Static_assert(offsetof(vosu_stage_header, ops_offset) == 24, "vosu_stage_header::ops_offset");
_Static_assert(offsetof(vosu_stage_header, op_count) == 28, "vosu_stage_header::op_count");

_Static_assert(sizeof(vosu_read_plan_submit_header) == 56, "vosu_read_plan_submit_header size");
_Static_assert(offsetof(vosu_read_plan_submit_header, plan) == 0, "vosu_read_plan_submit_header::plan");
_Static_assert(offsetof(vosu_read_plan_submit_header, handle) == 8, "vosu_read_plan_submit_header::handle");
_Static_assert(offsetof(vosu_read_plan_submit_header, root) == 16, "vosu_read_plan_submit_header::root");
_Static_assert(offsetof(vosu_read_plan_submit_header, stages_offset) == 48, "vosu_read_plan_submit_header::stages_offset");
_Static_assert(offsetof(vosu_read_plan_submit_header, stage_count) == 52, "vosu_read_plan_submit_header::stage_count");

_Static_assert(sizeof(vosu_read_result_flat) == 24, "vosu_read_result_flat size");
_Static_assert(offsetof(vosu_read_result_flat, kind) == 0, "vosu_read_result_flat::kind");
_Static_assert(offsetof(vosu_read_result_flat, bytes_offset) == 4, "vosu_read_result_flat::bytes_offset");
_Static_assert(offsetof(vosu_read_result_flat, bytes_len) == 8, "vosu_read_result_flat::bytes_len");
_Static_assert(offsetof(vosu_read_result_flat, error_code) == 12, "vosu_read_result_flat::error_code");
_Static_assert(offsetof(vosu_read_result_flat, error_message_offset) == 16, "vosu_read_result_flat::error_message_offset");
_Static_assert(offsetof(vosu_read_result_flat, error_message_len) == 20, "vosu_read_result_flat::error_message_len");

_Static_assert(sizeof(vosu_read_plan_result_header) == 16, "vosu_read_plan_result_header size");
_Static_assert(offsetof(vosu_read_plan_result_header, plan) == 0, "vosu_read_plan_result_header::plan");
_Static_assert(offsetof(vosu_read_plan_result_header, results_offset) == 8, "vosu_read_plan_result_header::results_offset");
_Static_assert(offsetof(vosu_read_plan_result_header, result_count) == 12, "vosu_read_plan_result_header::result_count");

_Static_assert(sizeof(vosu_publish_batch_header) == 16, "vosu_publish_batch_header size");
_Static_assert(offsetof(vosu_publish_batch_header, handle) == 0, "vosu_publish_batch_header::handle");
_Static_assert(offsetof(vosu_publish_batch_header, entries_offset) == 8, "vosu_publish_batch_header::entries_offset");
_Static_assert(offsetof(vosu_publish_batch_header, entry_count) == 12, "vosu_publish_batch_header::entry_count");
