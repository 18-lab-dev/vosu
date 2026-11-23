#!/usr/bin/env bash
# Optional local-dev build speed-up: `source` this (not execute it) so the
# exports reach your shell.
#
#   source scripts/dev-env.sh
#
# Sets RUSTC_WRAPPER=sccache when sccache is on PATH, so Cargo builds in this
# shell reuse a shared compiler cache across checkouts of the same commit
# (e.g. multiple `ovnfs` worktrees or coding agents). Never enabled by CI,
# prepush.py, or any committed Cargo config; see CONTRIBUTING.md "Speed up
# the local build loop" and docs/testing/benchmarks.md for measurements and
# caveats (it does not cache this repository's own incrementally-compiled
# crates, and a mold/lld linker override was measured and not adopted).
#
# To share one cache across checkouts explicitly, also export SCCACHE_DIR to
# a path outside any single checkout before sourcing this script.

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    echo "scripts/dev-env.sh: run as 'source scripts/dev-env.sh', not directly (the exports would be lost)" >&2
    exit 1
fi

if ! command -v sccache >/dev/null 2>&1; then
    echo "scripts/dev-env.sh: sccache not found on PATH; skipping (cargo install --locked sccache --version 0.11.0)" >&2
    return 0
fi

export RUSTC_WRAPPER=sccache
echo "scripts/dev-env.sh: RUSTC_WRAPPER=sccache ($(sccache --version))" >&2
