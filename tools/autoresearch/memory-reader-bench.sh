#!/usr/bin/env bash
set -euo pipefail

# Deterministic stable/lazer decoder benchmark. The Linux process-event
# experiment is tools/autoresearch/process-events-bench.sh.
#
# Default features match the Linux release's linux-process-events build. Not
# --no-default-features: without the retained pidfd, every native read checks
# the process generation by reading /proc/<pid>/stat twice, which measures that
# fallback (about ten times slower) instead of the shipped reader.
export CARGO_TERM_COLOR=never
exec cargo bench --locked --bench memory_reader_bench
