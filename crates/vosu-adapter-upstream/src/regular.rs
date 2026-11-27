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

pub(crate) const PLAY_CLOCK: &str = "/internal/playClock";

const PATHS: &[&str] = &[
    PLAY_CLOCK,
    "/session/playTime",
    "/session/server",
    "/session/state",
    "/rankedPlay",
    "/settings",
    "/profile",
    "/beatmap",
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

#[derive(serde::Deserialize)]
struct PlayClock {
    previous: f64,
    current: f64,
}

fn legacy_paused(capture: &CapturedSelection) -> Result<Option<bool>, Error> {
    match view::decode::<PlayClock>(capture, PLAY_CLOCK) {
        Ok(clock) => Ok(Some(clock.previous == clock.current)),
        // An older captured image cannot supply the preceding sample.
        Err(Error::NotReady) => Ok(None),
        Err(error) => Err(error),
    }
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
        let mut play = SourcePlay {
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
        let skin =
            vosu_adapter_support::skin_folder(lazer, paths.skin_folder.as_deref()).to_owned();
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
        let state: NumberName = view::decode(capture, "/session/state")?;
        let beatmap: Beatmap = view::decode(capture, "/beatmap")?;
        let performance: Performance = view::decode(capture, "/performance")?;
        play.pp.fc = vosu_adapter_support::full_combo_pp(
            play.pp.fc,
            performance.accuracy.acc_100,
            state.number.unwrap_or_default(),
            beatmap.time.live,
            beatmap.time.first_object,
        );
        play.pp.detailed.fc.total = play.pp.fc;
        let response = Response {
            game: Game {
                paused: legacy_paused(capture)?,
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
            state,
            session: Session {
                play_time: view::decode(capture, "/session/playTime")?,
                play_count: 0,
            },
            ranked_play: view::decode(capture, "/rankedPlay")?,
            settings,
            profile,
            beatmap,
            play,
            leaderboard: view::decode(capture, "/leaderboard")?,
            performance,
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
