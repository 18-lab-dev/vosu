# Maintainer documentation

This directory explains how **vosu** is built and why its load-bearing boundaries exist. The root [README](../README.md) remains the user-facing contract for installation, configuration, routes, migration, and runtime prerequisites.

## Documentation map

| Document | Use it for | Update it when |
| --- | --- | --- |
| [Architecture](architecture.md) | System context, component ownership, runtime flows, deployment, concurrency, and risks | A module boundary, lifecycle, data flow, or deployment model changes |
| [Architecture decisions](decisions/README.md) | The rationale and consequences of durable technical choices | A load-bearing choice is introduced, replaced, or deliberately reversed |
| [Benchmarks](benchmarks.md) | Criterion methodology, benchmark coverage, local observations, and regression workflow | A hot path, fixture, benchmark method, or measured baseline changes |
| [Performance and memory audit](performance-audit.md) | Prioritized CPU, allocation, and retained-memory opportunities with validation plans | Performance investigations change priorities, evidence, or measurement coverage |
| [Agent instructions](../AGENTS.md) | Exact commands, repository hazards, invariants, and verification expectations for coding agents | A command, ownership boundary, recurring hazard, or required check changes |
| [Claude Code entry point](../CLAUDE.md) | Claude-specific loading of the shared agent instructions | Claude requires guidance that does not apply to other agents |

## Information boundaries

Keep each fact in its narrowest authoritative document:

- Product behavior and operator instructions belong in the root README.
- Current structural facts belong in the architecture guide.
- The reason for a durable choice belongs in one ADR; supersede records rather than rewriting history.
- Performance values belong in the benchmark guide and must include the environment and method that produced them.
- Agent files contain concise, executable repository instructions—not duplicated architecture prose.
- Source comments explain local invariants that are unsafe to separate from the implementation.

Use relative links and exact source paths. Prefer diagrams for boundaries and interactions, not code-level detail. Do not copy API schemas into maintainer documentation; the implementation and compatibility tests remain authoritative.

## Documentation approach

The structure is intentionally lightweight:

- [C4](https://c4model.com/diagrams) system, container, and dynamic views provide only the zoom levels useful for this single-process daemon.
- [arc42](https://docs.arc42.org/) topics inform the architecture guide, tailored to this repository rather than reproduced as an empty template.
- Decision records use one accepted choice per record, with context, consequences, and alternatives.
- [Criterion.rs](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_options.html) provides the benchmark and baseline workflow.
- [AGENTS.md](https://agents.md/) carries vendor-neutral operational instructions; [Claude Code](https://code.claude.com/docs/en/memory) imports that file instead of maintaining a divergent copy.

## Review checklist

When changing documentation:

1. Verify every command against the current CLI or workflow.
2. Verify source links and symbol names against the current tree.
3. Label machine-specific measurements; never present them as service-level guarantees.
4. Keep current product-facing names `vosu`; use `tosu` only for an explicitly labeled upstream reference, one-time migration input, parity fixture, or historical record.
5. Update navigation only when a document has a distinct owner and maintenance trigger.
