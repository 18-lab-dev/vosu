//! `manifest.toml` runtime fields for `kind = "plugin"`
//! (`docs/v2/plugin-abi.md` §8). Shared identity/display metadata, capability
//! declarations, and `covers`'s common-namespace coverage rule are owned by
//! `docs/v2/catalog-v2.md`; this module validates only the ABI-level fields
//! §8 defines, and tolerates unknown top-level fields so a full manifest
//! (which also carries those catalog-owned fields) still parses here.

use std::fmt;

use serde::Deserialize;

use crate::limits::VOSU_PLUGIN_ABI_MAJOR;

/// A validated `manifest.toml` for `kind = "plugin"` (`docs/v2/plugin-abi.md`
/// §8's runtime fields only).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PluginManifest {
    pub kind: String,
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub host: String,
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
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(e) => write!(f, "manifest.toml: {e}"),
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
        }
    }
}

impl std::error::Error for ManifestError {}

/// A relative, confined path: non-empty, no NUL/backslash/colon, and every
/// component is a plain name (mirrors `src/tools/manifest.rs::relative`,
/// this ABI's manifest having the same confinement need).
fn relative(path: &str) -> bool {
    use std::path::{Component, Path};
    !path.is_empty()
        && !path.contains(['\\', '\0', ':'])
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// `[a-z0-9][a-z0-9-]{0,31}` segment grammar shared by resource kinds and
/// plugin id segments (`docs/v2/naming.md`).
fn valid_kind_segment(s: &str) -> bool {
    valid_segment(s, 32, false)
}

/// `[a-z0-9][a-z0-9_-]{0,63}` grammar (`root_slots`, `docs/v2/naming.md`
/// is silent on slot names specifically; plugin-abi.md §8 states this
/// grammar directly).
fn valid_root_slot(s: &str) -> bool {
    valid_segment(s, 64, true)
}

fn valid_segment(s: &str, max_len: usize, allow_underscore: bool) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    s.len() <= max_len
        && chars.all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || c == '-'
                || (allow_underscore && c == '_')
        })
}

/// `^[a-z0-9][a-z0-9-]{0,31}(\.[a-z0-9][a-z0-9-]{0,31}){1,3}$`
/// (`docs/v2/naming.md` "Plugin ids"): 2-4 dot-separated segments.
fn valid_plugin_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('.').collect();
    (2..=4).contains(&segments.len()) && segments.iter().all(|s| valid_kind_segment(s))
}

impl PluginManifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self =
            toml::from_str(text).map_err(|e| ManifestError::Toml(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

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
            let Some(rest) = cover.strip_prefix("field:") else {
                return Err(ManifestError::Covers(cover.clone()));
            };
            if rest.is_empty() || rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> String {
        "kind='plugin'\nmanifest_version=1\nid='vosu.osu'\nname='osu!'\nversion='2.0.0'\nhost='>=2.0.0, <3.0.0'\nabi_version=1\nruntime='in-process'\n".into()
    }

    #[test]
    fn valid_manifest_parses() {
        let manifest = PluginManifest::parse(&base()).unwrap();
        assert_eq!(manifest.id, "vosu.osu");
        assert_eq!(manifest.runtime, Runtime::InProcess);
        assert!(!manifest.concurrent_dispatch);
    }

    #[test]
    fn accepts_full_targets_match_and_resource_kinds() {
        let text = format!(
            "{}concurrent_dispatch=true\nsettings_schema='settings.schema.json'\nroot_slots=['songs']\ncovers=['field:vosu.osu', 'field:vosu.chart.title']\n\n[targets.x86_64-unknown-linux-gnu]\nlibrary='lib/libvosu_osu.so'\n\n[targets.x86_64-pc-windows-msvc]\nlibrary='lib/vosu_osu.dll'\n\n[match]\nprocess_names=['osu!.exe']\n\n[[resource_kinds]]\nkind='file'\ntree=false\nfamilies=['chart-text']\n",
            base()
        );
        let manifest = PluginManifest::parse(&text).unwrap();
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
            PluginManifest::parse(&base().replace("kind='plugin'", "kind='tool'")).unwrap_err(),
            ManifestError::Kind
        );
    }

    #[test]
    fn rejects_wrong_manifest_version() {
        assert_eq!(
            PluginManifest::parse(&base().replace("manifest_version=1", "manifest_version=2"))
                .unwrap_err(),
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
                PluginManifest::parse(&base().replace("id='vosu.osu'", &format!("id='{bad}'")))
                    .unwrap_err(),
                ManifestError::Id,
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_empty_name() {
        assert_eq!(
            PluginManifest::parse(&base().replace("name='osu!'", "name=''")).unwrap_err(),
            ManifestError::Name
        );
    }

    #[test]
    fn rejects_bad_version_and_host() {
        assert!(matches!(
            PluginManifest::parse(&base().replace("version='2.0.0'", "version='latest'"))
                .unwrap_err(),
            ManifestError::Version(_)
        ));
        assert!(matches!(
            PluginManifest::parse(&base().replace("host='>=2.0.0, <3.0.0'", "host='nonsense'"))
                .unwrap_err(),
            ManifestError::Host(_)
        ));
    }

    #[test]
    fn rejects_mismatched_abi_version() {
        assert_eq!(
            PluginManifest::parse(&base().replace("abi_version=1", "abi_version=2")).unwrap_err(),
            ManifestError::AbiVersion
        );
    }

    #[test]
    fn rejects_bad_runtime() {
        assert!(
            PluginManifest::parse(&base().replace("runtime='in-process'", "runtime='native'"))
                .is_err()
        );
    }

    #[test]
    fn rejects_unconfined_settings_schema() {
        let text = format!("{}settings_schema='../settings.schema.json'\n", base());
        assert_eq!(
            PluginManifest::parse(&text).unwrap_err(),
            ManifestError::SettingsSchema
        );
    }

    #[test]
    fn rejects_bad_root_slot() {
        let text = format!("{}root_slots=['Songs']\n", base());
        assert_eq!(
            PluginManifest::parse(&text).unwrap_err(),
            ManifestError::RootSlot("Songs".into())
        );
    }

    #[test]
    fn rejects_bad_covers_entry() {
        let text = format!("{}covers=['vosu.play.combo']\n", base());
        assert_eq!(
            PluginManifest::parse(&text).unwrap_err(),
            ManifestError::Covers("vosu.play.combo".into())
        );
    }

    #[test]
    fn rejects_unconfined_target_library() {
        let text = format!(
            "{}\n[targets.x86_64-unknown-linux-gnu]\nlibrary='/etc/passwd'\n",
            base()
        );
        assert_eq!(
            PluginManifest::parse(&text).unwrap_err(),
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
            PluginManifest::parse(&text).unwrap_err(),
            ManifestError::ResourceKind("Beatmap_Background".into())
        );
    }

    #[test]
    fn rejects_unknown_target_or_match_field() {
        let text = format!(
            "{}\n[targets.x86_64-unknown-linux-gnu]\nlibrary='lib/x.so'\nextra=1\n",
            base()
        );
        assert!(PluginManifest::parse(&text).is_err());
        let text = format!("{}\n[match]\nbogus=1\n", base());
        assert!(PluginManifest::parse(&text).is_err());
    }
}
