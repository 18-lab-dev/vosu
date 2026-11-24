use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ciborium::Value;
use ring::{digest, hmac};
use std::sync::Arc;
use vosu_plugin_api::ResourceRef;

use super::{ReferenceClass, RegistryError, ResourceSecret};

pub const MAX_RESOURCE_ID_BYTES: usize = 512;
const MAX_REFERENCE_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 65_536;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResourceId(Arc<str>);

impl ResourceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Original opaque values are retained unchanged for later plugin dispatch.
#[derive(Clone, Debug)]
pub struct ReferenceIdentity {
    pub id: ResourceId,
    pub plugin: Arc<str>,
    pub class: ReferenceClass,
    pub reference: Arc<ResourceRef>,
    metadata_bytes: usize,
}

impl ReferenceIdentity {
    pub fn new(
        secret: &ResourceSecret,
        plugin: &str,
        class: ReferenceClass,
        reference: ResourceRef,
    ) -> Result<Self, RegistryError> {
        if plugin.is_empty() || plugin.len() > 192 {
            return Err(RegistryError::InvalidReference);
        }
        let mut remaining = MAX_NODES;
        let mut retained =
            reference.kind.capacity() + reference.name.capacity() + size_of::<ResourceRef>();
        measure(&reference.key, 0, &mut remaining, &mut retained)?;
        measure(&reference.content_id, 0, &mut remaining, &mut retained)?;
        let mut bytes = vec![0x84];
        ciborium::into_writer(&reference.kind, &mut bytes)
            .map_err(|_| RegistryError::InvalidReference)?;
        encode(&reference.key, &mut bytes)?;
        ciborium::into_writer(&reference.name, &mut bytes)
            .map_err(|_| RegistryError::InvalidReference)?;
        encode(&reference.content_id, &mut bytes)?;
        let hash = digest::digest(&digest::SHA256, &bytes);
        let body = format!(
            "1.{}.{}.{}",
            class.tag(),
            URL_SAFE_NO_PAD.encode(plugin),
            URL_SAFE_NO_PAD.encode(hash.as_ref())
        );
        let tag = hmac::sign(secret.key(), body.as_bytes());
        let text = format!("{body}.{}", URL_SAFE_NO_PAD.encode(tag.as_ref()));
        if text.len() > MAX_RESOURCE_ID_BYTES {
            return Err(RegistryError::InvalidReference);
        }
        let metadata_bytes = retained + text.len() + plugin.len() + size_of::<Self>();
        Ok(Self {
            id: ResourceId(text.into()),
            plugin: plugin.into(),
            class,
            reference: Arc::new(reference),
            metadata_bytes,
        })
    }

    pub(super) fn bytes(&self) -> usize {
        self.metadata_bytes
    }
}

impl ResourceSecret {
    /// Authenticate before any membership lookup. Strict decoding rejects aliases.
    pub fn verify(
        &self,
        token: &str,
    ) -> Result<(ResourceId, String, ReferenceClass), RegistryError> {
        let invalid = || RegistryError::NotFound;
        if token.len() > MAX_RESOURCE_ID_BYTES {
            return Err(invalid());
        }
        let mut parts = token.split('.');
        let fields = [
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ];
        let [
            Some(version),
            Some(class_tag),
            Some(plugin_text),
            Some(digest_text),
            Some(tag_text),
        ] = fields
        else {
            return Err(invalid());
        };
        if parts.next().is_some() || version != "1" {
            return Err(invalid());
        }
        let class = match class_tag {
            "r" => ReferenceClass::Resource,
            "t" => ReferenceClass::Tree,
            _ => return Err(invalid()),
        };
        let decode = |text: &str| {
            let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid())?;
            if URL_SAFE_NO_PAD.encode(&bytes) != text {
                return Err(invalid());
            }
            Ok(bytes)
        };
        let plugin = String::from_utf8(decode(plugin_text)?).map_err(|_| invalid())?;
        if plugin.is_empty() || plugin.len() > 192 || decode(digest_text)?.len() != 32 {
            return Err(invalid());
        }
        let tag = decode(tag_text)?;
        let body = token.rsplit_once('.').ok_or_else(invalid)?.0;
        hmac::verify(self.key(), body.as_bytes(), &tag).map_err(|_| invalid())?;
        Ok((ResourceId(token.into()), plugin, class))
    }
}

