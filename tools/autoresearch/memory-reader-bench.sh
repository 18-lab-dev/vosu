#!/usr/bin/env bash
set -euo pipefail

# Deterministic stable/lazer decoder benchmark. The repository-root
# autoresearch.sh remains the canonical Linux process-event experiment.
export CARGO_TERM_COLOR=never
exec cargo bench --locked --no-default-features --bench memory_reader_bench
