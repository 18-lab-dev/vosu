//! Settings, CBOR (`docs/v2/plugin-abi.md` §6.9).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

/// `VOSU_OP_SETTINGS_CHANGED` (host -> plugin, no reply): `values` is
/// always the complete effective settings object in `init.settings`'s own
/// shape, never a diff of only the changed keys.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SettingsChanged {
    pub values: CborValue,
}
