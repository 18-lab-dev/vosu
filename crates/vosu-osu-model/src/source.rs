//! Accurate public osu! source facts shared by bus consumers, not wire DTOs.
//! Selection assembly and coherent revision ownership remain adapter responsibilities.
use crate::states::CalculateMods;
use serde::{Deserialize, Deserializer};
use std::sync::Arc;

/// A missing selection is different from an explicitly unavailable source.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Selected<T> {
    #[default]
    Absent,
    Null,
    Value(T),
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Selected<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayButton {
    pub is_pressed: bool,
    pub count: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct OverlaySlots {
    pub k1: Option<OverlayButton>,
    pub k2: Option<OverlayButton>,
    pub m1: Option<OverlayButton>,
    pub m2: Option<OverlayButton>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct Combo {
    pub current: i64,
    pub max: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct HealthRaw {
    pub normal: f64,
    pub smooth: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct Judgements {
    pub miss: i64,
    pub meh: i64,
    pub ok: i64,
    pub good: i64,
    pub great: i64,
    pub perfect: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TourneyAvailability {
    Pending,
    Ready,
    MissingScore,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardPlayer {
    pub name: Arc<str>,
    pub score: f64,
    pub combo: i64,
    pub max_combo: i64,
    pub mods: CalculateMods,
    pub judgements: Judgements,
    pub team: i64,
    pub position: i64,
    pub is_passing: bool,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct LeaderboardState {
    pub visible: bool,
    pub player: LeaderboardPlayer,
}
/// Decode once per relevant regular/precise revision, never once per client.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayFacts {
    #[serde(default)]
    pub score: Selected<f64>,
    #[serde(default)]
    pub combo: Selected<Combo>,
    #[serde(default)]
    pub accuracy_percent: Selected<f64>,
    #[serde(default)]
    pub health_raw: Selected<HealthRaw>,
    #[serde(default)]
    pub judgements: Selected<Judgements>,
    #[serde(default)]
    pub retries: Selected<i64>,
    #[serde(default)]
    pub precise_unstable_rate: Selected<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_preserve_absence_null_zero_and_slot_identity() {
        let absent: PlayFacts = serde_json::from_str("{}").unwrap();
        let null: PlayFacts = serde_json::from_str(r#"{"score":null}"#).unwrap();
        let zero: PlayFacts = serde_json::from_str(r#"{"score":0}"#).unwrap();
        assert_eq!(absent.score, Selected::Absent);
        assert_eq!(null.score, Selected::Null);
        assert_eq!(zero.score, Selected::Value(0.0));
        let slots: OverlaySlots = serde_json::from_str(
            r#"{"k1":{"isPressed":true,"count":11},"k2":null,"m1":{"isPressed":false,"count":33},"m2":{"isPressed":true,"count":44}}"#,
        ).unwrap();
        assert_eq!(slots.k1.unwrap().count, 11);
        assert_eq!(slots.k2, None);
        assert_eq!(slots.m1.unwrap().count, 33);
        assert_eq!(slots.m2.unwrap().count, 44);
    }
}
