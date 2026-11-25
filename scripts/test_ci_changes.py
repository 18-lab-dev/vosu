"""Exercise ci-changes.py's path classification, in particular the M0
schema-only exemption that moved from src/calculation/ to the extracted
vosu-osu-model/vosu-osu-calc crates."""

import importlib.util
from pathlib import Path
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ci_changes = _load("ci_changes", "ci-changes.py")


class ClassifyTests(unittest.TestCase):
    def test_docs_only_change_is_not_code(self):
        code, native, ui = ci_changes.classify(["docs/testing/benchmarks.md", "README.md"])
        self.assertFalse(code)
        self.assertFalse(native)
        self.assertFalse(ui)

    def test_vosu_osu_model_edit_stays_schema_only(self):
        code, native, ui = ci_changes.classify(["crates/vosu-osu-model/src/enums.rs"])
        self.assertTrue(code)
        self.assertFalse(native)
        self.assertFalse(ui)

    def test_vosu_osu_calc_edit_stays_schema_only(self):
        code, native, ui = ci_changes.classify(["crates/vosu-osu-calc/src/engine.rs"])
        self.assertTrue(code)
        self.assertFalse(native)
        self.assertFalse(ui)

    def test_src_model_api_edit_stays_schema_only(self):
        code, native, ui = ci_changes.classify(["src/model/api/v1.rs"])
        self.assertTrue(code)
        self.assertFalse(native)
        self.assertFalse(ui)

    def test_vosu_osu_reader_edit_triggers_native(self):
        code, native, ui = ci_changes.classify(["crates/vosu-osu-reader/src/memory/stable.rs"])
        self.assertTrue(code)
        self.assertTrue(native)
        self.assertFalse(ui)

    def test_vosu_sys_edit_triggers_native(self):
        code, native, ui = ci_changes.classify(["crates/vosu-sys/src/process.rs"])
        self.assertTrue(code)
        self.assertTrue(native)

    def test_vosu_core_edit_triggers_native(self):
        code, native, ui = ci_changes.classify(["crates/vosu-core/src/config.rs"])
        self.assertTrue(code)
        self.assertTrue(native)

    def test_server_edit_is_ui_and_native(self):
        code, native, ui = ci_changes.classify(["src/server/pages.rs"])
        self.assertTrue(code)
        self.assertTrue(native)
        self.assertTrue(ui)

    def test_tools_edit_is_ui(self):
        code, _native, ui = ci_changes.classify(["src/tools/mod.rs"])
        self.assertTrue(ui)

    def test_server_assets_edit_is_ui(self):
        code, _native, ui = ci_changes.classify(["assets/server/index.html"])
        self.assertTrue(ui)

    def test_render_file_under_runtime_is_ui(self):
        code, _native, ui = ci_changes.classify(["src/runtime/render.rs"])
        self.assertTrue(ui)

    def test_non_render_runtime_file_is_not_ui(self):
        code, _native, ui = ci_changes.classify(["src/runtime/worker.rs"])
        self.assertFalse(ui)

    def test_smoke_test_file_is_ui(self):
        code, _native, ui = ci_changes.classify(["tests/dashboard_smoke.rs"])
        self.assertTrue(ui)

    def test_mixed_change_unions_every_flag(self):
        code, native, ui = ci_changes.classify(
            ["crates/vosu-osu-model/src/enums.rs", "src/server/routes.rs"]
        )
        self.assertTrue(code)
        self.assertTrue(native)
        self.assertTrue(ui)


class SdkChangedTests(unittest.TestCase):
    def test_sdk_sources_and_workspace_inputs_need_the_package_check(self):
        for path in [
            "crates/vosu-plugin-api/src/manifest/files.rs",
            "crates/vosu-plugin-api/README.md",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".github/workflows/ci.yml",
            "scripts/sdk-package.py",
            "scripts/prepush.py",
        ]:
            self.assertTrue(ci_changes.sdk_changed([path]), path)

    def test_daemon_and_other_crate_changes_skip_the_package_check(self):
        for path in [
            "crates/vosu-core/src/catalog/install/prepare.rs",
            "crates/vosu-core/Cargo.toml",
            "crates/vosu-plugin-internal/src/glue.rs",
            "src/server/pages.rs",
            "docs/v2/catalog-v2.md",
            "scripts/test_sdk_package.py",
        ]:
            self.assertFalse(ci_changes.sdk_changed([path]), path)

    def test_any_sdk_path_in_a_mixed_change_runs_it(self):
        self.assertTrue(ci_changes.sdk_changed(["src/server/pages.rs", "Cargo.lock"]))
        self.assertFalse(ci_changes.sdk_changed([]))


if __name__ == "__main__":
    unittest.main()
