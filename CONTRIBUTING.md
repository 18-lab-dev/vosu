# Contributing to vosu

vosu is one Rust daemon with a vendored HTML/CSS/JS dashboard. A debug build
needs only Rust and a native C toolchain; no Node.js, Electron, or .NET.

## Set up

1. Install [rustup](https://rustup.rs/). The first `cargo` command in the
   checkout installs the pinned toolchain from `rust-toolchain.toml` (Rust
   1.96.1 with rustfmt, Clippy, and the Linux and Windows targets).
2. Install the native build dependencies:

   | Platform | Command |
   | --- | --- |
   | Debian / Ubuntu | `sudo apt install build-essential clang pkg-config libelf-dev zlib1g-dev libbpf-dev` |
   | Arch Linux | `sudo pacman -S --needed base-devel clang libelf zlib libbpf` |
   | Windows | Visual Studio Build Tools with the **Desktop development with C++** workload (MSVC and the Windows SDK) |

   Clang, libelf, zlib, and libbpf compile the default `linux-process-events`
   eBPF backend. Building with `--no-default-features` skips them and produces a
   polling-only Linux binary.

   On Debian and Ubuntu, the eBPF compile also needs the multiarch kernel
   headers on its include path (`asm/types.h` lives under
   `/usr/include/x86_64-linux-gnu`), as CI sets for its Linux Clippy and test
   steps:

   ```sh
   export C_INCLUDE_PATH=/usr/include/x86_64-linux-gnu
   ```

   Set it only for Linux-target builds and checks. Leave it unset for the
   Windows MSVC cross-check (`cargo xwin`); CI keeps it out of that step too.

   Run the Windows cross-check without Linux include overrides:

   ```sh
   env -u C_INCLUDE_PATH cargo xwin clippy --workspace --all-targets --all-features --locked \
     --target x86_64-pc-windows-msvc -- -D warnings
   ```

3. Install the pinned test runner:

   ```sh
   cargo install --locked cargo-nextest --version 0.9.152
   ```

   CI and `scripts/prepush.py`/`scripts/native-ci.ps1` use this exact pin
   (via `taiki-e/install-action` on hosted CI, a plain pinned `cargo install`
   on this host, and the same pinned `cargo install` into the Windows VM's
   `CARGO_HOME` for `native-ci.ps1`). `cargo nextest run` does not execute
   doctests; run those with `cargo test --doc`.

4. Install Python 3.11 or newer for the repository checks in `scripts/`
   (they read TOML with the standard-library `tomllib`).
5. Clone, build, and run:

   ```sh
   git clone https://github.com/18-lab-dev/vosu.git
   cd vosu
   cargo build --locked
   cargo run --locked
   ```

   The dashboard is at <http://127.0.0.1:24050/>. Configuration, counters, and
   caches use the platform locations listed in the
   [README](README.md#files-and-migration).

To read a running osu! client on Linux, the executable needs `CAP_SYS_PTRACE`.
Run `target/debug/vosu --setup` as your normal user; it asks for authentication
through polkit. Every rebuild replaces the binary and drops the capability, so
repeat it before testing against a live client. Never run the daemon as root.

A debug binary has no embedded offset-generator bundle. It works normally until
a lazer version needs local offset generation, which then reports a
missing-bundle error. Release builds and the bundle (Python and the .NET 8 SDK)
are covered in the [README](README.md#build-from-source).

For Linux release packaging, install `rpm`, `dpkg-deb`, and `bsdtar`
(Debian/Ubuntu: `rpm libarchive-tools`; `dpkg-deb` is in `dpkg`), plus nfpm
**2.47.0** using the verified archive/hash in
[release.yml](.github/workflows/release.yml). First create the portable Linux
ZIP with `scripts/package-release.py package`, then use
`scripts/package-linux.py build` against the same binary and output directory.
Build release binaries on Ubuntu 22.04, not the developer's newer glibc host.
The [release runbook](docs/operations/runbook.md#linux-distribution-releases)
describes package inspection, container smoke checks, and the `vosu-git` AUR
package.

## Speed up the local build loop

`scripts/dev-env.sh` is an optional, opt-in `sccache` wrapper for Linux debug
builds and gates. It is not enabled by default, not set by CI, and not
written into any committed Cargo config.

```sh
cargo install --locked sccache --version 0.11.0   # once, if not already on PATH
source scripts/dev-env.sh                         # exports RUSTC_WRAPPER=sccache
```

This is worthwhile mainly when several checkouts (e.g. multiple `ovnfs`
worktrees, or several coding agents) build the same commit: a shared
`sccache` cache (`~/.cache/sccache` by default; set `SCCACHE_DIR` to share one
across checkouts explicitly) skips recompiling dependencies that an earlier
checkout already built with the same flags. Measured on 2026-11-23, a cold
gate build (`cargo clippy --workspace --all-targets --all-features`,
`cargo nextest run --workspace ... --no-run`) in a fresh checkout against a
warm shared cache ran 25% faster than the same cold build with no cache; see
[benchmarks](docs/testing/benchmarks.md#local-dev-build-speed-up-tools-sccache-and-mold)
for the full numbers and caveats. `prepush.py` and `measure-build.py` already
inherit the full process environment, so no change to either script was
needed for them to honor it.

`sccache` cannot cache incrementally-compiled crates (this repository's own
workspace crates, under the default dev profile), so it speeds up dependency
rebuilds across checkouts, not single-crate edit/rebuild cycles within one
checkout. A `mold`/`lld` linker override was also measured and was not worth
adopting; see the same benchmark section for why.

## Repository map

| Path | Contents |
| --- | --- |
| `src/` | The daemon: process discovery, memory reads, state, APIs, server, config, counters, updater |
| `assets/server/` | Vendored dashboard (HTML/CSS/JS), embedded at build time |
| `crates/vosu-pp/` | Performance-calculation facade over provenance-pinned `rosu-*` forks |
| `crates/vosu-test-harness/` | Mock network fixture behind the development-only `test-harness` feature |
| `crates/vosu-release-smoke/` | Smoke test that release CI runs against the packaged executable |
| `tests/`, `benches/`, `examples/` | Compatibility, native, and smoke contracts; Criterion benchmarks; process-event benchmarks |
| `tools/` | Maintainer tooling: live parity, calculator oracle, offset generator, input validation, benchmark harnesses |
| `scripts/` | CI checks and release packaging |
| `docs/` | Architecture, decisions, workflows, operations, and verification guides |

[docs/architecture/source-map.md](docs/architecture/source-map.md) names the
owner of each concern.

## Before you open a pull request

Run the checks that CI runs on Linux:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --lib --bins --tests --examples --all-features --locked
cargo test --workspace --doc --all-features --locked
python scripts/check-doc-links.py
python scripts/check-file-length.py
python scripts/check-brand.py
python scripts/check-module-boundaries.py
python scripts/assemble-release-license.py
python scripts/check-asset-hashes.py
python -m unittest discover -s scripts -p 'test_*.py'
```

Clippy enforces the repository's rules, not only style. A suppression must be
`#[expect(lint, reason = "...")]`; every `unsafe` block needs a `// SAFETY:`
comment; `clippy.toml` rejects direct file writes, unbounded channels, and
blocking sleeps with a message naming the replacement. The ratchet scripts
(`check-file-length.py`, `check-brand.py`, `check-module-boundaries.py`) fail on
files over 1,000 lines, old-brand mentions without an allowlist reason, and new
module dependencies; each error says how to fix it. See
[ADR 0018](docs/decisions/0018-enforce-rules-with-checks.md).

For a narrower loop, [Testing and verification](docs/testing/verification.md#fast-selection-by-change)
maps each kind of change to its test targets. Changes to process discovery,
memory reading, or the updater also need the native tests
(`process_memory`, `updater_native`) on each affected OS. CI covers Windows
with an MSVC cross-check and a native Windows job. Coding agents with access
to the project's Windows VM: for Windows-sensitive changes, run
`python scripts/vm-test.py [--package P] [--test T] [--lib] [-- <filter>]`
before handing off. It is a fast, narrower lever than the native phase of
`scripts/prepush.py`; it never starts the VM.

Keep these in the same pull request:

- Documentation: README for user-visible contracts, `docs/` for architecture
  and workflows, and `CHANGELOG.md` for notable user-visible changes.
- Integrity manifest: a change to a file listed in `assets/SHA256SUMS`
  (dashboard assets, branding, `offsets.json`, `LICENSE`) needs
  `python scripts/check-asset-hashes.py --write` after review.
- Release `LICENSE`: it embeds the calculator notices from
  `crates/vosu-pp/THIRD_PARTY_LICENSES.txt` and the dashboard font notices from
  `assets/server/fonts-OFL.txt`. After changing any of them, run
  `python scripts/assemble-release-license.py --write`.
- Public contracts: routes, payload shapes, status codes, and WebSocket paths
  stay exact unless the change deliberately updates their tests and docs.

Provenance-pinned trees (`crates/vosu-pp/rosu-*-api/`, `tools/oracle/reference/`,
calculator fixtures) change only through their owning tooling; do not reformat
them by hand. The product name is **vosu**. The former name `tosu` appears only in
migration code, upstream references, and history.

Coding agents follow [AGENTS.md](AGENTS.md), which adds the full local CI and
push gate.
