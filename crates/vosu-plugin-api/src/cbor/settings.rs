//! Settings, CBOR (`docs/v2/plugin-abi.md` §6.9).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

use crate::cbor::common::deserialize_cbor_map;

/// `VOSU_OP_SETTINGS_CHANGED` (host -> plugin, no reply): `values` is
/// always the complete effective settings object in `init.settings`'s own
/// shape, never a diff of only the changed keys, and always a CBOR map
/// (minor 9): a non-map value is rejected on decode.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SettingsChanged {
    #[serde(deserialize_with = "deserialize_cbor_map")]
    pub values: CborValue,
}
