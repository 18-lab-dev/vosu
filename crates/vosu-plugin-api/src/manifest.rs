//! Unified package validation and typed plugin runtime view.
//! Intrinsic validation is separate from explicit registration context,
//! application host support, and V2/V3 asset-backed schema verification.
//!
//! Cutover consumers: core `plugin/registry.rs`, `plugin/host/capabilities.rs`,
//! `state/{grant,projector}.rs` and root `tools/{manifest_v2,capabilities}.rs`.
//! Core keeps runtime matchers, schema registry lifetimes and live authorization.
//! `PackageManifest::parse` validates intrinsic rules; `validate_context`
//! requires complete enabled-source registrations and an application version.
//! `Requirement`/`candidate_satisfies` are the M6b resolver's shared predicates,
//! not a source selector, permission grant or installation plan.
//!
//! V2 consumes `Capability` (own whole namespace, schema path, version) and
//! explicit `SchemaProvenance`, returning namespace/version, canonical
//! pointer/lane/activity triples, required-top-level and visibility metadata.
//! V3 reuses these declarations and `validate_actions`, and compares loaded
//! schema metadata plus ordered package fields before attesting equivalence.
//! Neither intrinsic nor context validation attests absent schema bytes.

use std::fmt;

use serde::Deserialize;

use crate::capability::{
    CORE_NAMESPACES, CapabilityId, parse_capability, valid_camel_path_segment, valid_kind_segment,
    valid_segment,
};
use crate::limits::VOSU_PLUGIN_ABI_MAJOR;

pub mod action_schema;
mod actions;
/// Confined, bounded reads of package files from an extracted package tree,
/// and the portable path-component rule every package path follows.
pub mod files;
pub mod legacy_metadata;
pub mod schema;
/// The `settings_schema` document (`docs/v2/plugin-abi.md` §6.9).
pub mod settings;
pub use actions::{ActionDeclaration, ActionSessionMode, validate_actions};
mod context;
mod diagnostics;
mod package;
#[cfg(test)]
mod package_tests;
mod plugin;
mod requirements;
mod shared;
pub use context::{
    HostSupport, NamespaceIndex, NamespaceRevision, PackageKind, Registration, SchemaProvenance,
    ValidationContext,
};
pub use package::{OverlayManifest, PackageFields, PackageManifest, ToolManifest, ToolUi};
pub use plugin::valid_native_target;
pub use requirements::{Requirement, candidate_satisfies};
pub use shared::{SharedManifest, ValidationError};

/// Plugin-specific typed view of the unified package document.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub kind: String,
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub host: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub icon: Option<String>,
    pub banner: Option<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    pub abi_version: u32,
    pub runtime: Runtime,
    #[serde(default)]
    pub concurrent_dispatch: bool,
    pub settings_schema: Option<String>,
    #[serde(default)]
    pub root_slots: Vec<String>,
    #[serde(default)]
    pub covers: Vec<String>,
    #[serde(default)]
    pub targets: std::collections::BTreeMap<String, Target>,
    #[serde(rename = "match")]
    pub matcher: Option<MatchTable>,
    #[serde(default, rename = "resource_kinds")]
    pub resource_kinds: Vec<ResourceKind>,
    #[serde(default)]
    pub actions: Vec<ActionDeclaration>,
    #[serde(default)]
    pub provides: Vec<Capability>,
    #[serde(default)]
    pub requires: Vec<Capability>,
    #[serde(default)]
    pub optional: Vec<Capability>,
}

/// One `[[provides]]`/`[[requires]]`/`[[optional]]` entry
/// (`docs/v2/catalog-v2.md` "Capability fields"): a capability id, its
/// version or version requirement, and — only for a whole-namespace
/// `field:` entry naming the plugin's own namespace (D7) — the path to
/// that namespace's own schema file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub schema: Option<String>,
}

/// `runtime = "in-process" | "process"` (§7.1's request-only field; the
/// host's trust decision, not validated here, lives in the host's registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    InProcess,
    Process,
}

