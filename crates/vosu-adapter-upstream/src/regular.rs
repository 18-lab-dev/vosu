//! `/json/v2` and `/websocket/v2` share this immutable bus projection.
use crate::{
    Error,
    dto::Response,
    view::{self, Identity},
};
use bytes::Bytes;
use std::sync::Arc;
use vosu_plugin_api::Selector;
use vosu_surface::{
    requester::BundledRequester,
    state::{CapturedSelection, SessionTarget},
};

mod decode;
mod decode_extra;
mod direct;
mod direct_extra;
mod direct_settings;
mod parse;
mod parse_settings;
mod slots;

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
        let (identity, response) = decode::decode_response(capture)?;
        Ok(Self { identity, response })
    }
    pub fn response(&self) -> &Response {
        &self.response
    }
}

pub(crate) fn encode_capture(capture: &CapturedSelection) -> Result<Bytes, Error> {
    direct::encode_capture(capture)
}

pub(crate) fn decode_response_only(capture: &CapturedSelection) -> Result<Response, Error> {
    decode::decode_response_only(capture)
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
