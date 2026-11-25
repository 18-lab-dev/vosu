//! Settings parity with the 1.x v2 builder; encoding borrows capture values.
use super::{OsuPointer as P, ProjectionSink, SessionFacts};
use serde_json::Value;
use vosu_osu_model::{enums::ClientType, names::*};
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishValueRef as V;

pub(super) struct Cbor<'a>(pub &'a mut Vec<u8>);
impl Cbor<'_> {
    fn head(&mut self, major: u8, n: u64) {
        if n < 24 {
            self.0.push(major | n as u8);
        } else if n <= 255 {
            self.0.extend_from_slice(&[major | 24, n as u8]);
        } else if n <= 65535 {
            self.0.push(major | 25);
            self.0.extend_from_slice(&(n as u16).to_be_bytes());
        } else if n <= u32::MAX as u64 {
            self.0.push(major | 26);
            self.0.extend_from_slice(&(n as u32).to_be_bytes());
        } else {
            self.0.push(major | 27);
            self.0.extend_from_slice(&n.to_be_bytes());
        }
    }
    pub fn map(&mut self, n: usize) {
        self.head(0xa0, n as u64);
    }
    pub fn array(&mut self, n: usize) {
        self.head(0x80, n as u64);
    }
    pub fn text(&mut self, s: &str) {
        self.head(0x60, s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
    pub fn text_parts(&mut self, a: &str, b: &str, c: &str, len: usize) {
        self.head(0x60, len as u64);
        self.0.extend_from_slice(a.as_bytes());
        self.0.extend_from_slice(b.as_bytes());
        self.0.extend_from_slice(c.as_bytes());
    }
    pub fn null(&mut self) {
        self.0.push(0xf6);
    }
    pub fn boolean(&mut self, b: bool) {
        self.0.push(if b { 0xf5 } else { 0xf4 });
    }
    pub fn int(&mut self, n: i64) {
        if n >= 0 {
            self.head(0, n as u64)
        } else {
            self.head(0x20, n.unsigned_abs() - 1)
        }
    }
    pub fn float(&mut self, n: f64) {
        self.0.push(0xfb);
        self.0.extend_from_slice(&n.to_bits().to_be_bytes());
    }
    pub fn optional_text(&mut self, s: Option<&str>) {
        if let Some(s) = s {
            self.text(s)
        } else {
            self.null()
        }
    }
    pub fn number_name(&mut self, n: Option<i64>, name: &str) {
        self.map(2);
        self.text("number");
        if let Some(n) = n {
            self.int(n)
        } else {
            self.null()
        };
        self.text("name");
        self.text(name);
    }
    pub fn json(&mut self, v: &Value) {
        match v {
            Value::Null => self.null(),
            Value::Bool(b) => self.boolean(*b),
            Value::Number(n) => {
                if let Some(n) = n.as_i64() {
                    self.int(n)
                } else {
                    self.float(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => self.text(s),
            Value::Array(a) => {
                self.array(a.len());
                for v in a {
                    self.json(v)
                }
            }
            Value::Object(o) => {
                self.map(o.len());
                for (k, v) in o {
                    self.text(k);
                    self.json(v)
                }
            }
        }
    }
}
pub(super) fn emit(sink: &mut dyn ProjectionSink, at: P, mut f: impl FnMut(&mut Cbor<'_>)) {
    sink.put_cbor(at, &mut |out| f(&mut Cbor(out)));
}
fn get<'a>(s: &'a vosu_osu_model::states::SettingsObject, p: &[&str]) -> Option<&'a Value> {
    let mut v = s.get(*p.first()?)?;
    for k in &p[1..] {
        v = v.get(*k)?
    }
    Some(v)
}
fn number(s: &vosu_osu_model::states::SettingsObject, p: &[&str]) -> i64 {
    get(s, p)
        .and_then(Value::as_i64)
        .or_else(|| get(s, p).and_then(Value::as_f64).map(|n| n as i64))
        .unwrap_or(0)
}
pub(super) fn project_all(
    snap: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    project(snap, facts, sink);
    super::profile::project(snap, sink);
    super::room::project(snap, sink);
    super::internal::project(snap, facts, sink);
}

pub(super) fn project(
    snap: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    if ![
        P::OsuSettingsInterfaceVisible,
        P::OsuSettingsResolution,
        P::OsuSettingsMode,
        P::OsuSettingsLeaderboard,
        P::OsuSettingsBassDensity,
    ]
    .into_iter()
    .any(|at| sink.wants(at))
    {
        return;
    }
    let r = &snap.regular;
    let s = &r.settings.values;
    let lazer = facts.client == ClientType::Lazer;
    sink.put(
        P::OsuSettingsInterfaceVisible,
        V::Bool(r.global.show_interface),
    );
    sink.put(
        P::OsuSettingsReplayUIVisible,
        V::Bool(!r.global.is_replay_ui_hidden),
    );
    sink.put(P::OsuSettingsBassDensity, V::F64(r.bass_density.density));
    for (at, n, name) in [
        (
            P::OsuSettingsChatVisibilityStatus,
            Some(r.global.chat_status),
            chat_status_name(r.global.chat_status),
        ),
        (
            P::OsuSettingsProgressBar,
            Some(number(s, &["progressBarType"])),
            progress_bar_name(number(s, &["progressBarType"])),
        ),
        (
            P::OsuSettingsSort,
            Some(number(s, &["sortType"])),
            if lazer {
                lazer_sort_name(number(s, &["sortType"]))
            } else {
                sort_name(number(s, &["sortType"]))
            },
        ),
        (
            P::OsuSettingsGroup,
            Some(number(s, &["groupType"])),
            if lazer {
                lazer_group_name(number(s, &["groupType"]))
            } else {
                group_name(number(s, &["groupType"]))
            },
        ),
        (
            P::OsuSettingsMode,
            r.menu.gamemode,
            r.menu.gamemode.map_or("", ruleset_name),
        ),
    ] {
        emit(sink, at, |c| c.number_name(n, name));
    }
    emit(sink, P::OsuSettingsLeaderboard, |c| {
        c.map(2);
        c.text("visible");
        c.boolean(r.gameplay.leaderboard_visible);
        c.text("type");
        let n = number(s, &["leaderboardType"]);
        c.number_name(
            Some(n),
            if lazer {
                lazer_leaderboard_name(n)
            } else {
                leaderboard_name(n)
            },
        );
    });
    // Field kinds: b=bool, i=integer, f=number, s=string. Missing settings use the 1.x defaults.
    for (at, group, fields) in [
        (
            P::OsuSettingsResolution,
            "resolution",
            &[
                ("fullscreen", 'b'),
                ("width", 'i'),
                ("height", 'i'),
                ("widthFullscreen", 'i'),
                ("heightFullscreen", 'i'),
            ][..],
        ),
        (
            P::OsuSettingsClient,
            "client",
            &[("updateAvailable", 'b'), ("branch", 'i'), ("version", 's')],
        ),
        (
            P::OsuSettingsCursor,
            "cursor",
            &[
                ("useSkinCursor", 'b'),
                ("autoSize", 'b'),
                ("size", 'f'),
                ("menuSize", 'f'),
            ],
        ),
        (
            P::OsuSettingsMouse,
            "mouse",
            &[
                ("rawInput", 'b'),
                ("disableButtons", 'b'),
                ("disableWheel", 'b'),
                ("sensitivity", 'f'),
            ],
        ),
        (
            P::OsuSettingsTablet,
            "tablet",
            &[
                ("enabled", 'b'),
                ("x", 'f'),
                ("y", 'f'),
                ("width", 'f'),
                ("height", 'f'),
                ("rotation", 'f'),
                ("pressureThreshold", 'f'),
            ],
        ),
        (
            P::OsuSettingsBackground,
            "background",
            &[
                ("storyboard", 'b'),
                ("video", 'b'),
                ("blur", 'f'),
                ("dim", 'f'),
            ],
        ),
        (
            P::OsuSettingsSkin,
            "skin",
            &[
                ("useDefaultSkinInEditor", 'b'),
                ("ignoreBeatmapSkins", 'b'),
                ("tintSliderBall", 'b'),
                ("useTaikoSkin", 'b'),
            ],
        ),
    ] {
        emit(sink, at, |c| {
            c.map(fields.len());
            for &(key, kind) in fields {
                c.text(key);
                field(c, s, &[group, key], kind);
            }
        });
    }
    emit(sink, P::OsuSettingsScoreMeter, |c| {
        c.map(2);
        c.text("type");
        let n = number(s, &["scoreMeter", "type"]);
        c.number_name(Some(n), score_meter_name(n));
        c.text("size");
        field(c, s, &["scoreMeter", "size"], 'f');
    });
    emit(sink, P::OsuSettingsMania, |c| {
        let speed = lazer
            .then(|| get(s, &["mania", "scrollSpeed"]).and_then(Value::as_f64))
            .flatten()
            .filter(|n| n.is_finite() && *n > 0.0);
        let timing = get(s, &["mania", "timingBasedNoteColouring"]).and_then(Value::as_bool);
        let per = get(s, &["mania", "perBeatmapScrollSpeed"]).and_then(Value::as_f64);
        c.map(
            4 + usize::from(speed.is_some()) * 2
                + usize::from(timing.is_some())
                + usize::from(per.is_some()),
        );
        for key in ["speedBPMScale", "usePerBeatmapSpeedScale"] {
            c.text(key);
            field(c, s, &["mania", key], 'b');
        }
        c.text("scrollSpeed");
        c.int(number(s, &["mania", "scrollSpeed"]));
        c.text("scrollDirection");
        let n = number(s, &["mania", "scrollDirection"]);
        let name = mania_scroll_name(n);
        c.map(if name.is_empty() { 1 } else { 2 });
        c.text("number");
        c.int(n);
        if !name.is_empty() {
            c.text("name");
            c.text(name);
        }
        if let Some(n) = speed {
            c.text("scrollSpeedValue");
            c.float(n);
            c.text("scrollTime");
            c.float(11485.0 / n)
        }
        if let Some(b) = timing {
            c.text("timingBasedNoteColouring");
            c.boolean(b)
        }
        if let Some(n) = per {
            c.text("perBeatmapScrollSpeed");
            c.float(n)
        }
    });
    emit(sink, P::OsuSettingsAudio, |c| {
        c.map(4);
        for key in ["ignoreBeatmapSounds", "useSkinSamples"] {
            c.text(key);
            field(c, s, &["audio", key], 'b')
        }
        c.text("volume");
        c.map(4);
        for key in ["masterInactive", "master", "music", "effect"] {
            c.text(key);
            field(c, s, &["audio", "volume", key], 'f')
        }
        c.text("offset");
        c.map(1);
        c.text("universal");
        field(c, s, &["audio", "offset", "universal"], 'f');
    });
    emit(sink, P::OsuSettingsKeybinds, |c| {
        c.map(4);
        for (group, keys) in [
            ("osu", &["k1", "k2", "smokeKey"][..]),
            ("fruits", &["k1", "k2", "Dash"]),
            (
                "taiko",
                &["innerLeft", "innerRight", "outerLeft", "outerRight"],
            ),
        ] {
            c.text(group);
            c.map(keys.len());
            for key in keys {
                c.text(key);
                field(c, s, &["keybinds", group, key], 's')
            }
        }
        c.text("quickRetry");
        field(c, s, &["keybinds", "quickRetry"], 's');
    });
    emit(sink, P::OsuSettingsVisual, |c| {
        let Some(o) = get(s, &["visual"])
            .and_then(Value::as_object)
            .filter(|o| !o.values().all(Value::is_null))
        else {
            c.null();
            return;
        };
        c.map(12);
        for (key, kind) in [
            ("snakingInSliders", 'b'),
            ("snakingOutSliders", 'b'),
            ("hitLighting", 'b'),
            ("beatmapColours", 'b'),
            ("comboColourNormalisation", 'f'),
            ("hitAnimations", 'b'),
            ("rateAdjustedHitAnimation", 'b'),
            ("scalingMode", 'i'),
            ("positionX", 'f'),
            ("positionY", 'f'),
            ("sizeX", 'f'),
            ("sizeY", 'f'),
        ] {
            c.text(key);
            let v = o.get(key);
            match kind {
                'b' => {
                    if let Some(v) = v.and_then(Value::as_bool) {
                        c.boolean(v)
                    } else {
                        c.null()
                    }
                }
                'i' => {
                    if let Some(v) = v.and_then(Value::as_i64) {
                        c.int(v)
                    } else {
                        c.null()
                    }
                }
                _ => {
                    if let Some(v) = v.and_then(Value::as_f64) {
                        c.float(v)
                    } else {
                        c.null()
                    }
                }
            }
        }
    });
}
fn field(c: &mut Cbor<'_>, s: &vosu_osu_model::states::SettingsObject, p: &[&str], kind: char) {
    let v = get(s, p);
    match kind {
        'b' => c.boolean(v.and_then(Value::as_bool).unwrap_or(false)),
        'i' => c.int(number(s, p)),
        's' => c.text(v.and_then(Value::as_str).unwrap_or("")),
        _ => c.float(if v.is_some_and(Value::is_null) {
            f64::NAN
        } else {
            v.and_then(Value::as_f64).unwrap_or(0.0)
        }),
    }
}
