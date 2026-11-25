use super::*;

impl CompiledActionSchema {
    pub fn compile(bytes: impl AsRef<[u8]>) -> Result<Self, SchemaError> {
        let text = bytes.as_ref();
        if text.len() > MAX_SCHEMA_BYTES {
            return Err(error("", "", "schema byte budget exceeded"));
        }
        let document: Value =
            serde_json::from_slice(text).map_err(|_| error("", "", "invalid schema JSON"))?;
        check_depth(&document, 0)?;
        let mut compiler = Compiler {
            document: &document,
            nodes: 0,
            sources: std::collections::BTreeSet::new(),
        };
        let root = compiler.node(&document, "", &mut Vec::new())?;
        Ok(Self { root })
    }
}

fn check_depth(value: &Value, depth: usize) -> Result<(), SchemaError> {
    if depth > MAX_DEPTH {
        return Err(error("", "", "schema depth exceeded"));
    }
    match value {
        Value::Object(map) => {
            for value in map.values() {
                check_depth(value, depth + 1)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                check_depth(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

struct Compiler<'a> {
    document: &'a Value,
    nodes: usize,
    sources: std::collections::BTreeSet<String>,
}

impl Compiler<'_> {
    fn node(
        &mut self,
        value: &Value,
        path: &str,
        refs: &mut Vec<String>,
    ) -> Result<Node, SchemaError> {
        self.nodes += 1;
        self.sources.insert(path.to_owned());
        if self.nodes > MAX_COMPILED_NODES || self.sources.len() > MAX_SCHEMA_NODES {
            return Err(error(path, "", "schema compilation budget exceeded"));
        }
        let schema = if let Some(boolean) = value.as_bool() {
            Rule::Boolean(boolean)
        } else {
            let map = value
                .as_object()
                .ok_or_else(|| error(path, "", "schema must be boolean or object"))?;
            let mut c = Constraints::default();
            for (key, value) in map {
                let p = pointer(path, key);
                let invalid = || error(&p, "", "invalid keyword value");
                match key.as_str() {
                    "type" => {
                        let types: Vec<&Value> = value
                            .as_array()
                            .map_or_else(|| vec![value], |a| a.iter().collect());
                        if types.is_empty() {
                            return Err(invalid());
                        }
                        for item in types {
                            let name = item.as_str().ok_or_else(invalid)?;
                            if ![
                                "null", "boolean", "object", "array", "number", "integer", "string",
                            ]
                            .contains(&name)
                                || c.types.iter().any(|s| s == name)
                            {
                                return Err(invalid());
                            }
                            c.types.push(name.to_owned());
                        }
                    }
                    "properties" | "$defs" => {
                        for (name, schema) in value.as_object().ok_or_else(invalid)? {
                            let node = self.node(schema, &pointer(&p, name), refs)?;
                            if key == "properties" {
                                c.properties.insert(name.clone(), node);
                            }
                        }
                    }
                    "required" => {
                        let mut names = std::collections::BTreeSet::new();
                        for name in value.as_array().ok_or_else(invalid)? {
                            let name = name.as_str().ok_or_else(invalid)?;
                            if !names.insert(name) {
                                return Err(invalid());
                            }
                            c.required.push(name.to_owned());
                        }
                    }
                    "additionalProperties" => c.additional = Some(self.node(value, &p, refs)?),
                    "items" => c.items = Some(self.node(value, &p, refs)?),
                    "$ref" => {
                        let reference = value.as_str().ok_or_else(invalid)?;
                        let target = fragment(reference).ok_or_else(invalid)?;
                        // Decoded RFC 6901 fragments, rooted inside this document's $defs.
                        let bytes = target.as_bytes();
                        for i in 0..bytes.len() {
                            if bytes[i] == b'~' && !matches!(bytes.get(i + 1), Some(b'0' | b'1')) {
                                return Err(invalid());
                            }
                        }
                        if refs.len() >= MAX_REFERENCE_DEPTH || refs.iter().any(|r| r == &target) {
                            return Err(error(&p, "", "reference cycle or depth exceeded"));
                        }
                        let schema = self.document.pointer(&target).ok_or_else(invalid)?;
                        refs.push(target.clone());
                        c.reference = Some(self.node(schema, &target, refs)?);
                        refs.pop();
                    }
                    "enum" => {
                        let values = value
                            .as_array()
                            .filter(|a| !a.is_empty())
                            .ok_or_else(invalid)?;
                        let mut work = MAX_WORK;
                        for (i, item) in values.iter().enumerate() {
                            for previous in &values[..i] {
                                if super::validate::equal(item, previous, &mut work).ok_or_else(
                                    || error(&p, "", "schema comparison budget exceeded"),
                                )? {
                                    return Err(invalid());
                                }
                            }
                        }
                        c.enumeration = Some(values.clone());
                    }
                    "const" => c.constant = Some(value.clone()),
                    "minItems" | "maxItems" | "minLength" | "maxLength" => {
                        let number = value.as_number().ok_or_else(invalid)?;
                        let n = number.as_f64().ok_or_else(invalid)?;
                        if n < 0.0 || n.fract() != 0.0 {
                            return Err(invalid());
                        }
                        let bound = Some(number.clone());
                        match key.as_str() {
                            "minItems" => c.min_items = bound,
                            "maxItems" => c.max_items = bound,
                            "minLength" => c.min_length = bound,
                            _ => c.max_length = bound,
                        }
                    }
                    "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                        let number = Some(value.as_number().ok_or_else(invalid)?.clone());
                        match key.as_str() {
                            "minimum" => c.minimum = number,
                            "maximum" => c.maximum = number,
                            "exclusiveMinimum" => c.exclusive_minimum = number,
                            _ => c.exclusive_maximum = number,
                        }
                    }
                    "$schema" => {
                        if value.as_str() != Some("https://json-schema.org/draft/2020-12/schema") {
                            return Err(invalid());
                        }
                    }
                    "$id" | "title" | "description" | "$comment" => {
                        if !value.is_string() {
                            return Err(invalid());
                        }
                    }
                    "deprecated" | "readOnly" | "writeOnly" => {
                        if !value.is_boolean() {
                            return Err(invalid());
                        }
                    }
                    "examples" => {
                        if !value.is_array() {
                            return Err(invalid());
                        }
                    }
                    "default" => {}
                    _ => return Err(error(&p, "", "unsupported keyword")),
                }
            }
            Rule::Object(Box::new(c))
        };
        Ok(Node {
            path: path.to_owned(),
            schema,
        })
    }
}

fn fragment(reference: &str) -> Option<String> {
    let mut bytes = reference.strip_prefix('#')?.bytes();
    let mut decoded = Vec::with_capacity(reference.len());
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    let target = String::from_utf8(decoded).ok()?;
    target.starts_with("/$defs/").then_some(target)
}
