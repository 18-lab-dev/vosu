use super::TreeIndexError as ResourceRequestError;
use ring::digest;
use serde::Serialize;
use std::collections::BTreeMap;
use vosu_plugin_api::{ResourceResolved, TreeEntry, VOSU_FRAME_MAX_BYTES};

pub const MAX_TREE_ENTRIES: usize = 16_384;

#[derive(Clone, Debug, Serialize)]
pub struct TreeIndexEntry {
    pub path: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub media_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TreeIndex {
    pub entries: Vec<TreeIndexEntry>,
    pub truncated: bool,
    pub revision: String,
    #[serde(skip)]
    pub etag: String,
    #[serde(skip)]
    members: BTreeMap<String, usize>,
}

impl TreeIndex {
    pub fn validate(
        entries: Vec<TreeEntry>,
        truncated: bool,
        revision: String,
        families: &[String],
    ) -> Result<Self, ResourceRequestError> {
        if entries.len() > MAX_TREE_ENTRIES || revision.len() > 4096 {
            return Err(ResourceRequestError::InvalidReply);
        }
        let text_bytes = entries
            .iter()
            .try_fold(revision.len(), |total, entry| {
                total
                    .checked_add(entry.path.len())?
                    .checked_add(entry.hash.as_ref().map_or(0, String::len))
            })
            .ok_or(ResourceRequestError::InvalidReply)?;
        if text_bytes > VOSU_FRAME_MAX_BYTES as usize {
            return Err(ResourceRequestError::InvalidReply);
        }
        // Check the complete wire shape, not just the sum of path lengths.
        let reply = ResourceResolved::Tree {
            entries,
            truncated,
            revision,
        };
        let mut wire = Vec::new();
        ciborium::into_writer(&reply, &mut wire).map_err(|_| ResourceRequestError::InvalidReply)?;
        if wire.len() > VOSU_FRAME_MAX_BYTES as usize {
            return Err(ResourceRequestError::InvalidReply);
        }
        let ResourceResolved::Tree {
            entries,
            truncated,
            revision,
        } = reply
        else {
            unreachable!()
        };
        let mut members = BTreeMap::new();
        let mut projected = Vec::with_capacity(entries.len());
        for entry in entries {
            super::path::validate(&entry.path)?;
            if members
                .insert(entry.path.clone(), projected.len())
                .is_some()
            {
                return Err(ResourceRequestError::InvalidReply);
            }
            projected.push(TreeIndexEntry {
                media_type: super::media::published_type(&entry.path, families),
                path: entry.path,
                size: entry.size,
                hash: entry.hash,
            });
        }
        let mut index = Self {
            entries: projected,
            truncated,
            revision,
            etag: String::new(),
            members,
        };
        let body = serde_json::to_vec(&index).map_err(|_| ResourceRequestError::InvalidReply)?;
        use std::fmt::Write;
        let mut etag = String::with_capacity(66);
        etag.push('"');
        for byte in digest::digest(&digest::SHA256, &body).as_ref() {
            write!(&mut etag, "{byte:02x}").expect("writing into a String cannot fail");
        }
        etag.push('"');
        index.etag = etag;
        Ok(index)
    }

    /// Exact indexed identity; case/fallback matching belongs to the plugin.
    pub fn member(&self, canonical: &str) -> Result<&TreeIndexEntry, ResourceRequestError> {
        self.members
            .get(canonical)
            .map(|i| &self.entries[*i])
            .ok_or(ResourceRequestError::InvalidReply)
    }
}
