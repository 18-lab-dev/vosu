# Tool catalog and identity updates

See the [tool protocol](protocol.md#catalog-sources-and-installation) for sources, install plans and the package format, and [Create a tool](create-a-tool.md) for packaging a tool.

**Match the format to the daemon.** A 2.0 release lists its web tools and
native plugin dependencies in `catalog-v2.json`. During official 1.x
coexistence, the same release also carries an unchanged `catalog.json`
pointing to the independently built, immutable 1.x release artifacts.
Never list a 2.0 ZIP in that frozen catalog. A direct source URL must name
the matching catalog file; each daemon reads only its own format.

## `replaces`

A tool's `manifest.toml` and its `catalog-v2.json` entry may both declare an optional array of former tool ids, and the two must agree:

```toml
replaces = ["mania-ratings"]
```

`replaces` defaults to `[]`. Ids follow the tool id rule (1–32 lowercase ASCII letters, digits or hyphens, beginning with a letter or digit). Within one source and kind, an entry cannot replace itself, claim an id twice, share a claim with another entry, or name another current id; a tool can never replace a plugin or an overlay. The full rule is [catalog v2](../v2/catalog-v2.md#catalog-v2json).

An ordinary 2.0 install plan never migrates data: adding a package whose
`replaces` names an installed package fails without changing it. Uninstall
the former tool before installing an unrelated third-party replacement; the
new package has its own settings, trust and enabled state. The approved
[first-start migration](../migration/2.0.md#what-the-stable-first-start-migration-transfers)
for official 1.x tools is separate. `replaces` can name a former web tool,
not move native data or secrets to a plugin; the closed AF.2 map owns that.

## 1.x migrations in progress

A 1.x `replaces` migration that a crash interrupted is still completed at startup, before discovery, from its durable journal: the package, `tool-data/<id>/`, settings file and cache move to the new id by no-overwrite renames, and trust and enabled state move with them. A conflicting target fails recovery rather than discarding data. No new migration of this kind starts in 2.0.

Use only current ids in links and documentation: no route or alias is created for a former id.
