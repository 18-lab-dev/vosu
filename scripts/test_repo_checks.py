"""Exercise check-file-length.py, check-brand.py, and check-module-boundaries.py on fixture trees."""

import importlib.util
from pathlib import Path
import tempfile
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


file_length = _load("check_file_length", "check-file-length.py")
brand = _load("check_brand", "check-brand.py")
boundaries = _load("check_module_boundaries", "check-module-boundaries.py")


def write(root, relative, content):
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


class FileLengthTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.baseline = self.root / "baseline.txt"

    def write_lines(self, relative, count):
        write(self.root, relative, "\n".join(f"line {i}" for i in range(count)) + "\n")

    def test_passes_under_the_limit(self):
        self.write_lines("src/small.rs", 10)
        self.baseline.write_text("", encoding="utf-8")
        errors, current = file_length.check(self.root, self.baseline, tracked=["src/small.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {"src/small.rs": 10})

    def test_fails_new_file_over_limit_without_baseline(self):
        self.write_lines("src/big.rs", 1001)
        self.baseline.write_text("", encoding="utf-8")
        errors, _current = file_length.check(self.root, self.baseline, tracked=["src/big.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("src/big.rs: 1001 lines exceeds the 1,000-line", errors[0])

    def test_fails_when_baselined_file_grows(self):
        self.write_lines("src/big.rs", 1100)
        self.baseline.write_text("1050 src/big.rs\n", encoding="utf-8")
        errors, _current = file_length.check(self.root, self.baseline, tracked=["src/big.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("exceeds its baseline of 1050", errors[0])

    def test_fails_on_stale_entry_once_file_shrinks(self):
        self.write_lines("src/big.rs", 900)
        self.baseline.write_text("1050 src/big.rs\n", encoding="utf-8")
        errors, _current = file_length.check(self.root, self.baseline, tracked=["src/big.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("at or below the 1,000-line limit; delete its stale entry", errors[0])

    def test_fails_on_stale_entry_once_file_is_gone(self):
        self.baseline.write_text("1050 src/gone.rs\n", encoding="utf-8")
        errors, _current = file_length.check(self.root, self.baseline, tracked=[])
        self.assertEqual(len(errors), 1)
        self.assertIn("no longer tracked", errors[0])

    def test_update_refuses_to_add_an_unreviewed_entry(self):
        self.write_lines("src/big.rs", 1001)
        self.baseline.write_text("", encoding="utf-8")
        refusals, result = file_length.update(self.root, self.baseline, tracked=["src/big.rs"])
        self.assertIsNone(result)
        self.assertEqual(len(refusals), 1)
        self.assertIn("cannot add new entries", refusals[0])

    def test_update_refuses_to_raise_a_count(self):
        self.write_lines("src/big.rs", 1100)
        self.baseline.write_text("1050 src/big.rs\n", encoding="utf-8")
        refusals, result = file_length.update(self.root, self.baseline, tracked=["src/big.rs"])
        self.assertIsNone(result)
        self.assertEqual(len(refusals), 1)
        self.assertIn("cannot raise a baseline", refusals[0])

    def test_update_lowers_a_shrunk_count(self):
        self.write_lines("src/big.rs", 1010)
        self.baseline.write_text("1200 src/big.rs\n", encoding="utf-8")
        refusals, result = file_length.update(self.root, self.baseline, tracked=["src/big.rs"])
        self.assertEqual(refusals, [])
        self.assertEqual(result, {"src/big.rs": 1010})
        self.assertEqual(self.baseline.read_text(encoding="utf-8"), "1010 src/big.rs\n")

    def test_update_drops_a_now_compliant_entry(self):
        self.write_lines("src/small.rs", 50)
        self.baseline.write_text("1200 src/small.rs\n", encoding="utf-8")
        refusals, result = file_length.update(self.root, self.baseline, tracked=["src/small.rs"])
        self.assertEqual(refusals, [])
        self.assertEqual(result, {})

    def test_excludes_provenance_pinned_trees(self):
        self.write_lines("crates/vosu-pp/rosu-pp-api/src/huge.rs", 2000)
        self.baseline.write_text("", encoding="utf-8")
        errors, current = file_length.check(
            self.root, self.baseline, tracked=["crates/vosu-pp/rosu-pp-api/src/huge.rs"]
        )
        self.assertEqual(errors, [])
        self.assertEqual(current, {})


class BrandTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.allowlist = self.root / "allowlist.txt"

    def test_passes_with_no_old_brand(self):
        write(self.root, "src/lib.rs", "pub fn vosu() {}\n")
        self.allowlist.write_text("", encoding="utf-8")
        errors, current = brand.check(self.root, self.allowlist, tracked=["src/lib.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {})

    def test_fails_on_unexplained_occurrence(self):
        write(self.root, "src/new.rs", "const NAME: &str = \"tosu\";\n")
        self.allowlist.write_text("", encoding="utf-8")
        errors, _current = brand.check(self.root, self.allowlist, tracked=["src/new.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("unexplained occurrence", errors[0])

    def test_passes_within_an_allowlisted_count(self):
        write(self.root, "src/migration.rs", "// tosu.env\n// tosu.env\n")
        self.allowlist.write_text("src/migration.rs 2 migration legacy tosu.env fixture\n", encoding="utf-8")
        errors, _current = brand.check(self.root, self.allowlist, tracked=["src/migration.rs"])
        self.assertEqual(errors, [])

    def test_fails_above_the_allowlisted_maximum(self):
        write(self.root, "src/migration.rs", "tosu tosu tosu\n")
        self.allowlist.write_text("src/migration.rs 1 migration legacy fixture\n", encoding="utf-8")
        errors, _current = brand.check(self.root, self.allowlist, tracked=["src/migration.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("exceeds its scripts/brand-allowlist.txt maximum of 1", errors[0])

    def test_fails_on_glob_entry_whose_file_is_gone(self):
        self.allowlist.write_text("docs/** 3 history old docs mention\n", encoding="utf-8")
        errors, _current = brand.check(self.root, self.allowlist, tracked=[])
        self.assertEqual(len(errors), 1)
        self.assertIn("matches no tracked file", errors[0])

    def test_fails_when_allowlisted_count_drops(self):
        write(self.root, "docs/history.md", "tosu\n")
        self.allowlist.write_text("docs/** 5 history old docs mention\n", encoding="utf-8")
        errors, _current = brand.check(self.root, self.allowlist, tracked=["docs/history.md"])
        self.assertEqual(len(errors), 1)
        self.assertIn("lower the recorded maximum", errors[0])

    def test_rejects_an_unknown_category(self):
        self.allowlist.write_text("src/x.rs 1 nonsense reason text\n", encoding="utf-8")
        with self.assertRaises(brand.AllowlistError):
            brand.check(self.root, self.allowlist, tracked=[])

    def test_skips_undecodable_binary_files(self):
        path = write(self.root, "assets/binary.bin", "placeholder")
        path.write_bytes(b"\xff\xfe\x00tosu\x00")
        self.allowlist.write_text("", encoding="utf-8")
        errors, current = brand.check(self.root, self.allowlist, tracked=["assets/binary.bin"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {})


class ModuleBoundariesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.rules = self.root / "rules.txt"
        self.lib_text = "pub mod alpha;\npub mod beta;\npub mod gamma;\n"

    def test_passes_when_edges_match_the_rules(self):
        src_files = {"alpha.rs": "use crate::beta::Thing;\n"}
        self.rules.write_text("alpha -> beta\n", encoding="utf-8")
        errors, edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(errors, [])
        self.assertEqual(set(edges), {("alpha", "beta")})

    def test_fails_on_a_new_edge(self):
        src_files = {"alpha.rs": "use crate::beta::Thing;\n"}
        self.rules.write_text("", encoding="utf-8")
        errors, _edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(len(errors), 1)
        self.assertIn("new module edge alpha -> beta", errors[0])
        self.assertIn("alpha.rs:1", errors[0])

    def test_fails_on_a_stale_rule(self):
        src_files = {"alpha.rs": "pub fn thing() {}\n"}
        self.rules.write_text("alpha -> beta\n", encoding="utf-8")
        errors, _edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(len(errors), 1)
        self.assertIn("alpha -> beta is no longer used", errors[0])

    def test_ignores_tests_rs_files_and_tests_directories(self):
        src_files = {
            "alpha/tests.rs": "use crate::beta::Thing;\n",
            "alpha/tests/extra.rs": "use crate::gamma::Thing;\n",
        }
        self.rules.write_text("", encoding="utf-8")
        errors, edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(errors, [])
        self.assertEqual(edges, {})

    def test_ignores_inline_cfg_test_mod_blocks(self):
        src_files = {
            "alpha.rs": (
                "pub fn thing() {}\n"
                "#[cfg(test)]\n"
                "mod extra_tests {\n"
                "    use crate::beta::Thing;\n"
                "    fn inner() { let _ = crate::gamma::Other; }\n"
                "}\n"
            )
        }
        self.rules.write_text("", encoding="utf-8")
        errors, edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(errors, [])
        self.assertEqual(edges, {})

    def test_ignores_cfg_test_external_module_files(self):
        src_files = {
            "alpha.rs": "#[cfg(test)]\nmod fixtures;\n",
            "alpha/fixtures.rs": "use crate::beta::Thing;\n",
        }
        self.rules.write_text("", encoding="utf-8")
        errors, edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(errors, [])
        self.assertEqual(edges, {})

    def test_expands_grouped_use_statements(self):
        src_files = {
            "alpha.rs": "use crate::{\n    beta::Thing,\n    gamma,\n};\n",
        }
        self.rules.write_text("", encoding="utf-8")
        errors, edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(set(edges), {("alpha", "beta"), ("alpha", "gamma")})
        self.assertEqual(len(errors), 2)

    def test_ignores_crate_paths_inside_strings_and_comments(self):
        src_files = {
            "alpha.rs": (
                '// crate::beta reference in a comment\n'
                'const DOC: &str = "see crate::beta for details";\n'
            )
        }
        self.rules.write_text("", encoding="utf-8")
        errors, edges = boundaries.check(self.rules, src_files=src_files, lib_text=self.lib_text)
        self.assertEqual(errors, [])
        self.assertEqual(edges, {})


if __name__ == "__main__":
    unittest.main()
