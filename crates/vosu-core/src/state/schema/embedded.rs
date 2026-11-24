//! `include_str!` of the schema documents under `docs/v2/schemas` — the
//! single source of truth for every core namespace plus `vosu.osu`
//! (plan:574-576, D13: no runtime `jsonschema` dependency). A production
//! build never re-reads these from disk: [`super::SchemaRegistry::with_core`]
//! parses this embedded text once, at construction.

pub(super) const COMMON_DEFS: &str =
    include_str!("../../../../../docs/v2/schemas/common-defs.schema.json");
pub(super) const VOSU_CLIENT: &str =
    include_str!("../../../../../docs/v2/schemas/vosu.client.schema.json");
pub(super) const VOSU_STATUS: &str =
    include_str!("../../../../../docs/v2/schemas/vosu.status.schema.json");
pub(super) const VOSU_CHART: &str =
    include_str!("../../../../../docs/v2/schemas/vosu.chart.schema.json");
pub(super) const VOSU_PLAY: &str =
    include_str!("../../../../../docs/v2/schemas/vosu.play.schema.json");
pub(super) const VOSU_SKIN: &str =
    include_str!("../../../../../docs/v2/schemas/vosu.skin.schema.json");
pub(super) const VOSU_OSU: &str =
    include_str!("../../../../../docs/v2/schemas/vosu.osu.schema.json");
