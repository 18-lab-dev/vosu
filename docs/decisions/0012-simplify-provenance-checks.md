# Simplify provenance checks to asset digests and the release LICENSE

- Status: Accepted
- Date: 2026-10-10

## Context

`scripts/check-provenance.py` ran on every CI run and before each release. Besides
verifying `assets/SHA256SUMS`, it duplicated asset hashes in `Cargo.toml`
metadata, required the `LICENSE` digest to appear once in
`THIRD_PARTY_NOTICES.md`, checked the copyright string across several files,
and required `docs/dependency-license-inventory.json` to match `Cargo.lock`
exactly. Those extra checks detected bookkeeping drift rather than changed
bytes, and every dependency update had to regenerate the inventory. The
attribution remediation they were added for (issue #6) is complete and recorded
in the [provenance audit](../provenance-audit.md).

## Decision

1. Keep `assets/SHA256SUMS` as the integrity manifest for embedded assets and
   the release `LICENSE`. `scripts/check-asset-hashes.py` verifies it in CI and
   in both release jobs; `--write` refreshes it after a reviewed change.
2. Keep `scripts/assemble-release-license.py` as a CI and release check, so the
   shipped `LICENSE` still carries the retained calculator MIT notices.
3. Keep `scripts/test_package_release.py` as tests for the release scripts,
   including the unsigned development archive's attribution members and
   `BUILD-INFO.json`, previously covered by `check-provenance.py
   --package-fixtures`.
4. Delete `scripts/check-provenance.py`, the duplicate asset hashes in
   `package.metadata.vosu`, the notices digest cross-reference, the copyright
   string cross-checks, and the dependency-license inventory. Keep the
   `package.metadata.vosu` revision and source records for upstream inputs and
   the offsets asset.

## Consequences

An asset or `LICENSE` byte change still fails CI until its digest is refreshed,
so changes stay deliberate. Dependency updates no longer touch committed
license files; a complete transitive license listing comes from
`cargo metadata` on demand.

## References

- [Provenance audit](../provenance-audit.md)
- [Third-party notices](../../THIRD_PARTY_NOTICES.md)
