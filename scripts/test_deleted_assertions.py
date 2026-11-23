"""Exercise check-deleted-assertions.py's pure diff/trailer logic."""

import importlib.util
from pathlib import Path
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


deleted_assertions = _load("check_deleted_assertions", "check-deleted-assertions.py")


def diff(old_path, new_path, hunks):
    """hunks: list of (old_start, old_count, new_start, new_count, body_lines)."""
    lines = [f"diff --git a/{old_path} b/{new_path}", f"--- a/{old_path}", f"+++ b/{new_path}"]
    for old_start, old_count, new_start, new_count, body in hunks:
        lines.append(f"@@ -{old_start},{old_count} +{new_start},{new_count} @@")
        lines.extend(body)
    return "\n".join(lines) + "\n"


class ParseDiffTests(unittest.TestCase):
    def test_tracks_removed_and_added_line_numbers(self):
        text = diff(
            "src/lib.rs",
            "src/lib.rs",
            [(10, 2, 10, 1, ["-assert_eq!(a, b);", "-assert_eq!(c, d);", "+assert_eq!(a, b);"])],
        )
        files = deleted_assertions.parse_diff(text)
        self.assertEqual(len(files), 1)
        self.assertEqual(files[0]["removed"], [(10, "assert_eq!(a, b);"), (11, "assert_eq!(c, d);")])
        self.assertEqual(files[0]["added"], [(10, "assert_eq!(a, b);")])


class TestLineNumbersTests(unittest.TestCase):
    def test_whole_file_counts_under_tests_directory(self):
        numbers = deleted_assertions.test_line_numbers("tests/smoke.rs", "a\nb\nc\n")
        self.assertEqual(numbers, {1, 2, 3})

    def test_whole_file_counts_for_tests_rs_and_suffix(self):
        self.assertTrue(deleted_assertions.is_test_path("src/foo/tests.rs"))
        self.assertTrue(deleted_assertions.is_test_path("src/foo/bar_tests.rs"))
        self.assertFalse(deleted_assertions.is_test_path("src/foo/bar.rs"))

    def test_only_cfg_test_module_span_counts_in_production_file(self):
        content = "pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    fn g() {}\n}\n\nfn h() {}\n"
        numbers = deleted_assertions.test_line_numbers("src/lib.rs", content)
        self.assertEqual(numbers, {3, 4, 5, 6})


class FlaggedRemovedAssertionsTests(unittest.TestCase):
    def test_flags_a_deleted_assertion_in_a_cfg_test_module(self):
        pre = "fn f() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn g() {\n        assert_eq!(1, 2);\n    }\n}\n"
        text = diff("src/lib.rs", "src/lib.rs", [(7, 1, 7, 0, ["-        assert_eq!(1, 2);"])])
        files = deleted_assertions.parse_diff(text)
        flagged = deleted_assertions.flagged_removed_assertions(files, {"src/lib.rs": pre})
        self.assertEqual(len(flagged), 1)
        self.assertEqual(flagged[0][0], "src/lib.rs")
        self.assertIn("assert_eq!(1, 2);", flagged[0][2])

    def test_ignores_a_removed_assertion_outside_test_code(self):
        pre = "fn f() {\n    assert!(ready());\n}\n"
        text = diff("src/lib.rs", "src/lib.rs", [(2, 1, 2, 0, ["-    assert!(ready());"])])
        files = deleted_assertions.parse_diff(text)
        flagged = deleted_assertions.flagged_removed_assertions(files, {"src/lib.rs": pre})
        self.assertEqual(flagged, [])

    def test_treats_a_moved_assertion_as_not_flagged(self):
        pre = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn g() {\n        assert_eq!(1, 2);\n    }\n}\n"
        text = diff(
            "tests/a.rs",
            "tests/b.rs",
            [(5, 1, 0, 0, ["-        assert_eq!(1, 2);"])],
        ) + diff("/dev/null", "tests/b.rs", [(0, 0, 10, 1, ["+assert_eq!(1, 2);"])])
        files = deleted_assertions.parse_diff(text)
        flagged = deleted_assertions.flagged_removed_assertions(files, {"tests/a.rs": pre})
        self.assertEqual(flagged, [])

    def test_indentation_only_differences_still_count_as_a_move(self):
        pre = "#[cfg(test)]\nmod tests {\n    fn g() {\n        assert!(x);\n    }\n}\n"
        text = diff("src/lib.rs", "src/lib.rs", [(4, 1, 4, 1, ["-        assert!(x);", "+    assert!(x);"])])
        files = deleted_assertions.parse_diff(text)
        flagged = deleted_assertions.flagged_removed_assertions(files, {"src/lib.rs": pre})
        self.assertEqual(flagged, [])

    def test_a_second_duplicate_removal_without_a_second_addition_is_flagged(self):
        pre = "#[cfg(test)]\nmod tests {\n    fn g() {\n        assert!(x);\n    }\n    fn h() {\n        assert!(x);\n    }\n}\n"
        text = diff(
            "src/lib.rs",
            "src/lib.rs",
            [
                (4, 1, 4, 1, ["-        assert!(x);", "+        assert!(x);"]),
                (7, 1, 7, 0, ["-        assert!(x);"]),
            ],
        )
        files = deleted_assertions.parse_diff(text)
        flagged = deleted_assertions.flagged_removed_assertions(files, {"src/lib.rs": pre})
        self.assertEqual(len(flagged), 1)

    def test_skips_files_with_no_pre_image_content(self):
        text = diff("/dev/null", "src/new.rs", [(0, 0, 1, 1, ["+assert!(true);"])])
        files = deleted_assertions.parse_diff(text)
        flagged = deleted_assertions.flagged_removed_assertions(files, {})
        self.assertEqual(flagged, [])


class TrailerTests(unittest.TestCase):
    def test_detects_the_trailer(self):
        self.assertTrue(deleted_assertions.has_removal_trailer(["subject\n\nbody\n\nAssertions-Removed: retiring the flaky check\n"]))

    def test_rejects_a_message_without_the_trailer(self):
        self.assertFalse(deleted_assertions.has_removal_trailer(["subject\n\nremoved assertions because they were wrong\n"]))

    def test_requires_a_value_after_the_colon(self):
        self.assertFalse(deleted_assertions.has_removal_trailer(["Assertions-Removed:\n"]))


if __name__ == "__main__":
    unittest.main()
