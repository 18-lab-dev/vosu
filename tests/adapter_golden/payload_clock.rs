//! Exact live-clock scopes from GOLDEN_CONTRACT §7, mirrored by parity_volatile.py.
use super::fixture::Platform;
use serde_json::Value;
use std::collections::BTreeSet;

/// JSON payload comparison of one route on the replaying host.
#[derive(Clone, Copy)]
pub(super) struct Scope<'a> {
    route: &'a str,
    foreign_windows_focus: bool,
    run5: bool,
}

impl<'a> Scope<'a> {
    pub(super) fn strict(route: &'a str) -> Self {
        Self {
            route,
            foreign_windows_focus: false,
            run5: false,
        }
    }

    pub(super) fn for_case(route: &'a str, platform: &Platform) -> Self {
        Self {
            route,
            foreign_windows_focus: cfg!(target_os = "linux")
                && platform.name.starts_with("windows-"),
            run5: platform.run5,
        }
    }

    fn parsed(self, got: &[u8], want: &[u8]) -> Option<(Value, Value)> {
        let got = serde_json::from_slice(got).ok()?;
        let mut want: Value = serde_json::from_slice(want).ok()?;
        // Upstream initializes gameFocused on Windows only. A Linux host
        // cannot reproduce that Windows projection; on Windows it stays exact.
        if self.foreign_windows_focus
            && matches!(self.route, "/json/v2" | "/websocket/v2")
            && let Some(game) = want.get_mut("game").and_then(Value::as_object_mut)
            && game.get("focused").is_some_and(Value::is_boolean)
        {
            game.remove("focused");
        }
        Some((got, want))
    }

    pub(super) fn same_json(self, got: &[u8], want: &[u8]) -> Option<bool> {
        let (got, want) = self.parsed(got, want)?;
        Some(same_json(
            &got,
            &want,
            &Clocks::for_route(self.route, &want),
        ))
    }

    pub(super) fn describe_json(self, got: &[u8], want: &[u8]) -> Option<String> {
        let (got, want) = self.parsed(got, want)?;
        let clocks = Clocks::for_route(self.route, &want);
        let mut changed = Vec::new();
        difference_paths(
            Some(&got),
            Some(&want),
            &clocks,
            &mut String::new(),
            &mut changed,
        );
        let paths = changed
            .iter()
            .map(|path| match path.as_str() {
                "/menu/mainMenu/bassDensity" | "/settings/bassDensity" if self.run5 => format!(
                    "{path} (pending recapture: unmatched pre-barrier main-menu FFT history)"
                ),
                "/game/paused" if self.run5 && got.pointer(path).is_none() => {
                    format!("{path} (pending recapture: raw play-clock pair was not recorded)")
                }
                "/" => "root value".into(),
                _ => path.clone(),
            })
            .collect::<Vec<_>>();
        Some(format!("JSON differs at {}", paths.join(", ")))
    }
}

fn same_json(got: &Value, want: &Value, clocks: &Clocks<'_>) -> bool {
    if let Some(matches) = clocks.compare(got, want) {
        return matches;
    }
    match (got, want) {
        (Value::Number(a), Value::Number(b)) => {
            const RELATIVE_TOLERANCE: f64 = 1e-12;
            let a = a.as_f64().expect("JSON number fits in binary64");
            let b = b.as_f64().expect("JSON number fits in binary64");
            (a - b).abs() <= RELATIVE_TOLERANCE * a.abs().max(b.abs())
        }
        (Value::Object(got), Value::Object(want)) => {
            got.len() == want.len()
                && got.iter().all(|(key, got)| {
                    want.get(key)
                        .is_some_and(|want| same_json(got, want, clocks))
                })
        }
        (Value::Array(got), Value::Array(want)) => {
            got.len() == want.len() && got.iter().zip(want).all(|(a, b)| same_json(a, b, clocks))
        }
        _ => got == want,
    }
}

// Exact pointers are useful when a legitimate clock skew precedes the real
// mismatch in the raw body. This diagnostic runs on failures only.
fn difference_paths(
    got: Option<&Value>,
    want: Option<&Value>,
    clocks: &Clocks<'_>,
    path: &mut String,
    changed: &mut Vec<String>,
) {
    if changed.len() >= 16 {
        return;
    }
    if let (Some(got), Some(want)) = (got, want)
        && let Some(matches) = clocks.compare(got, want)
    {
        if !matches {
            changed.push(path.clone());
        }
        return;
    }
    let mut compare_child = |key: &str, got, want| {
        let len = path.len();
        path.push('/');
        path.push_str(&key.replace('~', "~0").replace('/', "~1"));
        difference_paths(got, want, clocks, path, changed);
        path.truncate(len);
    };
    match (got, want) {
        (Some(Value::Object(got)), Some(Value::Object(want))) => {
            let keys: BTreeSet<_> = got.keys().chain(want.keys()).collect();
            for key in keys {
                compare_child(key, got.get(key), want.get(key));
            }
        }
        (Some(Value::Array(got)), Some(Value::Array(want))) if got.len() == want.len() => {
            for (index, (got, want)) in got.iter().zip(want).enumerate() {
                compare_child(&index.to_string(), Some(got), Some(want));
            }
        }
        (Some(a), Some(b)) if same_json(a, b, clocks) => {}
        _ => changed.push(if path.is_empty() {
            "/".into()
        } else {
            path.clone()
        }),
    }
}

