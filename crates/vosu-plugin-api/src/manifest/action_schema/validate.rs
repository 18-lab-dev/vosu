use super::*;
use std::cmp::Ordering;

// Stack-linked instance pointers allocate only when reporting an error.
enum Segment<'a> {
    Root,
    Key(&'a str),
    Index(usize),
}
struct InstancePath<'a> {
    parent: Option<&'a InstancePath<'a>>,
    segment: Segment<'a>,
}
impl<'a> InstancePath<'a> {
    fn child(&'a self, segment: Segment<'a>) -> Self {
        Self {
            parent: Some(self),
            segment,
        }
    }
    fn append(&self, output: &mut String) {
        use std::fmt::Write as _;
        if let Some(parent) = self.parent {
            parent.append(output);
        }
        match self.segment {
            Segment::Root => {}
            Segment::Key(key) => {
                output.push('/');
                for c in key.chars() {
                    match c {
                        '~' => output.push_str("~0"),
                        '/' => output.push_str("~1"),
                        c => output.push(c),
                    }
                }
            }
            Segment::Index(index) => {
                write!(output, "/{index}").expect("String writes cannot fail");
            }
        }
    }
}
fn error(schema: &str, instance: &InstancePath<'_>, reason: &'static str) -> ValidationError {
    let mut pointer = String::new();
    instance.append(&mut pointer);
    ValidationError {
        schema_pointer: schema.to_owned(),
        instance_pointer: pointer,
        reason,
    }
}

impl CompiledActionSchema {
    pub fn validate_arguments(&self, value: &Value) -> Result<(), ValidationError> {
        self.validate(value)
    }
    pub fn validate_result(&self, value: &Value) -> Result<(), ValidationError> {
        self.validate(value)
    }

    fn validate(&self, value: &Value) -> Result<(), ValidationError> {
        let mut work = MAX_WORK;
        let mut nodes = MAX_VALUE_NODES;
        let path = InstancePath {
            parent: None,
            segment: Segment::Root,
        };
        inspect(value, &path, 0, &mut nodes, &mut work)?;
        evaluate(&self.root, value, &path, &mut work)
    }
}

fn charge(work: &mut usize) -> Option<()> {
    *work = work.checked_sub(1)?;
    Some(())
}