/// `[targets.<host-triple>]`: omitted entirely for bundled plugins.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub library: String,
}

/// `[match]` (game plugins only; schema owned by §6.1, out of scope for this
/// module beyond shape — matching behavior is a later milestone).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchTable {
    #[serde(default)]
    pub process_names: Vec<String>,
    #[serde(default)]
    pub process_path_patterns: Vec<String>,
    #[serde(default)]
    pub command_line_patterns: Vec<String>,
    #[serde(default)]
    pub comm_hints: Vec<String>,
    /// Linux-only userspace substring matcher, additive to `comm_hints`
    /// (A1b/DB revised): a process whose `name` (kernel `comm`) contains
    /// an entry matches. Each entry is 1-15 bytes (the kernel/CN_PROC
    /// `comm` field width) and, like `comm_hints`, also feeds the
    /// kernel/CN_PROC pre-filter. Accepted but unused on Windows.
    #[serde(default)]
    pub linux_name_contains: Vec<String>,
}

/// `[[resource_kinds]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceKind {
    pub kind: String,
    #[serde(default)]
    pub tree: bool,
    #[serde(default)]
    pub families: Vec<String>,
}

/// Every way a `manifest.toml` fails to parse or validate against §8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Toml(String),
    /// `kind` must be `"plugin"` for this module to apply.
    Kind,
    /// `manifest_version` must be `1`.
    ManifestVersion,
    /// `id` must match the naming contract's plugin id grammar.
    Id,
    /// `name` must not be empty.
    Name,
    /// `version` must be valid semver.
    Version(String),
    /// `host` must be a valid semver requirement.
    Host(String),
    /// `abi_version` must equal `VOSU_PLUGIN_ABI_MAJOR`.
    AbiVersion,
    /// `settings_schema` must be a confined relative path.
    SettingsSchema,
    /// A `root_slots` entry failed its grammar.
    RootSlot(String),
    /// A `covers` entry failed its grammar (`field:<namespace>[.<path>]`).
    Covers(String),
    /// A `[targets.<triple>]` entry's `library` is not a confined relative
    /// path.
    Target(String),
    /// A `[[resource_kinds]]` entry's `kind` failed its grammar.
    ResourceKind(String),
    /// A `[[provides]]`/`[[requires]]`/`[[optional]]` id failed the
    /// capability id grammar (`crate::capability::parse_capability`).
    Capability(String),
    /// A `provides` entry's `version` was not valid semver, or a
    /// `requires`/`optional` entry's was not a valid semver requirement.
    CapabilityVersion(String),
    /// A `game:` capability's `version` was not the literal `"*"`
    /// (`docs/v2/naming.md` "Game ids").
    CapabilityGameVersion(String),
    /// A whole-namespace `field:<own id>` `provides` entry had no
    /// `schema` (`docs/v2/catalog-v2.md` "Capability fields", D7).
    CapabilityMissingSchema(String),
    /// A `field:` `provides` entry named a namespace other than this
    /// plugin's own id (core namespaces are always built-in providers;
    /// `docs/v2/catalog-v2.md` "Capability fields").
    CapabilityForeignNamespace(String),
    /// A capability id appeared in both `requires` and `optional`.
    CapabilityDuplicate(String),
    /// A capability `schema` path was not a confined relative path
    /// (`relative`, same confinement as `settings_schema`).
    CapabilitySchemaPath(String),
    /// A capability entry carried `schema` where only a whole-namespace
    /// `field:<own id>` `provides` entry may (`docs/v2/catalog-v2.md`
    /// "Capability fields").
    CapabilityUnexpectedSchema(String),
    /// A `requires`/`optional` entry named a capability under this
    /// plugin's own `provides` namespace (no self-dependency,
    /// `docs/v2/catalog-v2.md` "Capability fields").
    CapabilitySelfDependency(String),
    /// An action declaration or its matching provide is invalid.
    Action(String),
    Rule(ValidationError),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(e) => write!(f, "manifest.toml: {e}"),
            Self::Rule(error) => error.fmt(f),
            Self::Action(reason) => write!(f, "actions: {reason}"),
            Self::Kind => write!(f, "kind must be \"plugin\""),
            Self::ManifestVersion => write!(f, "manifest_version must be 1"),
            Self::Id => write!(f, "id must match the naming contract's plugin id grammar"),
            Self::Name => write!(f, "name must not be empty"),
            Self::Version(e) => write!(f, "version: {e}"),
            Self::Host(e) => write!(f, "host: {e}"),
            Self::AbiVersion => write!(
                f,
                "abi_version must equal VOSU_PLUGIN_ABI_MAJOR ({VOSU_PLUGIN_ABI_MAJOR})"
            ),
            Self::SettingsSchema => write!(f, "settings_schema must be a confined relative path"),
            Self::RootSlot(s) => write!(f, "root_slots: invalid entry {s:?}"),
            Self::Covers(s) => write!(f, "covers: invalid entry {s:?}"),
            Self::Target(s) => write!(f, "targets: invalid library path {s:?}"),
            Self::ResourceKind(s) => write!(f, "resource_kinds: invalid kind {s:?}"),
            Self::Capability(s) => write!(f, "invalid capability id {s:?}"),
            Self::CapabilityVersion(s) => {
                write!(f, "capability {s:?}: invalid version/version requirement")
            }
            Self::CapabilityGameVersion(s) => {
                write!(f, "capability {s:?}: game: version must be \"*\"")
            }
            Self::CapabilityMissingSchema(s) => write!(
                f,
                "capability {s:?}: a whole-namespace provides entry for this plugin's own namespace needs a schema"
            ),
            Self::CapabilityForeignNamespace(s) => write!(
                f,
                "capability {s:?}: a field: provides entry must name this plugin's own namespace"
            ),
            Self::CapabilityDuplicate(s) => {
                write!(f, "capability {s:?}: present in both requires and optional")
            }
            Self::CapabilitySchemaPath(s) => {
                write!(
                    f,
                    "capability schema {s:?} must be a confined relative path"
                )
            }
            Self::CapabilityUnexpectedSchema(s) => write!(
                f,
                "capability {s:?}: schema is only valid on a whole-namespace provides entry for this plugin's own namespace"
            ),
            Self::CapabilitySelfDependency(s) => write!(
                f,
                "capability {s:?}: requires/optional must not name this plugin's own provides namespace"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

/// A relative, confined path: non-empty, no NUL/backslash/colon, and every
/// component is a plain name (mirrors `src/tools/manifest.rs::relative`,
/// this ABI's manifest having the same confinement need).
fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0', ':'])
        && path.split('/').all(|c| !matches!(c, "" | "." | ".."))
}

