//! Unified package parser and typed views; per-kind unknown fields are rejected.
use super::{Capability, PackageKind, PluginManifest, SharedManifest, ValidationError};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolUi {
    pub root: String,
    #[serde(default = "default_entry")]
    pub entry: String,
}
fn default_entry() -> String {
    "index.html".to_owned()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolManifest {
    #[serde(flatten)]
    pub shared: SharedManifest,
    pub ui: ToolUi,
}
impl ToolManifest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.shared.validate("tool")?;
        for (name, path) in [("root", &self.ui.root), ("entry", &self.ui.entry)] {
            if !super::relative(path) {
                return Err(ValidationError::new("manifest.path", format!("/ui/{name}")));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayManifest {
    #[serde(flatten)]
    pub shared: SharedManifest,
    #[serde(default, deserialize_with = "resolution")]
    pub resolution: Option<[u32; 2]>,
}
impl OverlayManifest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.shared.validate("overlay")?;
        if let Some(size) = self.resolution {
            for (i, dimension) in size.into_iter().enumerate() {
                if !(1..=16384).contains(&dimension) {
                    return Err(ValidationError::new(
                        "manifest.resolution",
                        format!("/resolution/{i}"),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn resolution<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<[u32; 2]>, D::Error> {
    use serde::de::Error;
    let values = Option::<Vec<toml::Value>>::deserialize(deserializer)?;
    values
        .map(|values| {
            let [width, height]: [toml::Value; 2] = values
                .try_into()
                .map_err(|_| D::Error::custom("resolution requires exactly two integers"))?;
            let dimension = |value: toml::Value| {
                value
                    .as_integer()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| D::Error::custom("resolution requires unsigned integers"))
            };
            Ok([dimension(width)?, dimension(height)?])
        })
        .transpose()
}

#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "package documents are cold-path owned values; boxing the plugin view solely to reduce enum size adds an avoidable allocation"
)]
pub enum PackageManifest {
    Plugin(PluginManifest),
    Tool(ToolManifest),
    Overlay(OverlayManifest),
}

/// Borrowed shared view avoids copying metadata or declaration vectors.
#[derive(Debug, Clone, Copy)]
pub struct PackageFields<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub host: &'a str,
    pub description: Option<&'a str>,
    pub author: Option<&'a str>,
    pub homepage: Option<&'a str>,
    pub icon: Option<&'a str>,
    pub banner: Option<&'a str>,
    pub replaces: &'a [String],
    pub provides: &'a [Capability],
    pub requires: &'a [Capability],
    pub optional: &'a [Capability],
}
impl<'a> From<&'a SharedManifest> for PackageFields<'a> {
    fn from(s: &'a SharedManifest) -> Self {
        Self {
            id: &s.id,
            name: &s.name,
            version: &s.version,
            host: &s.host,
            description: s.description.as_deref(),
            author: s.author.as_deref(),
            homepage: s.homepage.as_deref(),
            icon: s.icon.as_deref(),
            banner: s.banner.as_deref(),
            replaces: &s.replaces,
            provides: &s.provides,
            requires: &s.requires,
            optional: &s.optional,
        }
    }
}
fn shape(error: toml::de::Error, pointer: &str) -> ValidationError {
    let mut diagnostic = ValidationError::new("manifest.shape", pointer);
    diagnostic.source_span = error.span();
    diagnostic
}
impl PackageManifest {
    pub fn parse(text: &str) -> Result<Self, ValidationError> {
        #[derive(Deserialize)]
        struct Discriminant {
            kind: String,
        }
        let discriminant: Discriminant = toml::from_str(text).map_err(|e| shape(e, "/kind"))?;
        // Each kind uses its strict typed view; no old/new parser fallback.
        let result = match discriminant.kind.as_str() {
            "plugin" => Self::Plugin(toml::from_str(text).map_err(|e| shape(e, ""))?),
            "tool" => Self::Tool(toml::from_str(text).map_err(|e| shape(e, ""))?),
            "overlay" => Self::Overlay(toml::from_str(text).map_err(|e| shape(e, ""))?),
            _ => return Err(ValidationError::new("manifest.kind", "/kind")),
        };
        result.validate()?;
        Ok(result)
    }
    /// Extracts the actual plugin variant without parsing or revalidating.
    pub fn into_plugin(self) -> Result<PluginManifest, ValidationError> {
        match self {
            Self::Plugin(plugin) => Ok(plugin),
            _ => Err(ValidationError::new("manifest.kind", "/kind")),
        }
    }
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Plugin(p) => p.validate().map_err(|e| e.for_manifest(p)),
            Self::Tool(t) => t.validate(),
            Self::Overlay(o) => o.validate(),
        }
    }
    pub fn kind(&self) -> PackageKind {
        match self {
            Self::Plugin(_) => PackageKind::Plugin,
            Self::Tool(_) => PackageKind::Tool,
            Self::Overlay(_) => PackageKind::Overlay,
        }
    }
    pub fn fields(&self) -> PackageFields<'_> {
        match self {
            Self::Plugin(s) => PackageFields {
                id: &s.id,
                name: &s.name,
                version: &s.version,
                host: &s.host,
                description: s.description.as_deref(),
                author: s.author.as_deref(),
                homepage: s.homepage.as_deref(),
                icon: s.icon.as_deref(),
                banner: s.banner.as_deref(),
                replaces: &s.replaces,
                provides: &s.provides,
                requires: &s.requires,
                optional: &s.optional,
            },
            Self::Tool(t) => (&t.shared).into(),
            Self::Overlay(o) => (&o.shared).into(),
        }
    }
}
