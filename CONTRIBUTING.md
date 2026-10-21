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

   The trainer tool's bundled Vorbis C sources trip clang-cl's
   `incompatible-pointer-types` error under `cargo xwin` (native MSVC only
   warns), and its time-stretch binding runs bindgen, so Windows builds need
   LLVM's `libclang` (set `LIBCLANG_PATH` if it is not found). Run the
   cross-check as:

   ```sh
   env -u C_INCLUDE_PATH 'CFLAGS_x86_64-pc-windows-msvc=-Wno-error=incompatible-pointer-types' \
     cargo xwin clippy --workspace --all-targets --all-features --locked \
     --target x86_64-pc-windows-msvc -- -D warnings
   ```

   The hyphenated variable name is required: `cargo xwin` overwrites the
   underscore spelling.

3. Install Python 3.11 or newer for the repository checks in `scripts/`
   (they read TOML with the standard-library `tomllib`).
4. Clone, build, and run:

   ```sh
   git clone https://github.com/shiruviaa/vosu.git
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

For Linux release packaging, install ImageMagick, `rpm`, `dpkg-deb`, and
`bsdtar` (Debian/Ubuntu: `imagemagick rpm libarchive-tools`; `dpkg-deb` is in
`dpkg`), plus nfpm **2.47.0** using the verified archive/hash in
[release.yml](.github/workflows/release.yml). First create the portable Linux
ZIP with `scripts/package-release.py package`, then use
`scripts/package-linux.py build` against the same binary and output directory.
Build release binaries on Ubuntu 22.04, not the developer's newer glibc host.
The [release runbook](docs/operations/runbook.md#linux-distribution-releases)
describes package inspection, container smoke checks, and the `vosu-git` AUR
package.


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
cargo test --workspace --all-features --locked
python scripts/check-doc-links.py
python scripts/assemble-release-license.py
python scripts/check-asset-hashes.py
python -m unittest discover -s scripts -p test_package_release.py
```

For a narrower loop, [Testing and verification](docs/testing/verification.md#fast-selection-by-change)
maps each kind of change to its test targets. Changes to process discovery,
memory reading, or the updater also need the native tests
(`process_memory`, `updater_native`) on each affected OS. CI covers Windows
with an MSVC cross-check and a native Windows job.

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
