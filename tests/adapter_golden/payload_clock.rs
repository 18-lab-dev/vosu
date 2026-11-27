//! Exact live-clock scopes from GOLDEN_CONTRACT §7, mirrored by parity_volatile.py.
use serde_json::Value;

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
