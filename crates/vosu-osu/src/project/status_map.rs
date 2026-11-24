//! Coarse common status, independent of the client's raw enum spelling.
pub(super) fn status(raw: i64, paused: bool) -> Option<&'static str> {
    match raw {
        0 | 1 | 3 | 8 | 9 | 16 | 19..=23 => Some("menu"),
        4..=6 | 11..=13 | 15 => Some("selection"),
        2 => Some(if paused { "pause" } else { "play" }),
        7 | 14 | 17 | 18 => Some("results"),
        _ => None,
    }
}
