//! Schema-driven sampling demand (MC/MJ), independent of delivery.
use std::sync::Arc;

use super::{LaneBits, Pointer, SchemaRegistry};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DemandSnapshot {
    pub regular: bool,
    pub precise: bool,
    /// Complete selector set, sorted and deduplicated, including slow fields.
    pub selectors: Vec<(Arc<str>, Arc<str>)>,
}

pub(super) fn compile(reg: &SchemaRegistry, selectors: &[(Arc<str>, Pointer)]) -> LaneBits {
    selectors.iter().fold(LaneBits::default(), |bits, (ns, p)| {
        let mut tokens = [""; 32];
        for (slot, token) in tokens.iter_mut().zip(p.tokens()) {
            *slot = token;
        }
        bits.union(reg.lanes(ns, &tokens[..p.tokens().len()]))
    })
}
