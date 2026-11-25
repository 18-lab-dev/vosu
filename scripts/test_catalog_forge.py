"""Pure checks for scripts/test-catalog-forge.py (the D15 real-forge gate).

The gate itself needs containers; these tests pin what must not drift
without them: the exact test inventory, the trusted/untrusted container
difference, zero-test refusal, secret redaction and the provisioned redirects.
"""

import contextlib
import importlib.util
import io
from pathlib import Path
import re
import unittest
import unittest.mock

ROOT = Path(__file__).resolve().parent.parent
TESTS = ROOT / "crates/vosu-test-harness/tests/network_integration"

_spec = importlib.util.spec_from_file_location("test_catalog_forge_runner", ROOT / "scripts/test-catalog-forge.py")
forge = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(forge)


def ignored_tests():
    """`module::name` for every #[ignore]d test in the D15 test modules."""
    found = []
    sources = {
        "forge::trusted": TESTS / "forge/trusted.rs",
        "forge::untrusted": TESTS / "forge/untrusted.rs",
    }
    for module, path in sources.items():
        found += [f"{module}::{name}" for name in re.findall(r"#\[ignore[^\]]*\]\s*fn (\w+)", path.read_text())]
    os_store = (TESTS / "os_store.rs").read_text()
    for module, body in re.findall(r"mod (trusted|untrusted) \{(.*?)\n\}", os_store, re.DOTALL):
        found += [f"os_store::{module}::{name}" for name in re.findall(r"#\[ignore[^\]]*\]\s*fn (\w+)", body)]
    return sorted(found)


class InventoryTest(unittest.TestCase):
    def test_expected_names_match_the_ignored_rust_tests(self):
        self.assertEqual(ignored_tests(), sorted(forge.EXPECTED_TRUSTED + forge.EXPECTED_UNTRUSTED))

    def test_each_phase_filter_selects_exactly_its_tests(self):
        everything = forge.EXPECTED_TRUSTED + forge.EXPECTED_UNTRUSTED
        for filters, expected in list(forge.PHASES.values()) + list(forge.WINDOWS_PHASES.values()):
            selected = sorted(name for name in everything if any(f in name for f in filters))
            self.assertEqual(selected, sorted(expected))
            self.assertTrue(expected)

    def test_provisioning_constants_match_the_rust_fixture_contract(self):
        rust = dict(re.findall(r'pub const (\w+): &str = "([^"]*)";', (TESTS / "fixture.rs").read_text()))
        for name in ("OWNER", "PRIVATE_REPO", "PRIVATE_TAG", "SAME_ORIGIN_REPO", "SAME_ORIGIN_TAG",
                     "CROSS_PORT_REPO", "CROSS_PORT_TAG", "FOREIGN_REDIRECT_REPO", "FOREIGN_REDIRECT_TAG",
                     "FOREIGN_ASSET_REPO", "PACKAGE_ASSET", "ICON_ASSET", "TOOL_ID", "TOOL_VERSION"):
            self.assertEqual(rust[name], getattr(forge, name), name)

    def test_forge_image_is_pinned_by_digest(self):
        self.assertRegex(forge.FORGE_IMAGE, r"^codeberg\.org/forgejo/forgejo:[\w.-]+@sha256:[0-9a-f]{64}$")
        self.assertIn(forge.FORGE_VERSION_PREFIX, forge.FORGE_IMAGE)


class ContainerCommandTest(unittest.TestCase):
    def command(self, engine, phase):
        resources = forge.names("abcd1234")
        return forge.phase_command(engine, "localhost/runner:1", resources, phase, Path("/tmp/x1/fixture"),
                                   Path("/target/network_integration-1"))

    def test_only_the_trusted_phase_installs_the_ca_into_the_os_store(self):
        trusted = self.command("podman", "trusted")
        untrusted = self.command("podman", "untrusted")
        store_mount = f"/tmp/x1/fixture/ca.crt:{forge.CA_STORE_PATH}:ro"
        self.assertIn(store_mount, trusted)
        self.assertNotIn(store_mount, untrusted)
        # Same image, aliases and store refresh; no custom root variables.
        for command in (trusted, untrusted):
            self.assertIn("localhost/runner:1", command)
            for host in (forge.FORGE_HOST, forge.MIRROR_HOST, forge.WRONG_HOST):
                self.assertIn(host, command)
            self.assertIn("update-ca-certificates --fresh", command[-1])
            self.assertNotIn("SSL_CERT", " ".join(command))
        self.assertIn("forge::trusted:: os_store::trusted::", trusted[-1])
        self.assertIn("forge::untrusted:: os_store::untrusted::", untrusted[-1])

    def test_engines_share_one_command_shape(self):
        for phase in ("trusted", "untrusted"):
            self.assertEqual(self.command("podman", phase)[1:], self.command("docker", phase)[1:])

    def test_every_resource_is_task_owned(self):
        self.assertTrue(all(name.startswith("vosu-x1-abcd1234-") for name in forge.names("abcd1234").values()))