fn inspect(
    value: &Value,
    path: &InstancePath<'_>,
    depth: usize,
    nodes: &mut usize,
    work: &mut usize,
) -> Result<(), ValidationError> {
    if depth > MAX_DEPTH || charge(nodes).is_none() || charge(work).is_none() {
        return Err(error("", path, "value budget exceeded"));
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                inspect(
                    child,
                    &path.child(Segment::Key(key)),
                    depth + 1,
                    nodes,
                    work,
                )?;
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                inspect(
                    child,
                    &path.child(Segment::Index(i)),
                    depth + 1,
                    nodes,
                    work,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn integer(n: &serde_json::Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}

pub(super) fn compare(a: &serde_json::Number, b: &serde_json::Number) -> Ordering {
    fn float_integer(f: f64, i: i128) -> Ordering {
        if f >= 18_446_744_073_709_551_616.0 {
            return Ordering::Greater;
        }
        if f < -9_223_372_036_854_775_808.0 {
            return Ordering::Less;
        }
        let whole = f as i128;
        whole
            .cmp(&i)
            .then_with(|| f.partial_cmp(&(whole as f64)).unwrap())
    }
    match (integer(a), integer(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(a), None) => float_integer(b.as_f64().unwrap(), a).reverse(),
        (None, Some(b)) => float_integer(a.as_f64().unwrap(), b),
        (None, None) => a
            .as_f64()
            .unwrap()
            .partial_cmp(&b.as_f64().unwrap())
            .unwrap(),
    }
}

pub(super) fn equal(a: &Value, b: &Value, work: &mut usize) -> Option<bool> {
    charge(work)?;
    Some(match (a, b) {
        (Value::Number(a), Value::Number(b)) => compare(a, b) == Ordering::Equal,
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(false);
            }
            for (a, b) in a.iter().zip(b) {
                if !equal(a, b, work)? {
                    return Some(false);
                }
            }
            true
        }
        (Value::Object(a), Value::Object(b)) => {
            if a.len() != b.len() {
                return Some(false);
            }
            for (key, a) in a {
                if let Some(b) = b.get(key) {
                    if !equal(a, b, work)? {
                        return Some(false);
                    }
                } else {
                    return Some(false);
                }
            }
            true
        }
        (Value::String(a), Value::String(b)) => {
            let mut a = a.chars();
            let mut b = b.chars();
            loop {
                charge(work)?;
                match (a.next(), b.next()) {
                    (None, None) => break true,
                    (Some(a), Some(b)) if a == b => {}
                    _ => break false,
                }
            }
        }
        _ => a == b,
    })
}

fn evaluate(
    node: &Node,
    value: &Value,
    path: &InstancePath<'_>,
    work: &mut usize,
) -> Result<(), ValidationError> {
    let fail = |reason| error(&node.path, path, reason);
    charge(work).ok_or_else(|| fail("validation work exceeded"))?;
    let c = match &node.schema {
        Rule::Boolean(true) => return Ok(()),
        Rule::Boolean(false) => return Err(fail("false schema")),
        Rule::Object(c) => c,
    };
    if !c.types.is_empty()
        && !c.types.iter().any(|t| match t.as_str() {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.as_number().is_some_and(|n| {
                integer(n).is_some() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
            }),
            _ => false,
        })
    {
        return Err(fail("type mismatch"));
    }
    if let Some(reference) = &c.reference {
        evaluate(reference, value, path, work)?;
    }
    if let Some(constant) = &c.constant
        && !equal(value, constant, work).ok_or_else(|| fail("validation work exceeded"))?
    {
        return Err(fail("const mismatch"));
    }
    if let Some(values) = &c.enumeration {
        let mut found = false;
        for candidate in values {
            if equal(value, candidate, work).ok_or_else(|| fail("validation work exceeded"))? {
                found = true;
                break;
            }
        }
        if !found {
            return Err(fail("enum mismatch"));
        }
    }
    if let Some(map) = value.as_object() {
        for key in &c.required {
            charge(work).ok_or_else(|| fail("validation work exceeded"))?;
            if !map.contains_key(key) {
                return Err(error(
                    &pointer(&node.path, "required"),
                    &path.child(Segment::Key(key)),
                    "required property missing",
                ));
            }
        }
        for (key, value) in map {
            charge(work).ok_or_else(|| fail("validation work exceeded"))?;
            if let Some(schema) = c.properties.get(key) {
                evaluate(schema, value, &path.child(Segment::Key(key)), work)?;
            } else if let Some(schema) = &c.additional {
                evaluate(schema, value, &path.child(Segment::Key(key)), work)?;
            }
        }
    }
    if let Some(items) = value.as_array() {
        let length = serde_json::Number::from(items.len());
        if c.min_items
            .as_ref()
            .is_some_and(|n| compare(&length, n).is_lt())
            || c.max_items
                .as_ref()
                .is_some_and(|n| compare(&length, n).is_gt())
        {
            return Err(fail("item count out of bounds"));
        }
        if let Some(schema) = &c.items {
            for (i, value) in items.iter().enumerate() {
                charge(work).ok_or_else(|| fail("validation work exceeded"))?;
                evaluate(schema, value, &path.child(Segment::Index(i)), work)?;
            }
        }
    }
    if let Some(text) = value.as_str()
        && (c.min_length.is_some() || c.max_length.is_some())
    {
        let mut length = 0u64;
        for _ in text.chars() {
            charge(work).ok_or_else(|| fail("validation work exceeded"))?;
            length += 1;
        }
        let length = serde_json::Number::from(length);
        if c.min_length
            .as_ref()
            .is_some_and(|n| compare(&length, n).is_lt())
            || c.max_length
                .as_ref()
                .is_some_and(|n| compare(&length, n).is_gt())
        {
            return Err(fail("string length out of bounds"));
        }
    }
    if let Some(n) = value.as_number()
        && (c.minimum.as_ref().is_some_and(|b| compare(n, b).is_lt())
            || c.maximum.as_ref().is_some_and(|b| compare(n, b).is_gt())
            || c.exclusive_minimum
                .as_ref()
                .is_some_and(|b| !compare(n, b).is_gt())
            || c.exclusive_maximum
                .as_ref()
                .is_some_and(|b| !compare(n, b).is_lt()))
    {
        return Err(fail("number out of bounds"));
    }
    Ok(())
}
