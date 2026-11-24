//! `/json/v2` and `/websocket/v2` share this immutable bus projection.
use crate::{
    Error,
    dto::*,
    source::*,
    view::{self, Identity},
};
use bytes::Bytes;
use std::sync::Arc;
use vosu_osu_model::source::TourneyAvailability;
use vosu_plugin_api::Selector;
use vosu_surface::{
    requester::BundledRequester,
    state::{CapturedSelection, SessionTarget},
};

const PATHS: &[&str] = &[
    "/session/paused",
    "/session/playTime",
    "/session/server",
    "/session/state",
    "/rankedPlay",
    "/settings",
    "/profile",
    "/beatmap",
    "/play/source",
    "/play/failed",
    "/play/playerName",
    "/play/mode",
    "/play/score",
    "/play/accuracyPercent",
    "/play/healthRaw",
    "/play/judgements",
    "/play/statistics",
    "/play/hitErrors/all",
    "/play/combo",
    "/play/mods",
    "/play/rank",
    "/play/pp",
    "/play/unstableRate",
    "/leaderboard",
    "/performance",
    "/result",
    "/internal/paths",
    "/tourney",
    "/tourneyAvailability",
    "/room",
];

pub fn selectors() -> Vec<Selector> {
    let mut selectors = view::selectors(PATHS);
    selectors.push(Selector {
        ns: "vosu.skin".into(),
        pointer: "/name".into(),
    });
    selectors
}

#[derive(Debug)]
pub struct View {
    pub identity: Identity,
    response: Response,
}
impl View {
    /// One-shot captures never create sampling demand or force a sample.
    pub fn capture(
        requester: &dyn BundledRequester,
        target: SessionTarget,
    ) -> Result<Arc<Self>, Error> {
        Self::from_capture(&requester.capture(target, &selectors())?).map(Arc::new)
    }
    pub fn from_capture(capture: &CapturedSelection) -> Result<Self, Error> {
        let identity = Identity::from_capture(capture)?;
        let lazer = identity.session.variant == "lazer";
        let availability = view::decode::<TourneyAvailability>(capture, "/tourneyAvailability")?;
        let tourney = match availability {
            TourneyAvailability::Pending => None,
            TourneyAvailability::MissingScore => return Err(Error::MissingTourneyScore),
            TourneyAvailability::Ready => {
                view::decode::<Option<SourceTourney>>(capture, "/tourney")?
                    .map(|t| t.into_wire(lazer))
            }
        };
        let mut settings: Settings = view::decode(capture, "/settings")?;
        settings.skin.name =
            view::decode_ns::<Option<String>>(capture, "vosu.skin", "/name")?.unwrap_or_default();
        let mut profile: Profile = view::decode(capture, "/profile")?;
        if profile.id.is_some() && profile.matchmaking.is_none() {
            profile.matchmaking = Some(None);
        }
        let raw: vosu_osu_model::source::HealthRaw = view::decode(capture, "/play/healthRaw")?;
        let play = SourcePlay {
            source: view::decode(capture, "/play/source")?,
            failed: view::decode(capture, "/play/failed")?,
            player_name: view::decode(capture, "/play/playerName")?,
            mode: view::decode(capture, "/play/mode")?,
            score: view::decode(capture, "/play/score")?,
            accuracy: view::decode(capture, "/play/accuracyPercent")?,
            health_bar: HealthBar {
                normal: (raw.normal / 200.0) * 100.0,
                smooth: (raw.smooth / 200.0) * 100.0,
            },
            judgements: view::decode(capture, "/play/judgements")?,
            statistics: view::decode(capture, "/play/statistics")?,
            hit_errors: HitErrors {
                all: view::decode(capture, "/play/hitErrors/all")?,
            },
            combo: view::decode(capture, "/play/combo")?,
            mods: view::decode(capture, "/play/mods")?,
            rank: view::decode(capture, "/play/rank")?,
            pp: view::decode(capture, "/play/pp")?,
            unstable_rate: view::decode(capture, "/play/unstableRate")?,
        }
        .into_wire(false);
        let paths: Paths = view::decode(capture, "/internal/paths")?;
        let current = paths.current.unwrap_or_default();
        let (game, songs) = if lazer {
            let p = paths.lazer.ok_or(Error::NotReady)?;
            (p.data.unwrap_or_default(), p.files.unwrap_or_default())
        } else {
            let p = paths.stable.ok_or(Error::NotReady)?;
            (p.game.unwrap_or_default(), p.songs.unwrap_or_default())
        };
        let skin = paths.skin_folder.unwrap_or_default();
        let direct_path = DirectPath {
            beatmap_file: crate::paths::direct_join(
                current.beatmap_folder.as_deref(),
                current.beatmap_file.as_deref(),
            ),
            beatmap_background: crate::paths::direct_join(
                current.beatmap_folder.as_deref(),
                current.background.as_deref(),
            ),
            beatmap_audio: crate::paths::direct_join(
                current.beatmap_folder.as_deref(),
                current.audio.as_deref(),
            ),
            beatmap_folder: current.beatmap_folder.clone(),
            skin_folder: skin.clone(),
        };
        let response = Response {
            game: Game {
                paused: view::decode(capture, "/session/paused")?,
                focused: cfg!(target_os = "windows").then(|| {
                    capture
                        .roster
                        .sessions
                        .iter()
                        .filter(|session| {
                            session.owner == identity.session.owner
                                && session.owner_instance == identity.session.owner_instance
                        })
                        .any(|session| session.foreground == Some(true))
                }),
            },
            client: identity.session.variant.clone(),
            server: view::decode::<Option<String>>(capture, "/session/server")?
                .unwrap_or_else(|| "ppy.sh".into()),
            state: view::decode(capture, "/session/state")?,
            session: Session {
                play_time: view::decode(capture, "/session/playTime")?,
                play_count: 0,
            },
            ranked_play: view::decode(capture, "/rankedPlay")?,
            settings,
            profile,
            beatmap: view::decode(capture, "/beatmap")?,
            play,
            leaderboard: view::decode(capture, "/leaderboard")?,
            performance: view::decode(capture, "/performance")?,
            results_screen: view::decode(capture, "/result")?,
            folders: Folders {
                game,
                songs,
                skin,
                beatmap: current.beatmap_folder,
            },
            files: Files {
                beatmap: current.beatmap_file,
                background: current.background,
                audio: current.audio,
            },
            direct_path,
            tourney,
            room: view::decode(capture, "/room")?,
        };
        Ok(Self { identity, response })
    }
    pub fn response(&self) -> &Response {
        &self.response
    }
}

pub fn build(view: &View) -> &Response {
    view.response()
}
pub fn encode(view: &View) -> Result<Bytes, Error> {
    crate::wire::regular(view.response())
}
pub fn filtered(view: &View) -> vosu_adapter_support::filtered::SharedDocument<&Response> {
    vosu_adapter_support::filtered::SharedDocument::new(view.response())
}