fn measure(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
    bytes: &mut usize,
) -> Result<(), RegistryError> {
    if depth > MAX_DEPTH || *nodes == 0 {
        return Err(RegistryError::InvalidReference);
    }
    *nodes -= 1;
    *bytes += size_of::<Value>();
    match value {
        Value::Bytes(v) => *bytes += v.capacity(),
        Value::Text(v) => *bytes += v.capacity(),
        Value::Array(v) => {
            *bytes += (v.capacity() - v.len()) * size_of::<Value>();
            for item in v {
                measure(item, depth + 1, nodes, bytes)?;
            }
        }
        Value::Map(v) => {
            *bytes += (v.capacity() - v.len()) * size_of::<(Value, Value)>();
            for (key, value) in v {
                measure(key, depth + 1, nodes, bytes)?;
                measure(value, depth + 1, nodes, bytes)?;
            }
        }
        Value::Tag(_, v) => measure(v, depth + 1, nodes, bytes)?,
        Value::Integer(_) | Value::Float(_) | Value::Bool(_) | Value::Null => {}
        _ => return Err(RegistryError::InvalidReference),
    }
    if *bytes > MAX_REFERENCE_BYTES {
        return Err(RegistryError::InvalidReference);
    }
    Ok(())
}

fn header(major: u8, n: u64, out: &mut Vec<u8>) {
    if n < 24 {
        out.push(major << 5 | n as u8);
    } else {
        let (extra, bytes) = if n <= u8::MAX.into() {
            (24, 1)
        } else if n <= u16::MAX.into() {
            (25, 2)
        } else if n <= u32::MAX.into() {
            (26, 4)
        } else {
            (27, 8)
        };
        out.push(major << 5 | extra);
        out.extend_from_slice(&n.to_be_bytes()[8 - bytes..]);
    }
}

/// RFC 8949 §4.2.1, including bytewise (not length-first) map sorting.
fn canonical(value: &Value) -> Result<Vec<u8>, RegistryError> {
    let mut out = Vec::new();
    encode(value, &mut out)?;
    Ok(out)
}

fn encode(value: &Value, out: &mut Vec<u8>) -> Result<(), RegistryError> {
    match value {
        Value::Array(values) => {
            header(4, values.len() as u64, out);
            for value in values {
                encode(value, out)?;
            }
        }
        Value::Map(values) => {
            let mut pairs = values
                .iter()
                .map(|(key, value)| Ok((canonical(key)?, value)))
                .collect::<Result<Vec<_>, RegistryError>>()?;
            pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            if pairs.windows(2).any(|p| p[0].0 == p[1].0) {
                return Err(RegistryError::InvalidReference);
            }
            header(5, pairs.len() as u64, out);
            for (key, value) in pairs {
                out.extend_from_slice(&key);
                encode(value, out)?;
            }
        }
        Value::Tag(tag, value) => {
            header(6, *tag, out);
            encode(value, out)?;
        }
        Value::Float(v) if v.is_nan() => out.extend_from_slice(&[0xf9, 0x7e, 0x00]),
        _ => ciborium::into_writer(value, out).map_err(|_| RegistryError::InvalidReference)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_canonical_cbor_uses_core_order_not_length_first() {
        let map = Value::Map(vec![
            (Value::Integer((-1).into()), Value::Null),
            (Value::Integer(24.into()), Value::Bool(true)),
        ]);
        assert_eq!(
            canonical(&map).unwrap(),
            [0xa2, 0x18, 0x18, 0xf5, 0x20, 0xf6]
        );
        assert_eq!(canonical(&Value::Float(1.5)).unwrap(), [0xf9, 0x3e, 0x00]);
        assert_eq!(canonical(&Value::Float(-0.0)).unwrap(), [0xf9, 0x80, 0x00]);
        assert_eq!(
            canonical(&Value::Float(f64::NAN)).unwrap(),
            [0xf9, 0x7e, 0x00]
        );
    }
}
