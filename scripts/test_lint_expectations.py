"""Exercise check-lint-expectations.py on fixture trees."""

import importlib.util
from pathlib import Path
import tempfile
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


lint_expectations = _load("check_lint_expectations", "check-lint-expectations.py")


def write(root, relative, content):
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


EXPECT_ATTR = """#[expect(
    clippy::disallowed_methods,
    reason = "polling a bounded-deadline retry"
)]
std::thread::sleep(Duration::from_millis(5));
"""


class LintExpectationsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.allowlist = self.root / "allowlist.txt"

    def test_passes_with_no_expect_attributes(self):
        write(self.root, "src/lib.rs", "pub fn vosu() {}\n")
        self.allowlist.write_text("", encoding="utf-8")
        errors, current = lint_expectations.check(self.root, self.allowlist, tracked=["src/lib.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {})

    def test_fails_on_unexplained_production_site(self):
        write(self.root, "src/new.rs", f"fn f() {{\n    {EXPECT_ATTR}}}\n")
        self.allowlist.write_text("", encoding="utf-8")
        errors, _current = lint_expectations.check(self.root, self.allowlist, tracked=["src/new.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("unexplained clippy::disallowed_methods", errors[0])

    def test_passes_within_an_allowlisted_count(self):
        write(self.root, "src/new.rs", f"fn f() {{\n    {EXPECT_ATTR}}}\n")
        self.allowlist.write_text("src/new.rs disallowed_methods 1 bounded-poll deadline-bound retry\n", encoding="utf-8")
        errors, current = lint_expectations.check(self.root, self.allowlist, tracked=["src/new.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {("src/new.rs", "disallowed_methods"): 1})

    def test_fails_above_the_allowlisted_maximum(self):
        write(self.root, "src/new.rs", f"fn f() {{\n    {EXPECT_ATTR}}}\nfn g() {{\n    {EXPECT_ATTR}}}\n")
        self.allowlist.write_text("src/new.rs disallowed_methods 1 bounded-poll deadline-bound retry\n", encoding="utf-8")
        errors, _current = lint_expectations.check(self.root, self.allowlist, tracked=["src/new.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("exceeds its scripts/lint-expectations.txt maximum of 1", errors[0])

    def test_fails_on_stale_entry_whose_site_is_gone(self):
        write(self.root, "src/clean.rs", "pub fn f() {}\n")
        self.allowlist.write_text("src/clean.rs disallowed_methods 1 bounded-poll was a retry\n", encoding="utf-8")
        errors, _current = lint_expectations.check(self.root, self.allowlist, tracked=["src/clean.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("matches no tracked file", errors[0])

    def test_fails_when_allowlisted_count_drops(self):
        write(self.root, "src/new.rs", f"fn f() {{\n    {EXPECT_ATTR}}}\n")
        self.allowlist.write_text("src/new.rs disallowed_methods 3 bounded-poll deadline-bound retry\n", encoding="utf-8")
        errors, _current = lint_expectations.check(self.root, self.allowlist, tracked=["src/new.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("lower the recorded maximum", errors[0])

    def test_rejects_an_unknown_lint(self):
        self.allowlist.write_text("src/x.rs disallowed_debug 1 bounded-poll reason\n", encoding="utf-8")
        with self.assertRaises(lint_expectations.AllowlistError):
            lint_expectations.check(self.root, self.allowlist, tracked=[])

    def test_rejects_an_unknown_category(self):
        self.allowlist.write_text("src/x.rs disallowed_methods 1 nonsense reason\n", encoding="utf-8")
        with self.assertRaises(lint_expectations.AllowlistError):
            lint_expectations.check(self.root, self.allowlist, tracked=[])

    def test_exempts_crate_level_cfg_attr_test_exception(self):
        content = (
            "#![cfg_attr(\n"
            "    test,\n"
            "    expect(\n"
            "        clippy::disallowed_methods,\n"
            '        reason = "unit-test fixtures sleep to sequence threads"\n'
            "    )\n"
            ")]\n"
            "pub fn f() {}\n"
        )
        write(self.root, "src/lib.rs", content)
        self.allowlist.write_text("", encoding="utf-8")
        errors, current = lint_expectations.check(self.root, self.allowlist, tracked=["src/lib.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {})

    def test_exempts_cfg_test_module(self):
        content = (
            "pub fn f() {}\n\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    use super::*;\n\n"
            "    #[test]\n"
            "    fn g() {\n"
            f"        {EXPECT_ATTR}"
            "    }\n"
            "}\n"
        )
        write(self.root, "src/lib.rs", content)
        self.allowlist.write_text("", encoding="utf-8")
        errors, current = lint_expectations.check(self.root, self.allowlist, tracked=["src/lib.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {})

    def test_exempts_tests_directory(self):
        write(self.root, "tests/smoke.rs", f"fn f() {{\n    {EXPECT_ATTR}}}\n")
        self.allowlist.write_text("", encoding="utf-8")
        errors, current = lint_expectations.check(self.root, self.allowlist, tracked=["tests/smoke.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {})

    def test_exempts_benches_directory(self):
        write(self.root, "benches/hot.rs", f"fn f() {{\n    {EXPECT_ATTR}}}\n")
        self.allowlist.write_text("", encoding="utf-8")
        errors, current = lint_expectations.check(self.root, self.allowlist, tracked=["benches/hot.rs"])
        self.assertEqual(errors, [])
        self.assertEqual(current, {})

    def test_does_not_exempt_a_sibling_function_after_a_cfg_test_module(self):
        content = (
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn g() {}\n"
            "}\n\n"
            "fn h() {\n"
            f"    {EXPECT_ATTR}"
            "}\n"
        )
        write(self.root, "src/lib.rs", content)
        self.allowlist.write_text("", encoding="utf-8")
        errors, _current = lint_expectations.check(self.root, self.allowlist, tracked=["src/lib.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("unexplained clippy::disallowed_methods", errors[0])

    def test_distinguishes_disallowed_methods_and_disallowed_types(self):
        content = (
            "fn f() {\n"
            "    #[expect(\n"
            "        clippy::disallowed_types,\n"
            '        reason = "needs the concrete type at this boundary"\n'
            "    )]\n"
            "    let _x: std::collections::HashMap<u8, u8> = Default::default();\n"
            "}\n"
        )
        write(self.root, "src/lib.rs", content)
        self.allowlist.write_text("", encoding="utf-8")
        errors, _current = lint_expectations.check(self.root, self.allowlist, tracked=["src/lib.rs"])
        self.assertEqual(len(errors), 1)
        self.assertIn("disallowed_types", errors[0])


if __name__ == "__main__":
    unittest.main()
