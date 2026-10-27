# Tool catalog and identity updates

See the [tool protocol](protocol.md#catalog-sources-and-installation) for source configuration, download checks and job routes, and [Create a tool](create-a-tool.md) for packaging a tool.

## `replaces`

A catalog's tool entry may declare an optional array of former tool IDs:

```json
{
  "id": "map-analysis",
  "name": "Map analysis",
  "replaces": ["mania-ratings"]
}
```

This is a fragment of a tool entry, not a complete catalog. All normal fields, package integrity checks and manifest validation still apply. `replaces` defaults to `[]`; IDs follow the manifest ID rules (1–32 lowercase ASCII letters, digits or hyphens, beginning with a letter or digit). An entry cannot replace itself. Duplicate IDs, two entries claiming the same former ID, or a former ID also listed as a current tool in the same catalog reject that source. The package's strict `tool.toml` must **not** contain `replaces`: it is catalog metadata, not a manifest extension.

The Tools page offers an installed former ID's replacement as an update, even if the new ID has the same or a lower version number. Its card names the new tool. The catalog response includes `replacement_of` (the installed former ID, otherwise `null`); the installed `ToolView` includes `update_target` (the new ID, otherwise `null`). Only an approved, catalog-installed tool can migrate, and the replacement must come from its **exact saved catalog source identity**. Changing source order, URL spelling, tag, or repository cannot transfer trust across sources. Development overrides and manual installations are never migrated. Multiple installed former IDs cannot be merged into one replacement.

Apply it from the dashboard, or run:

```sh
vosux --update mania-ratings
# Equivalent: vosu x --update mania-ratings
```

The update verifies and stages the new package, stops the former service, and writes a durable migration intent before changing any installed path. Same-filesystem, no-overwrite renames move the package, durable `tool-data/<id>/` directory, settings file, and cache to the new ID. Trust, enabled and autostart choices and installation provenance move together through an atomic state-file replacement. The former package is then removed, and an already-running service restarts under the new ID. The old tool ID and its hosted routes no longer exist; this does not create a route or command alias.

An existing target package directory, data directory, settings file, cache directory, or saved state is a clear error: nothing there is overwritten, including an empty directory. Fix the conflicting installation/storage deliberately before retrying. vosu does not merge settings or directories. A failure before publication leaves the former installation untouched. After the intent is published, startup and subsequent operations finish the recorded migration before discovery; a storage conflict fails recovery rather than discarding data. A stopped/restarted process at any boundary can safely retry. Repeating the former-ID update after completion resolves the recorded replacement and does not migrate data again.

## Dependencies after a rename

For `requires` and overlay `requires_tools`, an installed replacement satisfies its former IDs as well as its current ID. The semver constraint is checked against the **replacement's current manifest version**; enabled, trust and startup checks still apply. The catalog-authorized identities are retained in private install state, so this works while the catalog is offline. Dependency ordering, required-by listings, write authorization for declared dependencies, and uninstall protection use the same identities. Tools should nevertheless update their first-party requirements and requests to the current ID: dependency satisfaction does not redirect old HTTP routes.

vosu 1.2.0 and 1.2.1 ignore unknown catalog fields, including `replaces`. On those versions, Map analysis can be installed manually as a separate new tool; mania-ratings keeps working without further updates, and its state is not migrated. Upgrade vosu for automatic migration.