class ResultTest(unittest.TestCase):
    def output(self, names):
        lines = [f"running {len(names)} tests"] + [f"test {name} ... ok" for name in names]
        lines.append(f"test result: ok. {len(names)} passed; 0 failed; 0 ignored; 0 measured; 5 filtered out")
        return "\n".join(lines)

    def test_exact_pass_is_counted(self):
        self.assertEqual(forge.check_results(self.output(forge.EXPECTED_UNTRUSTED), forge.EXPECTED_UNTRUSTED), 2)

    def test_zero_tests_and_drift_fail(self):
        for output, expected in (
            (self.output([]), forge.EXPECTED_UNTRUSTED),
            (self.output(forge.EXPECTED_UNTRUSTED[:1]), forge.EXPECTED_UNTRUSTED),
            (self.output(forge.EXPECTED_UNTRUSTED + ("extra::test",)), forge.EXPECTED_UNTRUSTED),
            (self.output([]), ()),
        ):
            with self.assertRaises(forge.GateError):
                forge.check_results(output, expected)

    def test_listing_parser(self):
        self.assertEqual(forge.listed_tests("b::x: test\na::y: test\n\n2 tests, 0 benchmarks"), ["a::y", "b::x"])

    def test_secrets_are_found_and_redacted(self):
        sentinels = {"token": "0123abcd", "password": "pw-sentinel", "username": ""}
        text = "header token 0123abcd and pw-sentinel"
        self.assertEqual(forge.find_secrets(text, sentinels), ["token", "password"])
        redacted = forge.redact(text, sentinels)
        self.assertEqual(forge.find_secrets(redacted, sentinels), [])
        self.assertIn("<redacted token>", redacted)


class ProvisioningPlanTest(unittest.TestCase):
    def test_redirects_are_issued_by_the_real_forge_from_the_source_origin(self):
        plan = {repo: dict((name, (kind, value)) for name, kind, value in assets)
                for repo, _, assets in forge.release_plan()}
        origin = f"https://{forge.FORGE_HOST}/"
        for repo in (forge.SAME_ORIGIN_REPO, forge.CROSS_PORT_REPO, forge.FOREIGN_REDIRECT_REPO):
            kind, url = plan[repo][forge.CATALOG_ASSET]
            self.assertEqual(kind, "external")
            self.assertTrue(url.startswith(origin + forge.OWNER + f"/{repo}/releases/download/"), url)
        self.assertTrue(plan[forge.SAME_ORIGIN_REPO][forge.HOP_ASSET][1].startswith(origin))
        self.assertTrue(plan[forge.CROSS_PORT_REPO][forge.HOP_ASSET][1].startswith(
            f"https://{forge.FORGE_HOST}:{forge.CROSS_PORT}/x1/return/"))
        self.assertTrue(plan[forge.FOREIGN_REDIRECT_REPO][forge.HOP_ASSET][1].startswith(
            f"https://{forge.MIRROR_HOST}/"))
        self.assertTrue(plan[forge.FOREIGN_ASSET_REPO][forge.CATALOG_ASSET][1].startswith(
            f"https://{forge.MIRROR_HOST}/"))
        self.assertEqual(plan[forge.PRIVATE_REPO][forge.CATALOG_ASSET][0], "file")

    def test_catalog_lists_package_and_art_on_the_private_forge_host(self):
        package = forge.package_bytes("abcd1234")
        tool = forge.catalog_document(package)["tools"][0]
        self.assertEqual(tool["packages"]["any"]["size"], len(package))
        for url in (tool["packages"]["any"]["url"], tool["icon_url"]):
            self.assertTrue(url.startswith(f"https://{forge.FORGE_HOST}/"), url)


class PrerequisiteTest(unittest.TestCase):
    def test_missing_engine_fails_instead_of_skipping(self):
        stderr = io.StringIO()
        with unittest.mock.patch.object(forge.shutil, "which", return_value=None), \
                unittest.mock.patch.object(forge.signal, "signal"), \
                contextlib.redirect_stderr(stderr):
            code = forge.main(["--engine", "podman", "--artifacts", str(Path(self.tmp()) / "artifacts")])
        self.assertEqual(code, 1)
        self.assertIn("fails rather than skipping", stderr.getvalue())

    def tmp(self):
        import tempfile

        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        return directory.name


if __name__ == "__main__":
    unittest.main()
