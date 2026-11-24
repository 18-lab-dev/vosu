//! Embedded core schema documents; plugin-owned schemas are registered by
//! their bundled/development plugin. The osu schema below is a test fixture.

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
#[cfg(test)]
pub(super) const VOSU_OSU: &str =
    include_str!("../../../../../docs/v2/schemas/vosu.osu.schema.json");
