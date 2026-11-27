#!/usr/bin/env python3
"""Add only the M7 D17 harness to a disposable checkout of pre-M7 vosu."""

import argparse
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
BASE = "33f957de"


def git(checkout, *args):
    return subprocess.check_output(["git", "-C", checkout, *args], text=True).strip()


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise SystemExit(f"pre-M7 source anchor is missing or ambiguous: {old!r}")
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkout", type=Path, help="clean disposable ovnfs checkout at 33f957de")
    baseline = parser.parse_args().checkout.resolve(strict=True)
    if baseline == ROOT or git(baseline, "rev-parse", "HEAD") != git(baseline, "rev-parse", BASE):
        parser.error(f"a separate checkout at {BASE} is required")
    if git(baseline, "status", "--porcelain"):
        parser.error("the baseline checkout must be clean")

    compose = "    async fn compose(&mut self) -> io::Result<()> {\n"
    runtime = (baseline / "src/runtime/lifecycle.rs").read_text()
    runtime = replace_once(runtime, compose, compose + """\
        let broker = self.paths.broker_path().map(Some);
        self.compose_with_discovery(broker, |host, logger| {
            discovery::start_discovery(host, Handle::current(), logger)
        }).await
    }

    async fn compose_with_discovery(
        &mut self,
        broker: io::Result<Option<PathBuf>>,
        start_discovery: impl FnOnce(&Arc<PluginHost>, &Logger) -> Option<vosu_core::plugin::DiscoveryService>,
    ) -> io::Result<()> {
""")
    runtime = replace_once(runtime, "            self.paths.broker_path().map(Some),\n", "            broker,\n")
    runtime = replace_once(
        runtime,
        "        let discovery = discovery::start_discovery(&self.plugins, Handle::current(), &self.logger)\n",
        "        let discovery = start_discovery(&self.plugins, &self.logger)\n",
    )
    runtime += '\n#[path = "m7_bench_daemon.rs"]\nmod m7_bench_daemon;\npub use m7_bench_daemon::TestDaemon;\n'
    exports = (baseline / "src/runtime.rs").read_text() + "\n#[doc(hidden)]\npub use lifecycle::TestDaemon;\n"

    network = (baseline / "crates/vosu-core/src/config/network.rs").read_text()
    network = replace_once(
        network,
        """\
        let conflicts = |host: &str, port: f64| {
            candidate.dashboard_port == port && overlapping_hosts(&candidate.dashboard_ip, host)
        };
        if conflicts(&candidate.server_ip, candidate.server_port)
            || conflicts(&current.server_ip, current.server_port)
            || (candidate.server_port == current.dashboard_port
                && overlapping_hosts(&candidate.server_ip, &current.dashboard_ip))""",
        """\
        let same_port = |a: f64, b: f64| a != 0.0 && a == b;
        let conflicts = |host: &str, port: f64| {
            same_port(candidate.dashboard_port, port)
                && overlapping_hosts(&candidate.dashboard_ip, host)
        };
        if conflicts(&candidate.server_ip, candidate.server_port)
            || conflicts(&current.server_ip, current.server_port)
            || (same_port(candidate.server_port, current.dashboard_port)
                && overlapping_hosts(&candidate.server_ip, &current.dashboard_ip))""",
    )

    registration = (baseline / "benches/state_bus_bench.rs").read_text()
    registration = '#[path = "support/m7_e2e.rs"]\nmod m7_e2e;\n' + replace_once(
        registration, "    adapter_e2e::bench(c);\n", "    adapter_e2e::bench(c);\n    m7_e2e::bench(c);\n",
    )
    files = {
        "crates/vosu-core/src/config/network.rs": network,
        "src/runtime/lifecycle.rs": runtime,
        "src/runtime.rs": exports,
        "src/runtime/m7_bench_daemon.rs": (ROOT / "benches/support/m7/pre_m7_daemon.rs").read_text(),
        "src/runtime/m7_bench_paths.rs": (ROOT / "crates/vosu-core/src/runtime/test_paths.rs").read_text(),
        "benches/state_bus_bench.rs": registration,
        "benches/support/m7_e2e.rs": (ROOT / "benches/support/m7_e2e.rs").read_text(),
        "benches/support/m7/tool.rs": (ROOT / "benches/support/m7/tool.rs").read_text(),
        "benches/support/m7/daemon.rs": """\
pub use vosu::runtime::TestDaemon;

pub async fn new(root: &std::path::Path) -> TestDaemon {
    TestDaemon::new(root).await
}
""",
    }
    # Use the same schemas and fixture payloads on both revisions.
    for name in ("manifest.toml", "args.json", "result.json", "namespace.json"):
        path = f"tests/fixtures/api-v1-plugin/{name}"
        files[path] = (ROOT / path).read_text()

    for path, contents in files.items():
        destination = baseline / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(contents)
    print(f"Prepared pre-M7 D17 harness at {baseline} (build and measure separately).")


if __name__ == "__main__":
    main()
