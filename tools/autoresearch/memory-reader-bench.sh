#!/usr/bin/env bash
set -euo pipefail

# Deterministic stable/lazer decoder benchmark. The Linux process-event
# experiment is tools/autoresearch/process-events-bench.sh.
export CARGO_TERM_COLOR=never
exec cargo bench --locked --no-default-features --bench memory_reader_bench
