#!/usr/bin/env bash
set -euo pipefail

export CARGO_TERM_COLOR=never
exec cargo bench --locked --no-default-features --bench memory_reader_bench
