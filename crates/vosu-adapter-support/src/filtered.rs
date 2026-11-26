//! Legacy filter semantics and per-broadcast-pass shared wire documents.
use bytes::Bytes;
use serde::Serialize;
use std::{borrow::Cow, io};
use vosu_surface::filter::Filter;

mod parse;
pub use parse::parse_filters;
mod selection;
mod value;

use selection::{Account, SelectedKey, serialize_accounted};
pub use selection::{Selected, Selection, serialize_selected};
pub use value::{
    ObjectKey, ProjectionValue, encode_javascript_json, encode_projection, javascript_array_index,
    javascript_property_ref, projection_value_from_json, write_projection_json,
};
use value::{encode_root, filter_field};

/// Whether a legacy plan can observe a wire path. Empty plans mean the whole
/// document; an explicitly empty nested plan observes no descendants.
pub fn selects_path(filters: &[Filter], path: &[&str]) -> bool {
    filters.is_empty() || selects_nonempty(filters, path)
}

fn selects_nonempty(filters: &[Filter], path: &[&str]) -> bool {
    let Some((head, tail)) = path.split_first() else {
        return true;
    };
    filters.iter().any(|filter| match filter {
        Filter::Field(field) => field == head,
        Filter::Nested { field, keys } => {
            field == head && !keys.is_empty() && (tail.is_empty() || selects_nonempty(keys, tail))
        }
    })
}

/// A wire document and what the plans projected from it so far share.
pub struct SharedDocument<D> {
    document: D,
    shared: SharedProperties,
    /// Every frame is written here, then copied out at its exact length.
    scratch: Vec<u8>,
}

impl<D: Serialize> SharedDocument<D> {
    pub fn new(document: D) -> Self {
        Self {
            document,
            shared: SharedProperties::default(),
            scratch: Vec::new(),
        }
    }

    /// Whether the document property `key` is now projected from its whole
    /// value rather than walked for each plan.
    pub fn is_shared(&self, key: &str) -> bool {
        self.shared.whole(key).is_some()
    }

    /// The JSON the projection of the document through `filters` encodes to.
    pub fn encode(&mut self, filters: &[Filter]) -> Result<Bytes, serde_json::Error> {
        // Whether some selected property still needs a walk of its own.
        let mut walk = false;
        for (position, filter) in filters.iter().enumerate() {
            let key = filter_field(filter);
            if filters[..position]
                .iter()
                .any(|earlier| filter_field(earlier) == key)
            {
                continue;
            }
            if let Some((key, budget)) = self.shared.budget(key) {
                self.share(key, budget)?;
            }
            walk |= self.shared.whole(key).is_none();
        }
        let partial = if !walk {
            ProjectionValue::Object(Vec::new())
        } else {
            let mut selected = Selected::from_filters(filters);
            selected
                .keys
                .retain(|entry| self.shared.whole(entry.key).is_none());
            let account = Account::new(usize::MAX);
            let partial = serialize_accounted(&self.document, &selected, &account)?;
            self.shared.spend(account.into_properties());
            partial
        };
        let shared = &self.shared;
        encode_root(&mut self.scratch, filters, &|key| match shared.whole(key) {
            Some(whole) => whole.as_ref().map(Cow::Borrowed),
            None => javascript_property_ref(&partial, key),
        })
    }

    /// Serializes the document property `key` whole, unless that charges more
    /// than `budget`.
    fn share(&mut self, key: &'static str, budget: usize) -> Result<(), serde_json::Error> {
        let whole = Selected {
            keys: vec![SelectedKey {
                key,
                index: javascript_array_index(key),
                selection: Selection::Full,
            }],
        };
        let account = Account::new(budget);
        let result = serialize_accounted(&self.document, &whole, &account);
        // Checked first, so a value is refused even if some serializer
        // swallowed the error of the charge that exceeded the budget.
        if account.exceeded() {
            self.shared.refuse(key, budget);
            return Ok(());
        }
        let value = match result {
            Ok(value) => value,
            Err(_) => {
                // A full property may fail even when a selected subtree succeeds.
                // Never attempt to share it again during this broadcast pass.
                self.shared.refuse(key, usize::MAX);
                return Ok(());
            }
        };
        let value = match value {
            // Only `key` was selected, so it is the sole entry.
            ProjectionValue::Object(mut entries) => entries.pop().map(|(_, value)| value),
            document => javascript_property_ref(&document, key).map(Cow::into_owned),
        };
        self.shared.store(key, value);
        Ok(())
    }
}

