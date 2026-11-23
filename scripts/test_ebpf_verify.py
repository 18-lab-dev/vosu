"""Exercise ebpf-verify.py's pure parts: clang/bpftool command construction,
`bpftool -d prog loadall` output parsing, and the pass/fail verdict. None of
these touch the VM; the end-to-end behavior is proven separately against the
live VM (see AGENTS.md's `crates/vosu-sys/src/events/bpf/` verification
bullet)."""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


ebpf_verify = _load("ebpf_verify", "ebpf-verify.py")

# A grep-filtered success transcript as build_loadall_command's pipeline
# would actually produce it, captured from a real `bpftool -d prog loadall`
# run against this file's history (vosu-gaming VM, kernel 7.2.11-zen1).
SUCCESS_TRANSCRIPT = """\
libbpf: prog 'process_exec': -- BEGIN PROG LOAD LOG --
verification time 753572 usec
processed 86454 insns (limit 1000000) max_states_per_insn 80 total_states 7170 peak_states 6381 mark_read 0
libbpf: prog 'process_exec': pinned at '/sys/fs/bpf/ebpf-verify-abcd/process_exec'
libbpf: prog 'process_fork': -- BEGIN PROG LOAD LOG --
verification time 321 usec
processed 101 insns (limit 1000000) max_states_per_insn 0 total_states 9 peak_states 9 mark_read 0
libbpf: prog 'process_fork': pinned at '/sys/fs/bpf/ebpf-verify-abcd/process_fork'
libbpf: prog 'process_exit': -- BEGIN PROG LOAD LOG --
verification time 804520 usec
processed 86683 insns (limit 1000000) max_states_per_insn 80 total_states 7188 peak_states 6400 mark_read 0
libbpf: prog 'process_exit': pinned at '/sys/fs/bpf/ebpf-verify-abcd/process_exit'
"""

# A grep-filtered failure transcript in libbpf 1.7's real order: the
# `BPF program load failed` warning (libbpf.c:7948) prints *before* the
# verbose `-- BEGIN PROG LOAD LOG --` body (libbpf.c:7952), the opposite of
# what an earlier version of this fixture assumed. Shaped like the real
# E2BIG rejection this file's history hit (agent://EbpfProof): 1,000,001
# processed instructions against the kernel's 1,000,000 budget.
FAILURE_TRANSCRIPT = """\
libbpf: prog 'process_exec': BPF program load failed: -E2BIG
libbpf: prog 'process_exec': -- BEGIN PROG LOAD LOG --
BPF program is too large. Processed 1000001 insn
"""

# A sudo failure, a missing bpftool, and a map-create failure all print no
# line any of build_loadall_command's grep patterns match, so the parse is
# empty regardless of what text the guest actually produced. These three
# differ only in the (never-grep-matched) stderr/stdout text and exit code
# a caller would see, which check_verification does not need to parse to
# fail correctly.
SUDO_FAILURE_TEXT = "sudo: a terminal is required to read the password\n"
MISSING_BPFTOOL_TEXT = ""
MISSING_BPFTOOL_ERR = "bash: line 1: bpftool: command not found\n"
MAP_CREATE_FAILURE_TEXT = (
    "libbpf: map 'events': failed to create: Function not implemented(-38)\n"
    "libbpf: failed to load object 'process_events.bpf.o'\n"
)


class ClangCompileArgsTests(unittest.TestCase):
    def test_mirrors_libbpf_cargo_flags(self):
        args = ebpf_verify.clang_compile_args("src.c", "out.o", "/usr/include")
        self.assertEqual(args[0], "clang")
        self.assertIn("-fno-stack-protector", args)
        self.assertIn("-g", args)
        self.assertIn("-O2", args)
        self.assertEqual(args[args.index("-target") + 1], "bpf")
        self.assertIn("-D__TARGET_ARCH_x86", args)
        self.assertEqual(args[args.index("-I") + 1], "/usr/include")
        self.assertEqual(args[args.index("-c") + 1], "src.c")
        self.assertEqual(args[args.index("-o") + 1], "out.o")

    def test_maps_arch_to_core_arch_name(self):
        args = ebpf_verify.clang_compile_args("s.c", "o.o", "/x", arch="aarch64")
        self.assertIn("-D__TARGET_ARCH_arm64", args)

    def test_unknown_arch_passes_through(self):
        args = ebpf_verify.clang_compile_args("s.c", "o.o", "/x", arch="unknownarch")
        self.assertIn("-D__TARGET_ARCH_unknownarch", args)


