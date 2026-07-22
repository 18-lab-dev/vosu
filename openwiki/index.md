---
type: Documentation Index
title: "vosu maintainer documentation"
description: "Navigation, information boundaries, and review conventions for vosu maintainers."
---

# Start here

- [Quickstart](quickstart.md) — repository orientation and the shortest path to the right maintainer guide.
- [Architecture](architecture/index.md) — current structure, source ownership, runtime boundaries, and performance investigations.
- [Decisions](decisions/index.md) — durable technical choices, their context, alternatives, and consequences.
- [Workflows](workflows/index.md) — runtime/configuration and compatibility/calculation change guides.
- [Operations](operations/index.md) — build, launch, security, update, release, and troubleshooting procedures.
- [Testing](testing/index.md) — verification selection, precise-input evidence, and reproducible benchmarks.

## Documentation map

| Document | Use it for | Update it when |
| --- | --- | --- |
| [Architecture overview](architecture/overview.md) | System context, component ownership, runtime flows, deployment, concurrency, and risks | A module boundary, lifecycle, data flow, or deployment model changes |
| [Architecture decisions](decisions/index.md) | Rationale and consequences of durable technical choices | A load-bearing choice is introduced, replaced, or deliberately reversed |
| [Benchmarks](testing/benchmarks.md) | Criterion methodology, coverage, local observations, and regression workflow | A hot path, fixture, method, or measured baseline changes |
| [Precise input validation](testing/input-validation.md) | Harness workflow, evidence interpretation, troubleshooting, and maintenance triggers | Commands, evidence requirements, troubleshooting, or platform coverage changes |
| [Performance audit](architecture/performance-audit.md) | Prioritized CPU, allocation, and retained-memory opportunities with validation plans | Investigation evidence, priorities, or measurement coverage changes |
| [Source map](architecture/source-map.md) | Exact source ownership, hazards, and evidence hierarchy | An ownership boundary or recurring repository hazard changes |

## Information boundaries

Keep each fact in its narrowest authoritative home:

- Root `README.md` is the user and operator contract for installation, configuration, routes, migration, and runtime prerequisites.
- Architecture pages describe current structural facts; decision records preserve the rationale for durable choices. Supersede a decision record rather than rewriting its history.
- Tests and implementation remain authoritative for exact compatibility behavior.
- Benchmark pages contain reproducible methods and explicitly machine-specific observations, never service-level guarantees.
- `AGENTS.md` remains the concise operational contract for coding agents; OpenWiki provides the explanatory maintainer map.

The structure stays intentionally lightweight: use only useful [C4](https://c4model.com/diagrams) system, container, and dynamic views; tailor relevant [arc42](https://docs.arc42.org/) topics rather than reproducing an empty template; and keep one accepted choice, context, consequences, and alternatives in each decision record. [Criterion.rs](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_options.html) provides the benchmark baseline workflow. [`AGENTS.md`](../AGENTS.md) carries vendor-neutral operational instructions, while [`CLAUDE.md`](../CLAUDE.md) imports them rather than maintaining a divergent copy.

## Documentation review

1. Verify every command against the current CLI or workflow.
2. Verify source links and symbol names against the current tree.
3. Label machine-specific measurements and avoid presenting them as guarantees.
4. Use `vosu` for the current product; use `tosu` only for an explicitly labeled upstream reference, migration input, parity fixture, or historical record.
5. Add navigation only when a page has a distinct owner and maintenance trigger.