/// The document's own properties that the plans of one pass select. Each
/// distinct plan serializes only what it selects, so a property that many
/// plans reach is walked again for each. Serializing it whole once lets every
/// later plan project from that instead, which pays once the walks would
/// have cost more.
///
/// Neither the property's whole size nor the cost of a walk is known in
/// advance, so both are measured as `Account` charges. After a property's
/// first walk, it is serialized whole with a budget of twice the largest walk
/// so far; later attempts are budgeted at `1 / SHARE_RATIO` of all its walk
/// charges. An attempt stops at its budget and the next waits until the
/// budget has doubled, so the work and memory sharing costs stay a fixed
/// fraction of what the walks themselves charged, plus two walks.
#[derive(Default)]
struct SharedProperties {
    properties: Vec<SharedProperty>,
}

const SHARE_RATIO: usize = 16;

struct SharedProperty {
    key: &'static str,
    /// What walks that selected part of the property charged beneath it.
    spent: usize,
    /// The most one walk charged.
    largest_walk: usize,
    /// The budget of the last attempt, which the property exceeded.
    refused: usize,
    /// The property's value in the full document, once serialized whole.
    whole: Option<Option<ProjectionValue>>,
}

impl SharedProperties {
    fn property(&self, key: &str) -> Option<&SharedProperty> {
        self.properties.iter().find(|property| property.key == key)
    }

    fn property_mut(&mut self, key: &str) -> Option<&mut SharedProperty> {
        self.properties
            .iter_mut()
            .find(|property| property.key == key)
    }

    /// The property and the budget to serialize it whole with, if that is due.
    fn budget(&self, key: &str) -> Option<(&'static str, usize)> {
        let property = self.property(key)?;
        let budget = property
            .largest_walk
            .saturating_mul(2)
            .max(property.spent / SHARE_RATIO);
        (property.whole.is_none()
            && property.refused != usize::MAX
            && budget >= property.refused.saturating_mul(2))
        .then_some((property.key, budget))
    }

    fn spend(&mut self, walked: impl Iterator<Item = (&'static str, usize)>) {
        for (key, charged) in walked {
            match self.property_mut(key) {
                Some(property) => {
                    property.spent = property.spent.saturating_add(charged);
                    property.largest_walk = property.largest_walk.max(charged);
                }
                None => self.properties.push(SharedProperty {
                    key,
                    spent: charged,
                    largest_walk: charged,
                    refused: 0,
                    whole: None,
                }),
            }
        }
    }

    fn store(&mut self, key: &str, value: Option<ProjectionValue>) {
        if let Some(property) = self.property_mut(key) {
            property.whole = Some(value);
        }
    }

    fn refuse(&mut self, key: &str, budget: usize) {
        if let Some(property) = self.property_mut(key) {
            property.refused = budget;
        }
    }

    /// The whole value of a shared property, `None` inside where the full
    /// document lacks it.
    fn whole(&self, key: &str) -> Option<&Option<ProjectionValue>> {
        self.property(key)?.whole.as_ref()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod demand_tests {
    use super::*;
    #[test]
    fn observation_matches_whole_nested_and_explicitly_empty_plans() {
        assert!(selects_path(&[], &["play", "hitErrors"]));
        assert!(selects_path(
            &[Filter::Field("play".into())],
            &["play", "hitErrors"]
        ));
        assert!(!selects_path(
            &[Filter::Field("game".into())],
            &["play", "hitErrors"]
        ));
        let only_score = Filter::Nested {
            field: "play".into(),
            keys: vec![Filter::Field("score".into())],
        };
        assert!(!selects_path(&[only_score], &["play", "hitErrors"]));
        let empty = Filter::Nested {
            field: "keys".into(),
            keys: vec![],
        };
        assert!(!selects_path(&[empty], &["keys"]));
        let key = Filter::Nested {
            field: "keys".into(),
            keys: vec![Filter::Field("k1".into())],
        };
        assert!(selects_path(&[key], &["keys"]));
    }
}
