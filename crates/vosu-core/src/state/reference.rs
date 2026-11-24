//! Lock order: session commit -> reference registry. No plugin calls or filesystem
//! operations occur here; membership and the state swap share the registry lock.
use super::{CommitError, Pointer, PublisherGrant, StateObject, StateValue};
use crate::resources::{ReferenceClass, ReferenceIdentity, ResourceRegistry, ResourceSecret};
use std::sync::{Arc, Mutex};
use vosu_plugin_api::ResourceRef;

pub struct ReferenceStore {
    pub secret: ResourceSecret,
    pub registry: Mutex<ResourceRegistry>,
}
impl ReferenceStore {
    /// Production composition supplies the persisted F4 secret.
    pub fn new(secret: ResourceSecret, registry: ResourceRegistry) -> Self {
        Self {
            secret,
            registry: Mutex::new(registry),
        }
    }
    pub fn is_persistent(&self) -> bool {
        self.secret.is_persistent()
    }
    pub(super) fn ephemeral() -> Self {
        Self {
            secret: ResourceSecret::ephemeral().expect("secure randomness unavailable"),
            registry: Mutex::new(ResourceRegistry::default()),
        }
    }
    pub(super) fn project(
        &self,
        grant: &PublisherGrant,
        ns: &str,
        pointer: &Pointer,
        class: ReferenceClass,
        bytes: &[u8],
    ) -> Result<(StateValue, Arc<ReferenceIdentity>), CommitError> {
        let reference: ResourceRef =
            vosu_plugin_api::decode(bytes).map_err(|_| CommitError::InvalidReference)?;
        let kind = grant
            .resource_kinds
            .iter()
            .find(|k| k.kind == reference.kind)
            .ok_or(CommitError::InvalidReference)?;
        if kind.tree != (class == ReferenceClass::Tree) {
            return Err(CommitError::InvalidReference);
        }
        let tokens = pointer.tokens();
        let valid = if ns == "vosu.chart" {
            tokens.len() == 2
                && tokens[0].as_ref() == "resources"
                && tokens[1].as_ref() == reference.kind
                && reference.kind != "skin"
        } else if ns == "vosu.skin" {
            tokens.len() == 1 && tokens[0].as_ref() == "tree" && reference.kind == "skin"
        } else {
            ns == grant.plugin_id.as_ref()
        };
        if !valid {
            return Err(CommitError::InvalidReference);
        }
        let mime = published_type(&reference.name, &kind.families);
        let mut identity =
            ReferenceIdentity::new(&self.secret, grant.plugin_id(), class, reference)?;
        identity.media_type = mime;
        let identity = Arc::new(identity);
        let (prefix, suffix) = if class == ReferenceClass::Tree {
            ("trees", "/")
        } else {
            ("resources", "")
        };
        let mut fields = vec![(
            Arc::from("url"),
            StateValue::String(format!("/api/v1/{prefix}/{}{suffix}", identity.id.as_str()).into()),
        )];
        if class == ReferenceClass::Resource {
            fields.push((Arc::from("type"), StateValue::String(Arc::from(mime))));
        }
        Ok((
            StateValue::Object(Arc::new(StateObject::new(fields))),
            identity,
        ))
    }
}

fn published_type(name: &str, families: &[String]) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let (family, mime) = match extension.to_ascii_lowercase().as_str() {
        "png" => ("image", "image/png"),
        "jpg" | "jpeg" => ("image", "image/jpeg"),
        "webp" => ("image", "image/webp"),
        "gif" => ("image", "image/gif"),
        "mp3" => ("audio", "audio/mpeg"),
        "ogg" => ("audio", "audio/ogg"),
        "wav" => ("audio", "audio/wav"),
        "flac" => ("audio", "audio/flac"),
        "mp4" => ("video", "video/mp4"),
        "webm" => ("video", "video/webm"),
        "osu" | "osb" => ("chart-text", "text/plain; charset=utf-8"),
        _ => return "application/octet-stream",
    };
    if families.iter().any(|f| f == family) {
        mime
    } else {
        "application/octet-stream"
    }
}
