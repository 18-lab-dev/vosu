use serde_json::Value;

use super::{Extraction, SubscriptionRequestError};

/// Validated, nonoverlapping decoded pointers in request-index order.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Selection {
    pointers: Vec<Vec<String>>,
}

impl Selection {
    pub fn compile(pointers: Vec<String>) -> Result<Self, SubscriptionRequestError> {
        if !(1..=256).contains(&pointers.len())
            || pointers.iter().any(|pointer| pointer.len() > 512)
        {
            return Err(SubscriptionRequestError::InvalidRequest);
        }
        let mut compiled = Vec::with_capacity(pointers.len());
        for pointer in pointers {
            let tokens =
                decode_pointer(&pointer).ok_or(SubscriptionRequestError::InvalidPointer)?;
            if tokens.len() > 32 {
                return Err(SubscriptionRequestError::InvalidRequest);
            }
            if compiled.iter().any(|existing: &Vec<String>| {
                existing == &tokens || is_prefix(existing, &tokens) || is_prefix(&tokens, existing)
            }) {
                return Err(SubscriptionRequestError::InvalidPointer);
            }
            compiled.push(tokens);
        }
        Ok(Self { pointers: compiled })
    }

    pub fn pointers(&self) -> &[Vec<String>] {
        &self.pointers
    }
}

pub fn decode_pointer(pointer: &str) -> Option<Vec<String>> {
    if pointer.is_empty() {
        return Some(Vec::new());
    }
    let rest = pointer.strip_prefix('/')?;
    rest.split('/').map(decode_token).collect()
}

fn decode_token(token: &str) -> Option<String> {
    let mut decoded = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        if ch != '~' {
            decoded.push(ch);
            continue;
        }
        match chars.next()? {
            '0' => decoded.push('~'),
            '1' => decoded.push('/'),
            _ => return None,
        }
    }
    Some(decoded)
}

fn is_prefix(prefix: &[String], value: &[String]) -> bool {
    prefix.len() < value.len() && value.starts_with(prefix)
}

/// Objects up to this size are searched by comparing keys, which is cheaper than hashing the
/// token with the map's SipHash; keys are unique, so both find the same entry.
pub(crate) const LINEAR_KEY_SEARCH_MAX: usize = 8;

pub fn traverse<'a>(document: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = document;
    for token in tokens {
        current = match current {
            Value::Object(object) if object.len() <= LINEAR_KEY_SEARCH_MAX => object
                .iter()
                .find_map(|(key, value)| (key == token).then_some(value))?,
            Value::Object(object) => object.get(token)?,
            Value::Array(array) => array.get(parse_array_index(token)?)?,
            _ => return None,
        };
    }
    Some(current)
}

pub(crate) fn parse_array_index(token: &str) -> Option<usize> {
    if token == "0" {
        return Some(0);
    }
    if token.starts_with('0')
        || token.is_empty()
        || !token.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    token.parse().ok()
}

/// Values selected by one plan from one document, in pointer order.
pub fn extract(plan: &Selection, document: Option<&Value>) -> Extraction {
    plan.pointers
        .iter()
        .map(|tokens| document.and_then(|value| traverse(value, tokens)).cloned())
        .collect()
}