class RemoteDirTests(unittest.TestCase):
    def test_work_dir_is_unique_per_token(self):
        a = ebpf_verify.remote_work_dir("aaaa")
        b = ebpf_verify.remote_work_dir("bbbb")
        self.assertNotEqual(a, b)
        self.assertIn("aaaa", a)

    def test_pin_dir_under_bpf_fs(self):
        pin = ebpf_verify.remote_pin_dir(ebpf_verify.remote_work_dir("aaaa"))
        self.assertTrue(pin.startswith("/sys/fs/bpf/"))

    def test_pin_dir_derived_from_work_dir_token(self):
        work_dir = ebpf_verify.remote_work_dir("beef")
        self.assertEqual(ebpf_verify.remote_pin_dir(work_dir), "/sys/fs/bpf/ebpf-verify-beef")


class BuildLoadallCommandTests(unittest.TestCase):
    def test_removes_pin_dir_before_and_after(self):
        cmd = ebpf_verify.build_loadall_command("/tmp/x.o", "/sys/fs/bpf/p")
        self.assertEqual(cmd.count("sudo rm -rf /sys/fs/bpf/p"), 2)

    def test_uses_sudo_bpftool_debug_loadall(self):
        cmd = ebpf_verify.build_loadall_command("/tmp/x.o", "/sys/fs/bpf/p")
        self.assertIn("sudo bpftool -d prog loadall /tmp/x.o /sys/fs/bpf/p", cmd)

    def test_captures_bpftool_exit_status_via_pipestatus(self):
        cmd = ebpf_verify.build_loadall_command("/tmp/x.o", "/sys/fs/bpf/p")
        self.assertIn("PIPESTATUS[0]", cmd)


@unittest.skipUnless(shutil.which("bash"), "bash not available")
class LoadallPipelineBehaviorTests(unittest.TestCase):
    """Runs the generated shell snippet for real, against a fake `bpftool`
    on PATH, so these fail for the reason their names give rather than just
    checking which strings appear in the command text."""

    def _run_with_fake_bpftool(self, bpftool_script: str) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory() as bin_dir:
            fake_bpftool = Path(bin_dir) / "bpftool"
            fake_bpftool.write_text(f"#!/bin/sh\n{bpftool_script}\n")
            fake_bpftool.chmod(0o755)
            fake_sudo = Path(bin_dir) / "sudo"
            fake_sudo.write_text('#!/bin/sh\nexec "$@"\n')
            fake_sudo.chmod(0o755)
            cmd = ebpf_verify.build_loadall_command("/tmp/x.o", "/tmp/ebpf-verify-pin-test")
            env = dict(os.environ)
            env["PATH"] = f"{bin_dir}:{env['PATH']}"
            return subprocess.run(["bash", "-c", cmd], env=env, capture_output=True, text=True, timeout=10)

    def test_bpftool_failure_is_reported_even_when_grep_matches_a_line_first(self):
        # bpftool prints one grep-matching line (so grep itself succeeds,
        # exit 0) and then crashes: only PIPESTATUS[0], not the pipeline's
        # own $?, can still see that as a failure.
        completed = self._run_with_fake_bpftool(
            "echo \"libbpf: prog 'process_exec': -- BEGIN PROG LOAD LOG --\"; "
            'echo "simulated crash mid-verification" >&2; exit 1'
        )
        self.assertNotEqual(completed.returncode, 0)

    def test_success_still_reports_zero(self):
        completed = self._run_with_fake_bpftool(
            "echo \"libbpf: prog 'process_exec': -- BEGIN PROG LOAD LOG --\"; "
            "echo \"libbpf: prog 'process_exec': pinned at '/sys/fs/bpf/x'\"; exit 0"
        )
        self.assertEqual(completed.returncode, 0)


