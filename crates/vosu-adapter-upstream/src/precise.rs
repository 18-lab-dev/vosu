//! Upstream precise keys and hit history, without ADR 0021 extensions.
use crate::{
    Error,
    dto::*,
    source::PreciseTourneySource,
    view::{self, Identity},
};
use bytes::Bytes;
use std::{cmp::Ordering, sync::Arc};
use vosu_osu_model::source::{OverlayButton, OverlaySlots};
use vosu_plugin_api::Selector;
use vosu_surface::{
    requester::BundledRequester,
    state::{CapturedSelection, SessionTarget},
};

pub fn selectors() -> Vec<Selector> {
    view::selectors(&["/keys/overlay", "/play/hitErrors/all", "/tourney"])
}

#[derive(Debug)]
pub struct View {
    pub identity: Identity,
    response: PreciseResponse,
}
impl View {
    pub fn capture(
        requester: &dyn BundledRequester,
        target: SessionTarget,
    ) -> Result<Arc<Self>, Error> {
        Self::from_capture(&requester.capture(target, &selectors())?).map(Arc::new)
    }
    pub fn from_capture(capture: &CapturedSelection) -> Result<Self, Error> {
        let identity = Identity::from_capture(capture)?;
        // The pinned precise tournament traversal is stable manager/IPC only.
        let tourney = if identity.session.variant == "stable" {
            let source: Option<PreciseTourneySource> = view::decode(capture, "/tourney")?;
            let mut clients: Vec<_> = source
                .into_iter()
                .flat_map(|t| t.clients)
                .map(|client| PreciseTourney {
                    ipc_id: client.ipc_id,
                    keys: keys(client.keys_overlay),
                    hit_errors: client.play.hit_errors.all,
                })
                .collect();
            // JS sort is stable, and a NaN comparator behaves like equality.
            clients.sort_by(|a, b| a.ipc_id.partial_cmp(&b.ipc_id).unwrap_or(Ordering::Equal));
            clients
        } else {
            Vec::new()
        };
        Ok(Self {
            identity,
            response: PreciseResponse {
                keys: keys(view::decode(capture, "/keys/overlay")?),
                hit_errors: view::decode(capture, "/play/hitErrors/all")?,
                tourney,
            },
        })
    }
    pub fn response(&self) -> &PreciseResponse {
        &self.response
    }
}

fn button(button: Option<OverlayButton>) -> KeyOverlayButton {
    button.map_or(
        KeyOverlayButton {
            is_pressed: false,
            count: 0,
        },
        |b| KeyOverlayButton {
            is_pressed: b.is_pressed,
            count: b.count,
        },
    )
}
fn keys(slots: Option<OverlaySlots>) -> Keys {
    let slots = slots.unwrap_or(OverlaySlots {
        k1: None,
        k2: None,
        m1: None,
        m2: None,
    });
    Keys {
        k1: button(slots.k1),
        k2: button(slots.k2),
        m1: button(slots.m1),
        m2: button(slots.m2),
    }
}

pub fn build(view: &View) -> &PreciseResponse {
    view.response()
}
pub fn encode(view: &View) -> Result<Bytes, Error> {
    crate::wire::precise(view.response())
}
pub fn filtered(view: &View) -> vosu_adapter_support::filtered::SharedDocument<&PreciseResponse> {
    vosu_adapter_support::filtered::SharedDocument::new(view.response())
}
