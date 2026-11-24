//! The hand-written lane walker (S7, S12; plan:574-579, D7, D13;
//! ss:386-442): walks one namespace schema document's `properties`/
//! `items`/`additionalProperties`/`oneOf`/`anyOf`/`allOf`/`$ref` tree,
//! building the sampling-lane table, and enforcing the D7 bounds
//! (cat:58-85). The `x-visibility` carve-out itself is validated
//! separately, over the *entire* document ([`visibility::scan`]), since the
//! carve-out must hold regardless of which keywords this walker follows
//! for lanes (ss:297-311). Not a general JSON Schema evaluator: it reads
//! exactly the keywords above plus `x-sampling-lane`/`title`/`version`/
//! `required`, and nothing else.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;

use super::lanes::LaneBits;
use super::visibility;
use super::{SchemaError, SchemaOwner};
use crate::state::Pointer;

/// At most 256 KiB of schema text (D7, cat:66).
const MAX_BYTES: usize = 256 * 1024;
/// At most 32 levels of JSON structural nesting (D7, cat:66).
const MAX_DEPTH: usize = 32;
/// Total schema nodes (and so lane entries) one walk will visit before
/// refusing to continue. Not a named D7 bound, but the same intent: a
/// schema within the byte/depth bounds can still re-expand a shared `$ref`
/// exponentially through `allOf`/`oneOf` fan-out (a handful of KB, 6 levels
/// deep, ~2^40 node visits for a 40-def chain each referencing its
/// successor twice) — this cap turns that into a bounded, fast error
/// instead of unbounded work.
const MAX_NODE_VISITS: usize = 50_000;

/// What the walker extracts from one namespace schema document.
pub(super) struct Walked {
    pub(super) version: semver::Version,
    pub(super) required_top_level: Vec<Arc<str>>,
    pub(super) lanes: Vec<(Vec<Arc<str>>, LaneBits)>,
    /// Pointers (e.g. `Pointer::parse("/internal")`) carrying `x-visibility`.
    pub(super) hidden: Vec<Pointer>,
}

/// Walks `text`, a namespace schema document whose `title` must equal
/// `namespace`. `owner` is who is registering it: the `x-visibility`
/// carve-out is gated on the owner as well as the title, so a same-titled
/// schema registered under a different owner never gets it (finding 3).
/// `common_defs` is the parsed `common-defs.schema.json`'s `$defs` object,
/// for `common-defs.schema.json#/$defs/*` refs.
pub(super) fn walk(
    namespace: &str,
    owner: &SchemaOwner,
    text: &str,
    common_defs: &Value,
) -> Result<Walked, SchemaError> {
    if text.len() > MAX_BYTES {
        return Err(SchemaError::TooLarge);
    }
    let root: Value = serde_json::from_str(text).map_err(|e| SchemaError::Json(e.to_string()))?;
    if json_depth(&root) > MAX_DEPTH {
        return Err(SchemaError::TooDeep);
    }

    let title = root
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if title != namespace {
        return Err(SchemaError::TitleMismatch {
            title: title.to_string(),
            provides: namespace.to_string(),
        });
    }

    // Independent of the lane walk below: reject any x-visibility anywhere
    // outside the one allowed, owner-gated location (ss:297-311, cat:70-72).
    visibility::scan(namespace, owner, &root)?;

    let version_str = root
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| SchemaError::Json(format!("{namespace}: missing \"version\"")))?;
    let version = semver::Version::parse(version_str)
        .map_err(|e| SchemaError::Json(format!("{namespace}: invalid version: {e}")))?;

    let required_top_level = root
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(Arc::from).collect())
        .unwrap_or_default();

    let own_defs = root.get("$defs");
    let mut walker = Walker {
        own_defs,
        common_defs,
        visiting: HashSet::new(),
        node_visits: 0,
        lanes: Vec::new(),
    };

    if let Some(props) = root.get("properties").and_then(Value::as_object) {
        for (key, schema) in props {
            let mut path = vec![Arc::<str>::from(key.as_str())];
            walker.node(schema, &mut path)?;
        }
    }

    Ok(Walked {
        version,
        required_top_level,
        lanes: walker.lanes,
        hidden: visibility::hidden_subtrees(namespace, owner, &root),
    })
}