/// `[a-z0-9][a-z0-9_-]{0,63}` grammar (`root_slots`, `docs/v2/naming.md`
/// is silent on slot names specifically; plugin-abi.md §8 states this
/// grammar directly).
fn valid_root_slot(s: &str) -> bool {
    valid_segment(s, 64, true)
}

/// `^[a-z0-9][a-z0-9-]{0,31}(\.[a-z0-9][a-z0-9-]{0,31}){1,3}$`
/// (`docs/v2/naming.md` "Plugin ids"): 2-4 dot-separated segments, never
/// equal to or extending a core-owned or reserved namespace
/// (`docs/v2/catalog-v2.md` "Namespace and kind parsing": "A plugin id
/// that equals or extends any core-owned namespace ... is always
/// rejected, from any source").
fn valid_plugin_id(id: &str) -> bool {
    (2..=4).contains(&id.split('.').count())
        && id.split('.').all(valid_kind_segment)
        && !matches!(id.split('.').next(), Some("chart" | "skin"))
        && !is_reserved_namespace(id)
}

/// Core-owned namespaces and the reserved `vosu.sessions` pseudo-namespace
/// are never satisfiable as a plugin id, equal or as a dotted prefix
/// (`docs/v2/catalog-v2.md` "Namespace and kind parsing",
/// `docs/v2/naming.md` "Reserved names").
fn is_reserved_namespace(id: &str) -> bool {
    CORE_NAMESPACES
        .iter()
        .chain(["vosu.sessions"].iter())
        .any(|ns| id == *ns || id.starts_with(&format!("{ns}.")))
}

