"""Exercise ebpf-verify.py's pure parts: clang/bpftool command construction
and bpftool `-d prog loadall` output parsing. None of these touch the VM;
the end-to-end behavior is proven separately against the live VM (see
AGENTS.md's `crates/vosu-sys/src/events/bpf/` verification bullet)."""

import importlib.util
import sys
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

# A grep-filtered failure transcript shaped like the real E2BIG rejection
# this file's history hit (agent://EbpfProof): 1,000,001 processed
# instructions against the kernel's 1,000,000 budget.
FAILURE_TRANSCRIPT = """\
libbpf: prog 'process_exec': -- BEGIN PROG LOAD LOG --
BPF program is too large. Processed 1000001 insn
libbpf: prog 'process_exec': BPF program load failed: -E2BIG
"""


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
        pin = ebpf_verify.remote_pin_dir("/home/vosu/x", "aaaa")
        self.assertTrue(pin.startswith("/sys/fs/bpf/"))


class BuildLoadallCommandTests(unittest.TestCase):
    def test_removes_pin_dir_before_and_after(self):
        cmd = ebpf_verify.build_loadall_command("/tmp/x.o", "/sys/fs/bpf/p")
        self.assertEqual(cmd.count("sudo rm -rf /sys/fs/bpf/p"), 2)

    def test_uses_sudo_bpftool_debug_loadall(self):
        cmd = ebpf_verify.build_loadall_command("/tmp/x.o", "/sys/fs/bpf/p")
        self.assertIn("sudo bpftool -d prog loadall /tmp/x.o /sys/fs/bpf/p", cmd)

    def test_preserves_bpftool_exit_status(self):
        cmd = ebpf_verify.build_loadall_command("/tmp/x.o", "/sys/fs/bpf/p")
        self.assertIn("status=$?", cmd)
        self.assertIn("exit $status", cmd)


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

    def test_failure_marks_program_as_not_loaded(self):
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

    def test_empty_program_list_passes_vacuously(self):
        result = ebpf_verify.evaluate_budget([], budget=100_000)
        self.assertTrue(result.ok)


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
    def test_builds_positional_argv_string(self):
        cmd = ebpf_verify.bench_command("/bin/x", "ebpf", 1000, 50, 10000)
        self.assertEqual(cmd, "/bin/x ebpf 1000 50 10000")


if __name__ == "__main__":
    unittest.main()
