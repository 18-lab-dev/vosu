# Tool catalog and identity updates

See the [tool protocol](protocol.md#catalog-sources-and-installation) for sources, install plans and the package format, and [Create a tool](create-a-tool.md) for packaging a tool.

## `replaces`

A tool's `manifest.toml` and its `catalog-v2.json` entry may both declare an optional array of former tool ids, and the two must agree:

```toml
replaces = ["mania-ratings"]
```

`replaces` defaults to `[]`. Ids follow the tool id rule (1–32 lowercase ASCII letters, digits or hyphens, beginning with a letter or digit). Within one source and kind, an entry cannot replace itself, claim an id twice, share a claim with another entry, or name another current id; a tool can never replace a plugin or an overlay. The full rule is [catalog v2](../v2/catalog-v2.md#catalog-v2json).

In 2.0 an install plan never migrates data: adding a package whose `replaces` names an installed package fails with no change. To move to the new id, uninstall the former tool first; the new package starts with its own settings, trust and enabled state. The one-time move of official 1.x tools to their 2.0 successors, including their presentation settings and trust, is the first-start migration in [catalog v2](../v2/catalog-v2.md#first-start-migration-from-1x), not an ordinary update.

## 1.x migrations in progress

A 1.x `replaces` migration that a crash interrupted is still completed at startup, before discovery, from its durable journal: the package, `tool-data/<id>/`, settings file and cache move to the new id by no-overwrite renames, and trust and enabled state move with them. A conflicting target fails recovery rather than discarding data. No new migration of this kind starts in 2.0.

Use only current ids in links and documentation: no route or alias is created for a former id.
