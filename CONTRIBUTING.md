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
3. Install Python 3 for the repository checks in `scripts/`.
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
python scripts/check-provenance.py --package-fixtures
```

For a narrower loop, [Testing and verification](docs/testing/verification.md#fast-selection-by-change)
maps each kind of change to its test targets. Changes to process discovery,
memory reading, or the updater also need the native tests
(`process_memory`, `updater_native`) on each affected OS. CI covers Windows
with an MSVC cross-check and a native Windows job.

Keep these in the same pull request:

- Documentation: README for user-visible contracts, `docs/` for architecture
  and workflows, and `CHANGELOG.md` for notable user-visible changes.
- Integrity manifests: changes to `assets/` need the new hash in
  `assets/SHA256SUMS`. The `homepage.css`/`homepage.html` hashes are also stored
  in `Cargo.toml` metadata. After any `Cargo.toml` or `Cargo.lock` change, run
  `python scripts/check-provenance.py --refresh-dependency-inventory`. The
  provenance check names any stale entry.
- Public contracts: routes, payload shapes, status codes, and WebSocket paths
  stay exact unless the change deliberately updates their tests and docs.

Provenance-pinned trees (`crates/vosu-pp/rosu-*-api/`, `tools/oracle/reference/`,
calculator fixtures) change only through their owning tooling; do not reformat
them by hand. The product name is **vosu**. The former name `tosu` appears only in
migration code, upstream references, and history.

Coding agents follow [AGENTS.md](AGENTS.md), which adds the full local CI and
push gate.
