use crate::{Error, view::View};
use vosu_osu_calc::beatmap::{AppliedMod, ModSet, Ruleset, ScoringMode};
use vosu_osu_model::{mods_checksum, mods_for_calculation};
use vosu_plugin_api::CborValue;
use vosu_surface::state::IdentifiedSession;

/// A selected resource is authority only when the surface requester authorizes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartKey {
    pub(crate) session: String,
    pub(crate) generation: u64,
    pub(crate) owner: String,
    pub(crate) owner_instance: u64,
    pub(crate) token: String,
    pub(crate) mode: Ruleset,
    pub(crate) mods: ModSet,
    pub(crate) scoring: ScoringMode,
    pub(crate) audio_length: Option<u64>,
}
impl ChartKey {
    /// Inspect dependencies without starting, retrying or retaining chart work.
    pub fn from_view(view: &View) -> Result<Option<Self>, Error> {
        Job::from_view(view).map(|job| job.map(|job| job.key))
    }
}
#[derive(Clone)]
pub(crate) struct Job {
    pub key: ChartKey,
    pub session: IdentifiedSession,
}
impl Job {
    pub fn from_view(view: &View) -> Result<Option<Self>, Error> {
        let session = view.capture.session.clone().ok_or(Error::NotReady)?;
        let Some(entry) = view
            .capture
            .values
            .iter()
            .find(|e| e.ns == "vosu.chart" && e.pointer == "/resources/file")
        else {
            return Ok(None);
        };
        if entry.error.is_some() {
            return Ok(None);
        }
        let Some(value) = entry
            .value
            .as_ref()
            .filter(|v| !matches!(v, CborValue::Null))
        else {
            return Ok(None);
        };
        #[derive(serde::Deserialize)]
        struct Reference {
            url: String,
        }
        let reference: Reference = value
            .deserialized()
            .map_err(|e| Error::Source(e.to_string()))?;
        let token = reference
            .url
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(Error::NotReady)?
            .to_owned();
        let scoring = match session.variant.as_str() {
            "stable" => ScoringMode::Stable,
            "lazer" => ScoringMode::Lazer,
            _ => return Err(Error::Source("unknown chart scoring variant".into())),
        };
        let mode = view
            .mode()
            .and_then(|m| u8::try_from(m).ok())
            .and_then(Ruleset::from_id)
            .ok_or(Error::NotReady)?;
        let sanitized = mods_for_calculation(&view.play.mods.array, scoring == ScoringMode::Lazer);
        let mods = sanitized
            .iter()
            .map(|m| {
                Ok(AppliedMod {
                    acronym: m.acronym.clone(),
                    settings_json: m
                        .settings
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|e| Error::Source(e.to_string()))?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let mods = ModSet::new(mods_checksum(&sanitized), mods, view.play.mods.rate)
            .map_err(|e| Error::Source(e.to_string()))?;
        Ok(Some(Self {
            key: ChartKey {
                session: session.session.clone(),
                generation: session.generation,
                owner: session.owner.clone(),
                owner_instance: session.owner_instance,
                token,
                mode,
                mods,
                scoring,
                audio_length: view.metadata.time.mp3_length.map(f64::to_bits),
            },
            session,
        }))
    }
}