class ParseLoadallOutputTests(unittest.TestCase):
    def test_parses_every_program_in_order(self):
        programs = ebpf_verify.parse_loadall_output(SUCCESS_TRANSCRIPT)
        self.assertEqual([p.name for p in programs], ["process_exec", "process_fork", "process_exit"])
        self.assertTrue(all(p.loaded for p in programs))

    def test_parses_processed_states_and_time(self):
        programs = ebpf_verify.parse_loadall_output(SUCCESS_TRANSCRIPT)
        exec_prog = programs[0]
        self.assertEqual(exec_prog.processed, 86454)
        self.assertEqual(exec_prog.limit, 1_000_000)
        self.assertEqual(exec_prog.total_states, 7170)
        self.assertEqual(exec_prog.peak_states, 6381)
        self.assertEqual(exec_prog.verification_time_us, 753572)

    def test_trivial_program_parses_too(self):
        programs = ebpf_verify.parse_loadall_output(SUCCESS_TRANSCRIPT)
        fork_prog = programs[1]
        self.assertEqual(fork_prog.processed, 101)
        self.assertEqual(fork_prog.total_states, 9)

    def test_failure_marks_program_as_not_loaded_with_a_single_row(self):
        programs = ebpf_verify.parse_loadall_output(FAILURE_TRANSCRIPT)
        self.assertEqual(len(programs), 1)
        self.assertEqual(programs[0].name, "process_exec")
        self.assertFalse(programs[0].loaded)
        self.assertIsNone(programs[0].processed)
        self.assertIn("E2BIG", programs[0].error)

    def test_empty_output_parses_to_no_programs(self):
        self.assertEqual(ebpf_verify.parse_loadall_output(""), [])


class EvaluateBudgetTests(unittest.TestCase):
    def test_passes_when_every_program_under_budget(self):
        programs = ebpf_verify.parse_loadall_output(SUCCESS_TRANSCRIPT)
        result = ebpf_verify.evaluate_budget(programs, budget=100_000)
        self.assertTrue(result.ok)
        self.assertEqual(result.violations, [])

    def test_fails_when_a_program_exceeds_budget(self):
        programs = ebpf_verify.parse_loadall_output(SUCCESS_TRANSCRIPT)
        result = ebpf_verify.evaluate_budget(programs, budget=50_000)
        self.assertFalse(result.ok)
        self.assertTrue(any("process_exec" in v for v in result.violations))
        self.assertTrue(any("process_exit" in v for v in result.violations))
        # process_fork (101 insns) stays under even a tight budget.
        self.assertFalse(any("process_fork" in v for v in result.violations))

    def test_fails_when_a_program_did_not_load(self):
        programs = ebpf_verify.parse_loadall_output(FAILURE_TRANSCRIPT)
        result = ebpf_verify.evaluate_budget(programs, budget=100_000)
        self.assertFalse(result.ok)
        self.assertIn("process_exec", result.violations[0])
        self.assertIn("failed to load", result.violations[0])


