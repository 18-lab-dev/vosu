//! Shared identifier validation for anything routable under a reserved prefix
//! (currently out-of-process tools, dispatched through [`super::routes::RouteExtension`]).

/// `^[a-z0-9][a-z0-9-]{0,31}$`
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && (id.as_bytes()[0].is_ascii_lowercase() || id.as_bytes()[0].is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        for id in ["a", "0", "skill-tracker"] {
            assert!(valid_id(id));
        }
        for id in ["", "_host", "A", "a/b", "abcdefghijklmnopqrstuvwxyz1234567"] {
            assert!(!valid_id(id));
        }
    }
}