#[derive(Clone, Copy)]
enum Unit {
    Milliseconds,
    Seconds,
    ScDuration,
}

pub(super) struct Clocks<'a>([Option<(&'a Value, Unit)>; 3]);

impl<'a> Clocks<'a> {
    pub(super) fn for_route(route: &str, expected: &'a Value) -> Self {
        let at = |pointer, unit| expected.pointer(pointer).map(|value| (value, unit));
        Self(match route {
            "/json" | "/ws" => [at("/menu/bm/time/current", Unit::Milliseconds), None, None],
            "/json/v2" | "/websocket/v2" => {
                [at("/beatmap/time/live", Unit::Milliseconds), None, None]
            }
            "/json/sc" | "/tokens" => [
                at("/time", Unit::Seconds),
                at("/mapPosition", Unit::ScDuration),
                at("/timeLeft", Unit::ScDuration),
            ],
            _ => [None; 3],
        })
    }

    pub(super) fn compare(&self, got: &Value, want: &Value) -> Option<bool> {
        self.0.iter().flatten().find_map(|&(field, unit)| {
            std::ptr::eq(field, want).then(|| {
                unit.milliseconds(got)
                    .zip(unit.milliseconds(want))
                    .is_some_and(|(a, b)| (a - b).abs() <= 250.0)
            })
        })
    }
}

impl Unit {
    fn milliseconds(self, value: &Value) -> Option<f64> {
        match self {
            Self::Milliseconds => value.as_f64(),
            Self::Seconds => value.as_f64().map(|value| value * 1000.0),
            Self::ScDuration => sc_duration(value.as_str()?),
        }
    }
}

fn sc_duration(value: &str) -> Option<f64> {
    fn number(value: &str, fraction: bool) -> Option<f64> {
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        let unsigned = value.strip_prefix('-').unwrap_or(value);
        let valid = if let Some((whole, frac)) = unsigned.split_once('.') {
            fraction && digits(whole) && digits(frac)
        } else {
            digits(unsigned)
        };
        if !valid {
            return None;
        }
        value.parse::<f64>().ok().filter(|n| n.is_finite())
    }
    let (hours, rest) = value.split_once(':')?;
    let (minutes, rest) = rest.split_once(':')?;
    let (seconds, ms) = rest.split_once('.')?;
    let (mut hours, mut minutes, mut seconds, ms) = (
        number(hours, false)?,
        number(minutes, false)?,
        number(seconds, false)?,
        number(ms, true)?,
    );
    // SC floors signed JS remainders for h/m/s and prints unpadded remainder ms.
    // For negative values recover truncation on each component with lower digits.
    if hours < 0.0 {
        hours += f64::from(u8::from(minutes != 0.0 || seconds != 0.0 || ms != 0.0));
        minutes += f64::from(u8::from(seconds != 0.0 || ms != 0.0));
        seconds += f64::from(u8::from(ms != 0.0));
        let signed_remainder = |value, limit: f64| value > -limit && value <= 0.0;
        if !(signed_remainder(minutes, 60.0)
            && signed_remainder(seconds, 60.0)
            && signed_remainder(ms, 1000.0))
        {
            return None;
        }
    } else if !((0.0..60.0).contains(&minutes)
        && (0.0..60.0).contains(&seconds)
        && (0.0..1000.0).contains(&ms))
    {
        return None;
    }
    Some(((hours * 60.0 + minutes) * 60.0 + seconds) * 1000.0 + ms)
}

#[test]
fn foreign_host_projects_only_the_windows_focus_field() {
    let scope = Scope {
        route: "/json/v2",
        foreign_windows_focus: true,
        run5: true,
    };
    let want = br#"{"game":{"focused":true,"paused":true},"settings":{"bassDensity":0.6}}"#;
    let same = br#"{"game":{"paused":true},"settings":{"bassDensity":0.6}}"#;
    let missing = br#"{"game":{},"settings":{"bassDensity":0}}"#;
    assert_eq!(scope.same_json(same, want), Some(true));
    assert_eq!(Scope::strict("/json/v2").same_json(same, want), Some(false));
    assert_eq!(
        Scope {
            route: "/json/v2/precise",
            ..scope
        }
        .same_json(same, want),
        Some(false)
    );
    assert_eq!(scope.same_json(missing, want), Some(false));
    let detail = scope.describe_json(missing, want).unwrap();
    assert!(detail.contains(
        "/settings/bassDensity (pending recapture: unmatched pre-barrier main-menu FFT history)"
    ));
    assert!(
        detail.contains("/game/paused (pending recapture: raw play-clock pair was not recorded)")
    );
    assert!(!detail.contains("/game/focused"));
}
