# ADR 0005: Serve files through root-confined retained handles

## Status

Accepted

## Context

HTTP routes expose files selected beneath configured static or game roots. Request paths are attacker-controlled, may be percent-encoded, and can race with filesystem renames or symlink changes. Lexical `..` checks alone do not prove that the opened object remains beneath its root.

## Decision

Decode a URL path exactly once, reject malformed encoding, NULs, absolute paths, parent components, roots, and platform prefixes, then carry the validated root and relative child as a private `SafeChild`.

Open and validate the object against its root using platform-specific retained-handle logic. On Linux, prefer `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS` and use a retained-descriptor fallback when unavailable. On Windows, compare normalized final paths from handles. Build metadata and stream bytes from the same already-open file; do not validate a pathname and reopen it later.
A top-level counter-directory symlink directly beneath the configured static root is an explicit alias mount declaration and may point to a directory outside the static root; no index marker is required to authorize it. Resolve the requested suffix from the logical alias's current directory target through the same retained-handle confinement used for ordinary roots, so authorization and use do not depend on a stale canonical target pathname. Discovery follows only the top-level alias and skips every nested directory symlink: nested symlinks neither create nor traverse additional mount roots. Generic resolution remains confined to the ordinary configured root, and symlinked regular files continue through that generic retained-handle path.

The boundary is implemented in [`server/files.rs`](../../src/server/files.rs), directory rendering in [`server/files/directory.rs`](../../src/server/files/directory.rs), and route integration in [`server/service/responses.rs`](../../src/server/service/responses.rs) and [`server/service/routing.rs`](../../src/server/service/routing.rs).

## Consequences

- Traversal, root escape, and pathname replacement are checked at the file-serving boundary.
- Response metadata and content refer to the same opened object.
- Linux and Windows need different secure-open implementations and platform-specific tests.
- Symlink handling is deliberately constrained by the native resolution and final-path checks rather than ordinary path joining.
- A top-level symlinked counter directory may expose a directory outside the configured static root because the symlink itself is the explicit local mount declaration. Only its logical subtree is authorized; discovery skips nested directory symlinks, and each requested file is confined and streamed through retained handles rooted at the alias's current target.
- Directory indexes must escape display text and encode link segments independently of path validation.

## Alternatives considered

- **Reject `..` and then use `root.join(request)`:** rejected because symlinks and concurrent filesystem changes can still escape the root.
- **Canonicalize, validate, and reopen by pathname:** rejected because validation and use would target different filesystem lookups.
- **Read an entire file after validation:** rejected because retained handles support bounded streaming and byte ranges without a second lookup or whole-file allocation.

## Related documentation

- [Architecture](../architecture.md)
- [Benchmarks](../benchmarks.md) — file-serving measurements do not weaken the confinement contract.
