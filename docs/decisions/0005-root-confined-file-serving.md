# ADR 0005: Serve files through root-confined retained handles

## Status

Accepted

## Context

HTTP routes expose files selected beneath configured static or game roots. Request paths are attacker-controlled, may be percent-encoded, and can race with filesystem renames or symlink changes. Lexical `..` checks alone do not prove that the opened object remains beneath its root.

## Decision

Decode a URL path exactly once, reject malformed encoding, NULs, absolute paths, parent components, roots, and platform prefixes, then carry the validated root and relative child as a private `SafeChild`.

Open and validate the object against its root using platform-specific retained-handle logic. On Linux, prefer `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS` and use a retained-descriptor fallback when unavailable. On Windows, compare normalized final paths from handles. Build metadata and stream bytes from the same already-open file; do not validate a pathname and reopen it later.

The boundary is implemented in [`server/files.rs`](../../src/server/files.rs), directory rendering in [`server/files/directory.rs`](../../src/server/files/directory.rs), and route integration in [`server/routes.rs`](../../src/server/routes.rs).

## Consequences

- Traversal, root escape, and pathname replacement are checked at the file-serving boundary.
- Response metadata and content refer to the same opened object.
- Linux and Windows need different secure-open implementations and platform-specific tests.
- Symlink handling is deliberately constrained by the native resolution and final-path checks rather than ordinary path joining.
- Directory indexes must escape display text and encode link segments independently of path validation.

## Alternatives considered

- **Reject `..` and then use `root.join(request)`:** rejected because symlinks and concurrent filesystem changes can still escape the root.
- **Canonicalize, validate, and reopen by pathname:** rejected because validation and use would target different filesystem lookups.
- **Read an entire file after validation:** rejected because retained handles support bounded streaming and byte ranges without a second lookup or whole-file allocation.

## Related documentation

- [Architecture](../architecture.md)
- [Benchmarks](../benchmarks.md) — file-serving measurements do not weaken the confinement contract.
