---
type: Documentation Index
title: "vosu maintainer documentation"
description: "Reviewed Markdown documentation, architecture, operations, and performance evidence."
---

# Maintainer documentation

Set up a checkout with [CONTRIBUTING.md](../CONTRIBUTING.md), then start with the [quickstart](quickstart.md); the [README](../README.md) is the user-facing contract. Update affected documentation in the implementation PR and retain dated evidence rather than rewriting it as current verification.

## Current contracts

- [Architecture and source ownership](architecture/index.md)
- [Decision records](decisions/index.md)
- [Runtime, configuration, and compatibility workflows](workflows/index.md)
- [Operations and release runbook](operations/runbook.md)
- [Testing and verification](testing/verification.md)
- [Benchmark methods, measurements, and limitations](testing/benchmarks.md)
- [Precise input validation](testing/input-validation.md)
- [Source-provenance audit](provenance-audit.md) and [locked dependency-license inventory](dependency-license-inventory.json)

## Historical evidence

- [Linux process-event experiment](linux-process-events-experiment.md): dated polling, CN_PROC, and eBPF measurements and lifecycle checks.
