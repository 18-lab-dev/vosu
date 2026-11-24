"""Exercise check-crate-boundaries.py against fabricated `cargo metadata` graphs."""

import importlib.util
from pathlib import Path
import tempfile
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


crate_boundaries = _load("check_crate_boundaries", "check-crate-boundaries.py")


def package(name, deps):
    """A minimal `cargo metadata` package node: deps is a list of
    (dep_name, is_path_dependency) pairs."""
    dependencies = [
        {"name": dep_name, "path": "/workspace/somewhere"} if is_path else {"name": dep_name}
        for dep_name, is_path in deps
    ]
    return {
        "id": f"id:{name}",
        "name": name,
        "manifest_path": f"/workspace/{name}/Cargo.toml",
        "dependencies": dependencies,
    }


def metadata(packages):
    return {
        "workspace_members": [p["id"] for p in packages],
        "packages": packages,
    }


class CrateBoundariesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.rules = Path(self.temporary.name) / "rules.txt"

    def test_passes_when_edges_and_crates_match(self):
        meta = metadata(
            [
                package("alpha", [("beta", True), ("serde", False)]),
                package("beta", []),
            ]
        )
        self.rules.write_text("beta\nalpha -> beta\n", encoding="utf-8")
        errors, edges, names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(errors, [])
        self.assertEqual(edges, {("alpha", "beta")})
        self.assertEqual(names, {"alpha", "beta"})

    def test_fails_on_an_unlisted_crate(self):
        meta = metadata([package("alpha", []), package("beta", [])])
        self.rules.write_text("alpha\n", encoding="utf-8")
        errors, _edges, _names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(len(errors), 1)
        self.assertIn("'beta' is not listed", errors[0])

    def test_fails_on_a_new_edge(self):
        meta = metadata([package("alpha", [("beta", True)]), package("beta", [])])
        self.rules.write_text("alpha\nbeta\n", encoding="utf-8")
        errors, _edges, _names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(len(errors), 1)
        self.assertIn("new crate edge alpha -> beta", errors[0])

    def test_fails_on_a_stale_edge(self):
        meta = metadata([package("alpha", []), package("beta", [])])
        self.rules.write_text("alpha -> beta\n", encoding="utf-8")
        errors, _edges, _names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(len(errors), 1)
        self.assertIn("alpha -> beta is no longer a real Cargo dependency", errors[0])

    def test_fails_on_a_listed_crate_that_no_longer_exists(self):
        meta = metadata([package("alpha", [])])
        self.rules.write_text("alpha\nghost\n", encoding="utf-8")
        errors, _edges, _names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(len(errors), 1)
        self.assertIn("'ghost' is listed but is not a workspace crate", errors[0])

    def test_non_path_dependencies_are_not_edges(self):
        meta = metadata([package("alpha", [("beta", False)]), package("beta", [])])
        self.rules.write_text("alpha\nbeta\n", encoding="utf-8")
        errors, edges, _names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(errors, [])
        self.assertEqual(edges, set())

    def test_excludes_provenance_pinned_manifest_dirs(self):
        pinned = package("vosu-rosu-map", [])
        pinned["manifest_path"] = str(
            crate_boundaries.ROOT / "crates/vosu-pp/rosu-map-api/Cargo.toml"
        )
        meta = metadata([package("alpha", []), pinned])
        self.rules.write_text("alpha\n", encoding="utf-8")
        errors, _edges, names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(errors, [])
        self.assertEqual(names, {"alpha"})

    def test_m4a_rejects_production_adapter_state_even_if_recorded(self):
        for target in ["vosu-core", "vosu-osu", "vosu-osu-state", "vosu-osu-reader", "vosu-sys"]:
            for kind in [None, "build"]:
                with self.subTest(target=target, kind=kind):
                    adapter = package("vosu-adapter-upstream", [(target, True)])
                    adapter["dependencies"][0]["kind"] = kind
                    meta = metadata([adapter, package(target, [])])
                    self.rules.write_text(f"vosu-adapter-upstream -> {target}\n", encoding="utf-8")
                    errors, _, _ = crate_boundaries.check(meta, rules_path=self.rules)
                    self.assertTrue(any("production adapter edge" in error for error in errors))
                    adapter["dependencies"][0]["kind"] = "dev"
                    errors, _, _ = crate_boundaries.check(meta, rules_path=self.rules)
                    self.assertEqual(errors, [])

    def test_m4a_rejects_osu_surface_and_impure_support_even_if_recorded(self):
        for source, target in [("vosu-osu", "vosu-surface"), ("vosu-adapter-support", "vosu-surface")]:
            meta = metadata([package(source, [(target, True)]), package(target, [])])
            self.rules.write_text(f"{source} -> {target}\n", encoding="utf-8")
            errors, _, _ = crate_boundaries.check(meta, rules_path=self.rules)
            self.assertTrue(any("M4a:" in error for error in errors))

    def test_rules_file_comments_and_blank_lines_are_ignored(self):
        meta = metadata([package("alpha", [("beta", True)]), package("beta", [])])
        self.rules.write_text(
            "# a comment\nalpha -> beta  # trailing note\n\nbeta\n", encoding="utf-8"
        )
        errors, _edges, _names = crate_boundaries.check(meta, rules_path=self.rules)
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
