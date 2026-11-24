use super::{
    CompiledSchema, LaneRecord, MAX_BYTES, MAX_DEPTH, MAX_NODE_VISITS, SchemaError,
    SchemaProvenance, lanes,
};
use serde_json::Value;
use std::collections::HashSet;

struct Compiler<'a> {
    root: &'a Value,
    common: &'a Value,
    work: usize,
}
impl<'a> Compiler<'a> {
    fn tick(&mut self) -> Result<(), SchemaError> {
        self.work += 1;
        if self.work > MAX_NODE_VISITS {
            Err(SchemaError::TooManyNodes)
        } else {
            Ok(())
        }
    }
    fn resolve(&self, reference: &str, common: bool) -> Result<(&'a Value, bool), SchemaError> {
        let (tail, external) = if let Some(tail) = reference.strip_prefix('#') {
            (tail, common)
        } else if let Some(tail) = reference.strip_prefix("common-defs.schema.json#") {
            if !tail.starts_with("/$defs/") {
                return Err(SchemaError::ForeignRef(reference.into()));
            }
            (tail, true)
        } else {
            return Err(SchemaError::ForeignRef(reference.into()));
        };
        // state-schema.md Sampling lanes: malformed fragments are rejected
        // references, distinct from malformed catalog mirror pointer records.
        let decoded =
            decode_fragment(tail).map_err(|_| SchemaError::ForeignRef(reference.into()))?;
        let path =
            lanes::tokens(&decoded).map_err(|_| SchemaError::ForeignRef(reference.into()))?;
        let mut node = if external { self.common } else { self.root };
        let skip = usize::from(external);
        if external && path.first().map(String::as_str) != Some("$defs") {
            return Err(SchemaError::ForeignRef(reference.into()));
        }
        for part in &path[skip..] {
            node = match node {
                Value::Object(map) => map.get(part),
                Value::Array(items) => {
                    if part.is_empty()
                        || !part.bytes().all(|b| b.is_ascii_digit())
                        || (part != "0" && part.starts_with('0'))
                    {
                        None
                    } else {
                        part.parse::<usize>()
                            .ok()
                            .and_then(|index| items.get(index))
                    }
                }
                _ => None,
            }
            .ok_or_else(|| SchemaError::UnresolvedRef(reference.into()))?;
        }
        if !node.is_object() && !node.is_boolean() {
            return Err(SchemaError::UnresolvedRef(reference.into()));
        }
        Ok((node, external))
    }
    // Iterative DFS checks every reference, even in unused branches. Node identity
    // includes the trusted-document bit; active nodes detect all graph cycles.
    fn references(&mut self) -> Result<(), SchemaError> {
        let mut stack = vec![(self.root, false, false)];
        let mut active = HashSet::new();
        let mut done = HashSet::new();
        while let Some((node, common, exit)) = stack.pop() {
            let id = (std::ptr::from_ref(node) as usize, common);
            if exit {
                active.remove(&id);
                done.insert(id);
                continue;
            }
            if done.contains(&id) {
                continue;
            }
            if !active.insert(id) {
                return Err(SchemaError::CyclicRef("#".into()));
            }
            self.tick()?;
            stack.push((node, common, true));
            match node {
                Value::Object(map) => {
                    for value in map.values() {
                        stack.push((value, common, false));
                    }
                    if let Some(reference) = map.get("$ref") {
                        let reference = reference
                            .as_str()
                            .ok_or_else(|| SchemaError::ForeignRef("non-string $ref".into()))?;
                        let (target, target_common) = self.resolve(reference, common)?;
                        stack.push((target, target_common, false));
                    }
                }
                Value::Array(items) => {
                    for value in items {
                        stack.push((value, common, false));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn extract(&mut self, start: &'a Value, emit: bool) -> Result<Vec<LaneRecord>, SchemaError> {
        let mut rows = Vec::new();
        let mut stack = vec![(start, false, Vec::<String>::new(), true)];
        while let Some((node, common, path, supported)) = stack.pop() {
            self.tick()?;
            let Some(map) = node.as_object() else {
                continue;
            };
            if let Some(annotation) = map.get("x-sampling-lane") {
                if !supported {
                    return Err(SchemaError::UnsupportedLane(lanes::pointer(&path)));
                }
                let lane = annotation
                    .get("lane")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let activity = annotation
                    .get("activity")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !lanes::valid_annotation(lane, activity)
                    || annotation.as_object().is_none_or(|obj| obj.len() != 2)
                {
                    return Err(SchemaError::InvalidLane(lanes::pointer(&path)));
                }
                if emit {
                    rows.push(LaneRecord {
                        pointer: lanes::pointer(&path),
                        lane: lane.into(),
                        activity: activity.into(),
                    });
                }
            }
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                let (target, common) = self.resolve(reference, common)?;
                stack.push((target, common, path.clone(), supported));
            }
            for (key, value) in map {
                match key.as_str() {
                    "properties" if value.is_object() => {
                        if let Some(properties) = value.as_object() {
                            for (name, child) in properties {
                                if name == "*" {
                                    return Err(SchemaError::InvalidPointer(lanes::pointer(&path)));
                                }
                                let mut next = path.clone();
                                next.push(name.clone());
                                stack.push((child, common, next, supported));
                            }
                        }
                    }
                    "items" | "additionalProperties" if value.is_object() || value.is_boolean() => {
                        let mut next = path.clone();
                        next.push("*".into());
                        stack.push((value, common, next, supported));
                    }
                    "allOf" | "anyOf" | "oneOf" if value.is_array() => {
                        if let Some(branches) = value.as_array() {
                            for child in branches {
                                stack.push((child, common, path.clone(), supported));
                            }
                        }
                    }
                    "$defs" | "definitions" | "$ref" | "x-sampling-lane" => {}
                    _ => {
                        // Inspect unhandled structures without interpreting their value
                        // semantics. A lane reached through them cannot be projected.
                        let mut pending = vec![value];
                        while let Some(child) = pending.pop() {
                            self.tick()?;
                            match child {
                                Value::Object(object) => {
                                    if object.contains_key("x-sampling-lane")
                                        || object.contains_key("$ref")
                                    {
                                        stack.push((child, common, path.clone(), false));
                                    } else {
                                        pending.extend(object.values());
                                    }
                                }
                                Value::Array(array) => pending.extend(array),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        rows.sort();
        rows.dedup();
        Ok(rows)
    }
}

fn scan(
    node: &Value,
    namespace: &str,
    provenance: SchemaProvenance,
    path: &mut Vec<String>,
    depth: usize,
    work: &mut usize,
) -> Result<(), SchemaError> {
    *work += 1;
    if *work > MAX_NODE_VISITS {
        return Err(SchemaError::TooManyNodes);
    }
    let nested = usize::from(node.is_object() || node.is_array());
    if depth + nested > MAX_DEPTH {
        return Err(SchemaError::TooDeep);
    }
    match node {
        Value::Object(map) => {
            if let Some(value) = map.get("x-visibility")
                && (namespace != "vosu.osu"
                    || provenance != SchemaProvenance::TrustedOfficial
                    || path.as_slice() != ["properties", "internal"]
                    || value.as_str() != Some("plugins"))
            {
                return Err(SchemaError::VisibilityNotAllowed {
                    namespace: namespace.into(),
                    at: lanes::pointer(path),
                });
            }
            // Validate annotation syntax everywhere, including unused definitions.
            if let Some(annotation) = map.get("x-sampling-lane") {
                let lane = annotation
                    .get("lane")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let activity = annotation
                    .get("activity")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !lanes::valid_annotation(lane, activity)
                    || annotation.as_object().is_none_or(|a| a.len() != 2)
                {
                    return Err(SchemaError::InvalidLane(lanes::pointer(path)));
                }
            }
            if let Some(properties) = map.get("properties").and_then(Value::as_object)
                && properties.contains_key("*")
            {
                return Err(SchemaError::InvalidPointer(lanes::pointer(path)));
            }
            for (key, child) in map {
                path.push(key.clone());
                scan(child, namespace, provenance, path, depth + nested, work)?;
                path.pop();
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                path.push(index.to_string());
                scan(child, namespace, provenance, path, depth + nested, work)?;
                path.pop();
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn compile(
    namespace: &str,
    expected: &semver::Version,
    provenance: SchemaProvenance,
    text: &str,
    common: &Value,
) -> Result<CompiledSchema, SchemaError> {
    if text.len() > MAX_BYTES {
        return Err(SchemaError::TooLarge);
    }
    let root: Value = serde_json::from_str(text).map_err(|e| SchemaError::Json(e.to_string()))?;
    let mut work = 0;
    scan(&root, namespace, provenance, &mut Vec::new(), 0, &mut work)?;
    let title = root
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if title != namespace {
        return Err(SchemaError::TitleMismatch {
            title: title.into(),
            provides: namespace.into(),
        });
    }
    let version = root
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| SchemaError::Json("missing /version".into()))?;
    let version = semver::Version::parse(version)
        .map_err(|_| SchemaError::Json("invalid /version".into()))?;
    if &version != expected {
        return Err(SchemaError::VersionMismatch);
    }
    let required_top_level = root
        .get("required")
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let hidden = if root.pointer("/properties/internal/x-visibility").is_some() {
        vec!["/internal".into()]
    } else {
        Vec::new()
    };
    let mut compiler = Compiler {
        root: &root,
        common,
        work,
    };
    compiler.references()?;
    let lanes = compiler.extract(&root, true)?;
    // Definitions are legal repositories of annotations, but unsupported
    // lane-bearing structures in unused definitions must fail too.
    let mut pending = vec![&root];
    while let Some(node) = pending.pop() {
        compiler.tick()?;
        match node {
            Value::Object(map) => {
                for key in ["$defs", "definitions"] {
                    if let Some(definitions) = map.get(key).and_then(Value::as_object) {
                        for definition in definitions.values() {
                            compiler.extract(definition, false)?;
                        }
                    }
                }
                pending.extend(map.values());
            }
            Value::Array(array) => pending.extend(array),
            _ => {}
        }
    }
    Ok(CompiledSchema {
        namespace: namespace.into(),
        version,
        lanes,
        required_top_level,
        hidden,
    })
}

fn decode_fragment(fragment: &str) -> Result<std::borrow::Cow<'_, str>, SchemaError> {
    if !fragment.contains('%') {
        return Ok(std::borrow::Cow::Borrowed(fragment));
    }
    let mut bytes = Vec::with_capacity(fragment.len());
    let mut source = fragment.bytes();
    while let Some(byte) = source.next() {
        if byte == b'%' {
            let high = source.next().and_then(|b| (b as char).to_digit(16));
            let low = source.next().and_then(|b| (b as char).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(SchemaError::InvalidPointer(fragment.into()));
            };
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes)
        .map(std::borrow::Cow::Owned)
        .map_err(|_| SchemaError::InvalidPointer(fragment.into()))
}