/// `field:<core-namespace>[.<path>]` (`docs/v2/catalog-v2.md`
/// "`kind = \"plugin\"`", `docs/v2/state-schema.md` "Naming" and
/// `docs/v2/naming.md` "Capability id grammar"): a `covers` entry names one
/// of the five core namespaces, bare (the whole namespace) or with a dotted
/// path of camelCase schema keys below it — except
/// `field:vosu.chart.resources`/`field:vosu.skin.tree` and everything below
/// them, which only reserved-name `[[resource_kinds]]` entries cover, never
/// `covers` ("Coverage matching"). A `field:` id outside a core namespace
/// is likewise rejected here: a plugin's own namespace needs no `covers`
/// entry at all, since its `provides` entry for that namespace already
/// covers it (`docs/v2/catalog-v2.md` "`kind = \"plugin\"`").
fn valid_covers_entry(cover: &str) -> bool {
    let Some(rest) = cover.strip_prefix("field:") else {
        return false;
    };
    for namespace in CORE_NAMESPACES {
        if rest == namespace {
            return true;
        }
        let Some(path) = rest
            .strip_prefix(namespace)
            .and_then(|r| r.strip_prefix('.'))
        else {
            continue;
        };
        if path.is_empty() || !path.split('.').all(valid_camel_path_segment) {
            return false;
        }
        return match namespace {
            "vosu.chart" => path != "resources" && !path.starts_with("resources."),
            "vosu.skin" => path != "tree" && !path.starts_with("tree."),
            _ => true,
        };
    }
    false
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.kind != "plugin" {
            return Err(ManifestError::Kind);
        }
        if self.manifest_version != 1 {
            return Err(ManifestError::ManifestVersion);
        }
        if !valid_plugin_id(&self.id) {
            return Err(ManifestError::Id);
        }
        if self.name.trim().is_empty() {
            return Err(ManifestError::Name);
        }
        semver::Version::parse(&self.version).map_err(|e| ManifestError::Version(e.to_string()))?;
        semver::VersionReq::parse(&self.host).map_err(|e| ManifestError::Host(e.to_string()))?;
        if self.abi_version != VOSU_PLUGIN_ABI_MAJOR {
            return Err(ManifestError::AbiVersion);
        }
        if let Some(schema) = &self.settings_schema
            && !relative(schema)
        {
            return Err(ManifestError::SettingsSchema);
        }
        for slot in &self.root_slots {
            if !valid_root_slot(slot) {
                return Err(ManifestError::RootSlot(slot.clone()));
            }
        }
        for cover in &self.covers {
            if !valid_covers_entry(cover) {
                return Err(ManifestError::Covers(cover.clone()));
            }
        }
        for target in self.targets.values() {
            if !relative(&target.library) {
                return Err(ManifestError::Target(target.library.clone()));
            }
        }
        for resource_kind in &self.resource_kinds {
            if !valid_kind_segment(&resource_kind.kind) {
                return Err(ManifestError::ResourceKind(resource_kind.kind.clone()));
            }
        }
        for cap in &self.provides {
            let parsed =
                parse_capability(&cap.id).map_err(|_| ManifestError::Capability(cap.id.clone()))?;
            match parsed {
                CapabilityId::Game(_) => {
                    if cap.version != "*" {
                        return Err(ManifestError::CapabilityGameVersion(cap.id.clone()));
                    }
                    if cap.schema.is_some() {
                        return Err(ManifestError::CapabilityUnexpectedSchema(cap.id.clone()));
                    }
                }
                CapabilityId::Field(suffix) => {
                    semver::Version::parse(&cap.version)
                        .map_err(|_| ManifestError::CapabilityVersion(cap.id.clone()))?;
                    if suffix == self.id.as_str() {
                        match &cap.schema {
                            None => {
                                return Err(ManifestError::CapabilityMissingSchema(cap.id.clone()));
                            }
                            Some(schema) if !relative(schema) => {
                                return Err(ManifestError::CapabilitySchemaPath(schema.clone()));
                            }
                            Some(_) => {}
                        }
                    } else {
                        if cap.schema.is_some() {
                            return Err(ManifestError::CapabilityUnexpectedSchema(cap.id.clone()));
                        }
                        if CORE_NAMESPACES.contains(&suffix) {
                            return Err(ManifestError::CapabilityForeignNamespace(cap.id.clone()));
                        }
                    }
                }
                CapabilityId::Resource(suffix) | CapabilityId::Action(suffix) => {
                    semver::Version::parse(&cap.version)
                        .map_err(|_| ManifestError::CapabilityVersion(cap.id.clone()))?;
                    if cap.schema.is_some() {
                        return Err(ManifestError::CapabilityUnexpectedSchema(cap.id.clone()));
                    }
                    if !suffix.starts_with(&format!("{}.", self.id)) {
                        return Err(ManifestError::CapabilityForeignNamespace(cap.id.clone()));
                    }
                }
            }
        }
        for cap in self.requires.iter().chain(self.optional.iter()) {
            let parsed =
                parse_capability(&cap.id).map_err(|_| ManifestError::Capability(cap.id.clone()))?;
            if cap.schema.is_some() {
                return Err(ManifestError::CapabilityUnexpectedSchema(cap.id.clone()));
            }
            let own_namespace_suffix = match parsed {
                CapabilityId::Game(_) => {
                    if cap.version != "*" {
                        return Err(ManifestError::CapabilityGameVersion(cap.id.clone()));
                    }
                    None
                }
                CapabilityId::Field(suffix)
                | CapabilityId::Resource(suffix)
                | CapabilityId::Action(suffix) => {
                    semver::VersionReq::parse(&cap.version)
                        .map_err(|_| ManifestError::CapabilityVersion(cap.id.clone()))?;
                    Some(suffix)
                }
            };
            if self
                .provides
                .iter()
                .any(|provided| crate::capability::covers(&provided.id, &cap.id))
            {
                return Err(ManifestError::CapabilitySelfDependency(cap.id.clone()));
            }
            if let Some(suffix) = own_namespace_suffix
                && (suffix == self.id.as_str() || suffix.starts_with(&format!("{}.", self.id)))
            {
                return Err(ManifestError::CapabilitySelfDependency(cap.id.clone()));
            }
        }
        let required_ids: std::collections::HashSet<&str> =
            self.requires.iter().map(|cap| cap.id.as_str()).collect();
        for cap in &self.optional {
            if required_ids.contains(cap.id.as_str()) {
                return Err(ManifestError::CapabilityDuplicate(cap.id.clone()));
            }
        }
        actions::validate(self)?;
        plugin::validate(self).map_err(ManifestError::Rule)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Typed validator diagnostics are exercised without exporting a second parser.
    fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
        let manifest: PluginManifest =
            toml::from_str(text).map_err(|error| ManifestError::Toml(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn base() -> String {
        "kind='plugin'\nmanifest_version=1\nid='vosu.osu'\nname='osu!'\nversion='2.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n".into()
    }

    #[test]
    fn valid_manifest_parses() {
        let manifest = parse(&base()).unwrap();
        assert_eq!(manifest.id, "vosu.osu");
        assert_eq!(manifest.runtime, Runtime::InProcess);
        assert!(!manifest.concurrent_dispatch);
    }

    #[test]
    fn accepts_full_targets_match_and_resource_kinds() {
        let text = format!(
            "{}concurrent_dispatch=true\nsettings_schema='settings.schema.json'\nroot_slots=['songs']\ncovers=['field:vosu.play.combo', 'field:vosu.chart.title']\n\n[targets.x86_64-unknown-linux-gnu]\nlibrary='lib/libvosu_osu.so'\n\n[targets.x86_64-pc-windows-msvc]\nlibrary='lib/vosu_osu.dll'\n\n[match]\nprocess_names=['osu!.exe']\n\n[[resource_kinds]]\nkind='file'\ntree=false\nfamilies=['chart-text']\n\n[[provides]]\nid='game:osu'\nversion='*'\n",
            base()
        );
        let manifest = parse(&text).unwrap();
        assert!(manifest.concurrent_dispatch);
        assert_eq!(manifest.root_slots, vec!["songs"]);
        assert_eq!(manifest.targets.len(), 2);
        assert_eq!(
            manifest.matcher.unwrap().process_names,
            vec!["osu!.exe".to_string()]
        );
        assert_eq!(manifest.resource_kinds.len(), 1);
    }

    #[test]
    fn rejects_wrong_kind() {
        assert_eq!(
            parse(&base().replace("kind='plugin'", "kind='tool'")).unwrap_err(),
            ManifestError::Kind
        );
    }

    #[test]
    fn rejects_wrong_manifest_version() {
        assert_eq!(
            parse(&base().replace("manifest_version=1", "manifest_version=2")).unwrap_err(),
            ManifestError::ManifestVersion
        );
    }

    #[test]
    fn rejects_invalid_plugin_ids() {
        for bad in [
            "osu",
            "Vosu.Osu",
            "vosu..osu",
            "vosu.osu.a.b.c",
            "-vosu.osu",
        ] {
            assert_eq!(
                parse(&base().replace("id='vosu.osu'", &format!("id='{bad}'"))).unwrap_err(),
                ManifestError::Id,
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_empty_name() {
        assert_eq!(
            parse(&base().replace("name='osu!'", "name=''")).unwrap_err(),
            ManifestError::Name
        );
    }

    #[test]
    fn rejects_bad_version_and_host() {
        assert!(matches!(
            parse(&base().replace("version='2.0.0'", "version='latest'")).unwrap_err(),
            ManifestError::Version(_)
        ));
        assert!(matches!(
            parse(&base().replace("host='>=2.0.0-alpha.1, <3.0.0'", "host='nonsense'"))
                .unwrap_err(),
            ManifestError::Host(_)
        ));
    }

    #[test]
    fn rejects_mismatched_abi_version() {
        assert_eq!(
            parse(&base().replace("abi_version=1", "abi_version=2")).unwrap_err(),
            ManifestError::AbiVersion
        );
    }

    #[test]
    fn rejects_bad_runtime() {
        assert!(parse(&base().replace("runtime='in-process'", "runtime='native'")).is_err());
    }

    #[test]
    fn rejects_unconfined_settings_schema() {
        let text = format!("{}settings_schema='../settings.schema.json'\n", base());
        assert_eq!(parse(&text).unwrap_err(), ManifestError::SettingsSchema);
    }

    #[test]
    fn rejects_bad_root_slot() {
        let text = format!("{}root_slots=['Songs']\n", base());
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::RootSlot("Songs".into())
        );
    }

    #[test]
    fn rejects_bad_covers_entry() {
        let text = format!("{}covers=['vosu.play.combo']\n", base());
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::Covers("vosu.play.combo".into())
        );
    }

    #[test]
    fn accepts_covers_entries_in_every_core_namespace() {
        for entry in [
            "field:vosu.client",
            "field:vosu.status.online",
            "field:vosu.chart.title",
            "field:vosu.chart.difficultyName",
            "field:vosu.play.combo",
            "field:vosu.skin.name",
        ] {
            let text = format!("{}covers=['{entry}']\n", base());
            parse(&text).unwrap_or_else(|e| panic!("expected {entry:?} to be accepted, got {e}"));
        }
    }

    #[test]
    fn rejects_covers_entry_outside_core_namespace() {
        for entry in [
            "field:vosu.osu",
            "field:vosu.osu.play.pp.current",
            "acme.test",
        ] {
            let text = format!("{}covers=['{entry}']\n", base());
            assert_eq!(
                parse(&text).unwrap_err(),
                ManifestError::Covers(entry.into()),
                "expected {entry:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_covers_entry_with_non_camel_case_path_segment() {
        for entry in [
            "field:vosu.chart.difficulty-name",
            "field:vosu.chart.DifficultyName",
            "field:vosu.chart.",
            "field:vosu.chart..title",
        ] {
            let text = format!("{}covers=['{entry}']\n", base());
            assert_eq!(
                parse(&text).unwrap_err(),
                ManifestError::Covers(entry.into()),
                "expected {entry:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_covers_entry_reserved_for_resource_kinds() {
        for entry in [
            "field:vosu.chart.resources",
            "field:vosu.chart.resources.file",
            "field:vosu.skin.tree",
            "field:vosu.skin.tree.anything",
        ] {
            let text = format!("{}covers=['{entry}']\n", base());
            assert_eq!(
                parse(&text).unwrap_err(),
                ManifestError::Covers(entry.into()),
                "expected {entry:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_unconfined_target_library() {
        let text = format!(
            "{}\n[targets.x86_64-unknown-linux-gnu]\nlibrary='/etc/passwd'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::Target("/etc/passwd".into())
        );
    }

    #[test]
    fn rejects_bad_resource_kind() {
        let text = format!(
            "{}\n[[resource_kinds]]\nkind='Beatmap_Background'\ntree=false\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::ResourceKind("Beatmap_Background".into())
        );
    }

    #[test]
    fn rejects_unknown_target_or_match_field() {
        let text = format!(
            "{}\n[targets.x86_64-unknown-linux-gnu]\nlibrary='lib/x.so'\nextra=1\n",
            base()
        );
        assert!(parse(&text).is_err());
        let text = format!("{}\n[match]\nbogus=1\n", base());
        assert!(parse(&text).is_err());
    }

    #[test]
    fn parses_provides_requires_optional() {
        let text = format!(
            "{}\n[[provides]]\nid = 'field:vosu.osu'\nversion = '1.2.0'\nschema = 'schemas/vosu.osu.schema.json'\n\n[[requires]]\nid = 'field:vosu.play.combo'\nversion = '^1.0.0'\n\n[[optional]]\nid = 'resource:chart.background'\nversion = '^1.0.0'\n",
            base()
        );
        let manifest = parse(&text).unwrap();
        assert_eq!(manifest.provides.len(), 1);
        assert_eq!(manifest.provides[0].id, "field:vosu.osu");
        assert_eq!(
            manifest.provides[0].schema.as_deref(),
            Some("schemas/vosu.osu.schema.json")
        );
        assert_eq!(manifest.requires.len(), 1);
        assert_eq!(manifest.requires[0].id, "field:vosu.play.combo");
        assert_eq!(manifest.optional.len(), 1);
        assert_eq!(manifest.optional[0].id, "resource:chart.background");
    }

    #[test]
    fn own_namespace_provides_requires_schema() {
        let without_schema = format!(
            "{}\n[[provides]]\nid = 'field:vosu.osu'\nversion = '1.2.0'\n",
            base()
        );
        assert_eq!(
            parse(&without_schema).unwrap_err(),
            ManifestError::CapabilityMissingSchema("field:vosu.osu".into())
        );
        let with_schema = format!(
            "{}\n[[provides]]\nid = 'field:vosu.osu'\nversion = '1.2.0'\nschema = 'schemas/vosu.osu.schema.json'\n",
            base()
        );
        parse(&with_schema).unwrap();
    }

    #[test]
    fn id_in_requires_and_optional_refused() {
        let text = format!(
            "{}\n[[requires]]\nid = 'field:vosu.play.combo'\nversion = '^1.0.0'\n\n[[optional]]\nid = 'field:vosu.play.combo'\nversion = '^1.0.0'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityDuplicate("field:vosu.play.combo".into())
        );
    }

    #[test]
    fn rejects_field_provides_outside_own_namespace() {
        let text = format!(
            "{}\n[[provides]]\nid = 'field:vosu.play'\nversion = '1.0.0'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityForeignNamespace("field:vosu.play".into())
        );
    }

    #[test]
    fn rejects_bad_game_capability_version() {
        let text = format!(
            "{}\n[[provides]]\nid = 'field:vosu.osu'\nversion = '1.2.0'\nschema = 'schemas/vosu.osu.schema.json'\n\n[[requires]]\nid = 'game:osu'\nversion = '^1.0.0'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityGameVersion("game:osu".into())
        );
    }

    #[test]
    fn rejects_malformed_capability_id() {
        let text = format!(
            "{}\n[[requires]]\nid = 'vosu.play.combo'\nversion = '^1.0.0'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::Capability("vosu.play.combo".into())
        );
    }

    #[test]
    fn rejects_unconfined_capability_schema_path() {
        for bad in ["/etc/passwd", "../../x", "a/../../x"] {
            let text = format!(
                "{}\n[[provides]]\nid = 'field:vosu.osu'\nversion = '1.2.0'\nschema = '{bad}'\n",
                base()
            );
            assert_eq!(
                parse(&text).unwrap_err(),
                ManifestError::CapabilitySchemaPath(bad.into()),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_plugin_id_equal_to_or_extending_a_core_namespace() {
        for bad in [
            "vosu.play",
            "vosu.play.pp",
            "vosu.sessions",
            "vosu.sessions.focused",
        ] {
            assert_eq!(
                parse(&base().replace("id='vosu.osu'", &format!("id='{bad}'"))).unwrap_err(),
                ManifestError::Id,
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_core_namespace_field_provides_even_with_schema() {
        // With `id = "vosu.osu"` (not a core namespace), a provides entry
        // naming a *different* core namespace is already both a foreign
        // namespace and a leaf entry carrying a schema it must not
        // (schema is checked first); this only passes if `id` itself is
        // also allowed to be a core namespace, which it no longer is
        // (the test above).
        let text = format!(
            "{}\n[[provides]]\nid = 'field:vosu.play'\nversion = '1.0.0'\nschema = 'schemas/vosu.play.schema.json'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityUnexpectedSchema("field:vosu.play".into())
        );
    }

    #[test]
    fn rejects_schema_on_leaf_field_provides_entry() {
        let text = format!(
            "{}\n[[provides]]\nid = 'field:vosu.osu.play'\nversion = '1.0.0'\nschema = 'schemas/x.schema.json'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityUnexpectedSchema("field:vosu.osu.play".into())
        );
    }

    #[test]
    fn rejects_schema_on_requires_or_optional_entry() {
        let text = format!(
            "{}\n[[requires]]\nid = 'field:vosu.play.combo'\nversion = '^1.0.0'\nschema = 'schemas/x.schema.json'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityUnexpectedSchema("field:vosu.play.combo".into())
        );
    }

    #[test]
    fn rejects_core_resource_kind_in_provides() {
        let text = format!(
            "{}\n[[provides]]\nid = 'resource:chart.background'\nversion = '1.0.0'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityForeignNamespace("resource:chart.background".into())
        );
        let text = format!(
            "{}\n[[provides]]\nid = 'resource:skin'\nversion = '1.0.0'\n",
            base()
        );
        assert_eq!(
            parse(&text).unwrap_err(),
            ManifestError::CapabilityForeignNamespace("resource:skin".into())
        );
    }

    #[test]
    fn rejects_self_dependency() {
        for id in ["field:vosu.osu", "field:vosu.osu.play.pp"] {
            let text = format!(
                "{}\n[[requires]]\nid = '{id}'\nversion = '^1.0.0'\n",
                base()
            );
            assert_eq!(
                parse(&text).unwrap_err(),
                ManifestError::CapabilitySelfDependency(id.into()),
                "expected {id:?} to be rejected"
            );
        }
    }
}