/// JSON value nesting depth: an object/array one level deeper than its
/// deepest child; a plain value (string, number, bool, null) contributes no
/// nesting of its own, so an empty object/array is depth 1 (cat:66's "at
/// most 32 levels of JSON structural nesting" — 32 levels of pure object
/// nesting must pass, 33 must not).
fn json_depth(v: &Value) -> usize {
    match v {
        Value::Object(m) => 1 + m.values().map(json_depth).max().unwrap_or(0),
        Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

struct Walker<'a> {
    own_defs: Option<&'a Value>,
    common_defs: &'a Value,
    /// `$ref` strings currently being resolved, for `CyclicRef` detection.
    visiting: HashSet<String>,
    /// Total `node` calls so far this walk (`MAX_NODE_VISITS`).
    node_visits: usize,
    lanes: Vec<(Vec<Arc<str>>, LaneBits)>,
}

impl Walker<'_> {
    /// Walks one schema node at `path`, recording its `x-sampling-lane`
    /// annotation (if any) and recursing into `$ref`/`properties`/`items`/
    /// `additionalProperties`/`oneOf`/`anyOf`/`allOf`.
    fn node(&mut self, node: &Value, path: &mut Vec<Arc<str>>) -> Result<(), SchemaError> {
        self.node_visits += 1;
        if self.node_visits > MAX_NODE_VISITS {
            return Err(SchemaError::TooManyNodes);
        }

        let Some(obj) = node.as_object() else {
            return Ok(());
        };

        if let Some(lane) = obj.get("x-sampling-lane") {
            let lane_obj = lane.as_object().ok_or_else(|| {
                SchemaError::InvalidLane(format!(
                    "x-sampling-lane at /{} is not an object",
                    path.join("/")
                ))
            })?;
            let lane_s = lane_obj
                .get("lane")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let activity_s = lane_obj
                .get("activity")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let bits = LaneBits::from_annotation(lane_s, activity_s).ok_or_else(|| {
                SchemaError::InvalidLane(format!(
                    "x-sampling-lane at /{} has lane={lane_s:?} activity={activity_s:?}",
                    path.join("/")
                ))
            })?;
            self.lanes.push((path.clone(), bits));
        }

        if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
            return self.follow_ref(reference, path);
        }

        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            for (key, schema) in props {
                path.push(Arc::from(key.as_str()));
                self.node(schema, path)?;
                path.pop();
            }
        }

        if let Some(items) = obj.get("items") {
            path.push(Arc::from("*"));
            self.node(items, path)?;
            path.pop();
        }

        // A schema-shaped additionalProperties is a map value's shape
        // (`true`/`false` carry no schema to walk): a lane annotation there
        // must still raise demand for a selector into the map, never be
        // silently dropped (reviewer finding 1).
        if let Some(additional) = obj.get("additionalProperties")
            && additional.is_object()
        {
            path.push(Arc::from("*"));
            self.node(additional, path)?;
            path.pop();
        }

        for combinator in ["oneOf", "anyOf", "allOf"] {
            if let Some(branches) = obj.get(combinator).and_then(Value::as_array) {
                for branch in branches {
                    self.node(branch, path)?;
                }
            }
        }

        Ok(())
    }

    /// Resolves `reference` to `#/$defs/*` (this schema's own) or
    /// `common-defs.schema.json#/$defs/*`; any other target is a
    /// `ForeignRef` (the walker rule, cat:67-68). Walks the target at the
    /// same `path`, since `$ref` names no new property segment.
    fn follow_ref(&mut self, reference: &str, path: &mut Vec<Arc<str>>) -> Result<(), SchemaError> {
        let (defs, def_name) = if let Some(name) = reference.strip_prefix("#/$defs/") {
            (self.own_defs, name)
        } else if let Some(name) = reference.strip_prefix("common-defs.schema.json#/$defs/") {
            (Some(self.common_defs), name)
        } else {
            return Err(SchemaError::ForeignRef(reference.to_string()));
        };
        let target = defs
            .and_then(|d| d.get(def_name))
            .ok_or_else(|| SchemaError::UnresolvedRef(reference.to_string()))?;

        if !self.visiting.insert(reference.to_string()) {
            return Err(SchemaError::CyclicRef(reference.to_string()));
        }
        let result = self.node(target, path);
        self.visiting.remove(reference);
        result
    }
}