class CheckVerificationTests(unittest.TestCase):
    """The combined rc + parsed-program-set + budget verdict `main` relies
    on, covering every empty-or-silent failure shape finding 1 named."""

    def test_passes_only_when_all_five_expected_programs_load_under_budget(self):
        # SUCCESS_TRANSCRIPT only has 3 of the 5 real programs, so even a
        # clean rc=0 run of it must fail here on the other two missing.
        programs = ebpf_verify.parse_loadall_output(SUCCESS_TRANSCRIPT)
        result = ebpf_verify.check_verification(0, "", programs, budget=100_000)
        self.assertFalse(result.ok)
        self.assertTrue(any("missing expected program" in v for v in result.violations))

    def test_sudo_failure_text_fails(self):
        programs = ebpf_verify.parse_loadall_output(SUDO_FAILURE_TEXT)
        result = ebpf_verify.check_verification(1, SUDO_FAILURE_TEXT, programs, budget=100_000)
        self.assertFalse(result.ok)
        self.assertTrue(any("bpftool loadall exited 1" in v for v in result.violations))
        self.assertTrue(any("missing expected program" in v for v in result.violations))

    def test_missing_bpftool_fails(self):
        programs = ebpf_verify.parse_loadall_output(MISSING_BPFTOOL_TEXT)
        result = ebpf_verify.check_verification(127, MISSING_BPFTOOL_ERR, programs, budget=100_000)
        self.assertFalse(result.ok)
        self.assertTrue(any("bpftool loadall exited 127" in v for v in result.violations))

    def test_map_create_failure_output_fails(self):
        programs = ebpf_verify.parse_loadall_output(MAP_CREATE_FAILURE_TEXT)
        self.assertEqual(programs, [])  # none of that text matches the grep patterns
        result = ebpf_verify.check_verification(1, "", programs, budget=100_000)
        self.assertFalse(result.ok)
        self.assertTrue(any("missing expected program" in v for v in result.violations))

    def test_empty_program_list_fails(self):
        # The inverse of the old (buggy) "passes vacuously" assumption:
        # an empty parse must fail, because it means none of the object's
        # five expected programs were ever confirmed loaded.
        result = ebpf_verify.check_verification(0, "", [], budget=100_000)
        self.assertFalse(result.ok)
        self.assertEqual(
            result.violations,
            [f"missing expected program(s): {', '.join(sorted(ebpf_verify.EXPECTED_PROGRAMS))}"],
        )

    def test_unexpected_program_name_fails(self):
        programs = [ebpf_verify.ProgramVerification(name="mystery_prog", loaded=True, processed=10)]
        result = ebpf_verify.check_verification(0, "", programs, budget=100_000)
        self.assertFalse(result.ok)
        self.assertTrue(any("unexpected program" in v for v in result.violations))

    def test_all_five_expected_programs_under_budget_passes(self):
        programs = [
            ebpf_verify.ProgramVerification(name=name, loaded=True, processed=1_000)
            for name in sorted(ebpf_verify.EXPECTED_PROGRAMS)
        ]
        result = ebpf_verify.check_verification(0, "", programs, budget=100_000)
        self.assertTrue(result.ok)
        self.assertEqual(result.violations, [])


class FormatTableTests(unittest.TestCase):
    def test_includes_every_program_name_and_budget(self):
        programs = ebpf_verify.parse_loadall_output(SUCCESS_TRANSCRIPT)
        table = ebpf_verify.format_table(programs, budget=100_000)
        self.assertIn("process_exec", table)
        self.assertIn("process_fork", table)
        self.assertIn("process_exit", table)
        self.assertIn("86454", table)
        self.assertIn("budget: 100000", table)

    def test_marks_a_failed_program_explicitly(self):
        programs = ebpf_verify.parse_loadall_output(FAILURE_TRANSCRIPT)
        table = ebpf_verify.format_table(programs, budget=100_000)
        self.assertIn("NO (", table)
        self.assertIn("E2BIG", table)


class BenchCommandTests(unittest.TestCase):
    def test_builds_positional_argv_string_in_mode_idle_iterations_churn_order(self):
        cmd = ebpf_verify.bench_command("/bin/x", "ebpf", 1000, 16, 50, 10000)
        self.assertEqual(cmd, "/bin/x ebpf 1000 16 50 10000")


class SummarizeBenchResultsTests(unittest.TestCase):
    def test_reduces_lifecycle_cpu_and_median_add_latency_across_samples(self):
        def sample(user_cpu, system_cpu, median_latency):
            return (
                '{"lifecycle": {"user_cpu_us": %d, "system_cpu_us": %d}, '
                '"add_latency": {"median_us": %d}}' % (user_cpu, system_cpu, median_latency)
            )

        results = {
            "ebpf": [sample(1000, 200, 50), sample(1200, 300, 60)],
            "cn-proc": [sample(2000, 500, 80)],
        }
        summary = ebpf_verify.summarize_bench_results(results)
        self.assertAlmostEqual(summary["ebpf"]["lifecycle_cpu_us_mean"], 1350.0)
        self.assertEqual(summary["ebpf"]["lifecycle_cpu_us_min"], 1200)
        self.assertEqual(summary["ebpf"]["lifecycle_cpu_us_max"], 1500)
        self.assertAlmostEqual(summary["ebpf"]["detect_latency_us_mean"], 55.0)
        self.assertEqual(summary["cn-proc"]["lifecycle_cpu_us_mean"], 2500)


if __name__ == "__main__":
    unittest.main()
