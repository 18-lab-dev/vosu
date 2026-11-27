//! Core zero-Serde CBOR parsers shared by owned DTO decoding and direct wire encoding.
use crate::{
    Error,
    dto::*,
    source::Statistics,
    view::{as_array, as_f64, as_i64, as_map, as_opt_f64, as_opt_i64, as_opt_str, as_str, missing},
};
use ciborium::Value;
use vosu_osu_model::source::{HealthRaw, Judgements, TourneyAvailability};

#[derive(Clone, Copy)]
pub(super) struct NumberNameRef<'a> {
    pub number: Option<i64>,
    pub name: &'a str,
}

impl NumberNameRef<'_> {
    pub(super) fn into_owned(self) -> NumberName {
        NumberName {
            number: self.number,
            name: self.name.to_owned(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ScrollDirectionRef<'a> {
    pub number: i64,
    pub name: Option<&'a str>,
}

#[derive(Clone, Copy)]
pub(super) struct BeatmapStatusRef<'a> {
    pub number: Option<f64>,
    pub name: &'a str,
}

#[derive(Clone, Copy, Default)]
pub(super) struct CurrentPathsRef<'a> {
    pub beatmap_folder: Option<&'a str>,
    pub beatmap_file: Option<&'a str>,
    pub background: Option<&'a str>,
    pub audio: Option<&'a str>,
}

#[derive(Clone, Copy)]
pub(super) struct PathsRef<'a> {
    pub game: &'a str,
    pub songs: &'a str,
    pub skin: &'a str,
    pub current: CurrentPathsRef<'a>,
}

pub(super) fn parse_tourney_availability(
    v: &Value,
    ctx: &str,
) -> Result<TourneyAvailability, Error> {
    match as_str(v, ctx)? {
        "pending" => Ok(TourneyAvailability::Pending),
        "ready" => Ok(TourneyAvailability::Ready),
        "missingScore" => Ok(TourneyAvailability::MissingScore),
        other => Err(Error::InvalidSource(format!(
            "{ctx}: unknown tourneyAvailability `{other}`"
        ))),
    }
}

pub(super) fn parse_play_clock_paused(v: &Value, ctx: &str) -> Result<bool, Error> {
    let mut previous = None;
    let mut current = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("previous") => previous = Some(as_f64(val, ctx)?),
            Some("current") => current = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    let previous = previous.ok_or_else(|| missing(ctx, "previous"))?;
    let current = current.ok_or_else(|| missing(ctx, "current"))?;
    Ok(previous == current)
}

#[inline]
pub(super) fn parse_number_name<'a>(v: &'a Value, ctx: &str) -> Result<NumberNameRef<'a>, Error> {
    let mut number = None;
    let mut name = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("number") => number = as_opt_i64(val, ctx)?,
            Some("name") => name = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    Ok(NumberNameRef {
        number,
        name: name.ok_or_else(|| missing(ctx, "name"))?,
    })
}

#[inline]
pub(super) fn parse_opt_number_name<'a>(
    v: &'a Value,
    ctx: &str,
) -> Result<Option<NumberNameRef<'a>>, Error> {
    if matches!(v, Value::Null) {
        Ok(None)
    } else {
        parse_number_name(v, ctx).map(Some)
    }
}

pub(super) fn parse_scroll_direction<'a>(
    v: &'a Value,
    ctx: &str,
) -> Result<ScrollDirectionRef<'a>, Error> {
    let mut number = None;
    let mut name = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("number") => number = Some(as_i64(val, ctx)?),
            Some("name") => name = as_opt_str(val, ctx)?,
            _ => {}
        }
    }
    Ok(ScrollDirectionRef {
        number: number.ok_or_else(|| missing(ctx, "number"))?,
        name,
    })
}

pub(super) fn parse_health_raw(v: &Value, ctx: &str) -> Result<HealthRaw, Error> {
    let mut normal = None;
    let mut smooth = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("normal") => normal = Some(as_f64(val, ctx)?),
            Some("smooth") => smooth = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(HealthRaw {
        normal: normal.ok_or_else(|| missing(ctx, "normal"))?,
        smooth: smooth.ok_or_else(|| missing(ctx, "smooth"))?,
    })
}

pub(super) fn parse_health_bar(v: &Value, ctx: &str) -> Result<HealthBar, Error> {
    let raw = parse_health_raw(v, ctx)?;
    Ok(HealthBar {
        normal: raw.normal,
        smooth: raw.smooth,
    })
}

pub(super) fn parse_judgements(v: &Value, ctx: &str) -> Result<Judgements, Error> {
    let mut miss = None;
    let mut meh = None;
    let mut ok = None;
    let mut good = None;
    let mut great = None;
    let mut perfect = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("miss") => miss = Some(as_i64(val, ctx)?),
            Some("meh") => meh = Some(as_i64(val, ctx)?),
            Some("ok") => ok = Some(as_i64(val, ctx)?),
            Some("good") => good = Some(as_i64(val, ctx)?),
            Some("great") => great = Some(as_i64(val, ctx)?),
            Some("perfect") => perfect = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Judgements {
        miss: miss.ok_or_else(|| missing(ctx, "miss"))?,
        meh: meh.ok_or_else(|| missing(ctx, "meh"))?,
        ok: ok.ok_or_else(|| missing(ctx, "ok"))?,
        good: good.ok_or_else(|| missing(ctx, "good"))?,
        great: great.ok_or_else(|| missing(ctx, "great"))?,
        perfect: perfect.ok_or_else(|| missing(ctx, "perfect"))?,
    })
}

pub(super) fn parse_statistics(v: &Value, ctx: &str) -> Result<Statistics, Error> {
    let mut slider_breaks = None;
    let mut slider_end_hits = None;
    let mut small_tick_hit = None;
    let mut large_tick_hit = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("sliderBreaks") => slider_breaks = Some(as_i64(val, ctx)?),
            Some("sliderEndHits") => slider_end_hits = Some(as_i64(val, ctx)?),
            Some("smallTickHit") => small_tick_hit = Some(as_i64(val, ctx)?),
            Some("largeTickHit") => large_tick_hit = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Statistics {
        slider_breaks: slider_breaks.ok_or_else(|| missing(ctx, "sliderBreaks"))?,
        slider_end_hits: slider_end_hits.ok_or_else(|| missing(ctx, "sliderEndHits"))?,
        small_tick_hit: small_tick_hit.ok_or_else(|| missing(ctx, "smallTickHit"))?,
        large_tick_hit: large_tick_hit.ok_or_else(|| missing(ctx, "largeTickHit"))?,
    })
}

pub(super) fn parse_combo(v: &Value, ctx: &str) -> Result<Combo, Error> {
    let mut current = None;
    let mut max = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("current") => current = Some(as_i64(val, ctx)?),
            Some("max") => max = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Combo {
        current: current.ok_or_else(|| missing(ctx, "current"))?,
        max: max.ok_or_else(|| missing(ctx, "max"))?,
    })
}

pub(super) fn parse_detailed_pp_attrs(v: &Value, ctx: &str) -> Result<DetailedPpAttributes, Error> {
    let mut aim = None;
    let mut speed = None;
    let mut accuracy = None;
    let mut difficulty = None;
    let mut flashlight = None;
    let mut total = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("aim") => aim = Some(as_f64(val, ctx)?),
            Some("speed") => speed = Some(as_f64(val, ctx)?),
            Some("accuracy") => accuracy = Some(as_f64(val, ctx)?),
            Some("difficulty") => difficulty = Some(as_f64(val, ctx)?),
            Some("flashlight") => flashlight = Some(as_f64(val, ctx)?),
            Some("total") => total = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(DetailedPpAttributes {
        aim: aim.ok_or_else(|| missing(ctx, "aim"))?,
        speed: speed.ok_or_else(|| missing(ctx, "speed"))?,
        accuracy: accuracy.ok_or_else(|| missing(ctx, "accuracy"))?,
        difficulty: difficulty.ok_or_else(|| missing(ctx, "difficulty"))?,
        flashlight: flashlight.ok_or_else(|| missing(ctx, "flashlight"))?,
        total: total.ok_or_else(|| missing(ctx, "total"))?,
    })
}

pub(super) fn parse_pp(v: &Value, ctx: &str) -> Result<Pp, Error> {
    let mut current = None;
    let mut fc = None;
    let mut max_achieved = None;
    let mut max_achievable = None;
    let mut detailed = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("current") => current = Some(as_f64(val, ctx)?),
            Some("fc") => fc = Some(as_f64(val, ctx)?),
            Some("maxAchieved") => max_achieved = Some(as_f64(val, ctx)?),
            Some("maxAchievable") => max_achievable = Some(as_f64(val, ctx)?),
            Some("detailed") => {
                let mut d_cur = None;
                let mut d_fc = None;
                for (dk, dv) in as_map(val, ctx)? {
                    match dk.as_text() {
                        Some("current") => d_cur = Some(parse_detailed_pp_attrs(dv, ctx)?),
                        Some("fc") => d_fc = Some(parse_detailed_pp_attrs(dv, ctx)?),
                        _ => {}
                    }
                }
                detailed = Some(DetailedPp {
                    current: d_cur.ok_or_else(|| missing(ctx, "detailed.current"))?,
                    fc: d_fc.ok_or_else(|| missing(ctx, "detailed.fc"))?,
                });
            }
            _ => {}
        }
    }
    Ok(Pp {
        current: current.ok_or_else(|| missing(ctx, "current"))?,
        fc: fc.ok_or_else(|| missing(ctx, "fc"))?,
        max_achieved: max_achieved.ok_or_else(|| missing(ctx, "maxAchieved"))?,
        max_achievable: max_achievable.ok_or_else(|| missing(ctx, "maxAchievable"))?,
        detailed: detailed.ok_or_else(|| missing(ctx, "detailed"))?,
    })
}

pub(super) fn parse_beatmap_time(v: &Value, ctx: &str) -> Result<BeatmapTime, Error> {
    let mut live = None;
    let mut first_object = None;
    let mut last_object = None;
    let mut mp3_length = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("live") => live = Some(as_f64(val, ctx)?),
            Some("firstObject") => first_object = Some(as_i64(val, ctx)?),
            Some("lastObject") => last_object = Some(as_f64(val, ctx)?),
            Some("mp3Length") => mp3_length = as_opt_f64(val, ctx)?,
            _ => {}
        }
    }
    Ok(BeatmapTime {
        live: live.ok_or_else(|| missing(ctx, "live"))?,
        first_object: first_object.ok_or_else(|| missing(ctx, "firstObject"))?,
        last_object: last_object.ok_or_else(|| missing(ctx, "lastObject"))?,
        mp3_length,
    })
}

pub(super) fn parse_beatmap_status<'a>(
    v: &'a Value,
    ctx: &str,
) -> Result<BeatmapStatusRef<'a>, Error> {
    let mut number = None;
    let mut name = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("number") => number = as_opt_f64(val, ctx)?,
            Some("name") => name = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    Ok(BeatmapStatusRef {
        number,
        name: name.ok_or_else(|| missing(ctx, "name"))?,
    })
}

fn parse_diff_pair(v: &Value, ctx: &str) -> Result<(f64, f64), Error> {
    let mut original = None;
    let mut converted = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("original") => original = Some(as_f64(val, ctx)?),
            Some("converted") => converted = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok((
        original.ok_or_else(|| missing(ctx, "original"))?,
        converted.ok_or_else(|| missing(ctx, "converted"))?,
    ))
}

pub(super) fn parse_stats(v: &Value, ctx: &str) -> Result<Stats, Error> {
    let mut stars = None;
    let mut ar = None;
    let mut cs = None;
    let mut od = None;
    let mut hp = None;
    let mut bpm = None;
    let mut objects = None;
    let mut hit_window = None;
    let mut max_combo = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("stars") => stars = Some(parse_stars(val, ctx)?),
            Some("ar") => {
                let (original, converted) = parse_diff_pair(val, ctx)?;
                ar = Some(Ar {
                    original,
                    converted,
                });
            }
            Some("cs") => {
                let (original, converted) = parse_diff_pair(val, ctx)?;
                cs = Some(Cs {
                    original,
                    converted,
                });
            }
            Some("od") => {
                let (original, converted) = parse_diff_pair(val, ctx)?;
                od = Some(Od {
                    original,
                    converted,
                });
            }
            Some("hp") => {
                let (original, converted) = parse_diff_pair(val, ctx)?;
                hp = Some(Hp {
                    original,
                    converted,
                });
            }
            Some("bpm") => bpm = Some(parse_bpm(val, ctx)?),
            Some("objects") => objects = Some(parse_objects(val, ctx)?),
            Some("hitWindow") => hit_window = Some(parse_hit_window(val, ctx)?),
            Some("maxCombo") => max_combo = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Stats {
        stars: stars.ok_or_else(|| missing(ctx, "stars"))?,
        ar: ar.ok_or_else(|| missing(ctx, "ar"))?,
        cs: cs.ok_or_else(|| missing(ctx, "cs"))?,
        od: od.ok_or_else(|| missing(ctx, "od"))?,
        hp: hp.ok_or_else(|| missing(ctx, "hp"))?,
        bpm: bpm.ok_or_else(|| missing(ctx, "bpm"))?,
        objects: objects.ok_or_else(|| missing(ctx, "objects"))?,
        hit_window: hit_window.ok_or_else(|| missing(ctx, "hitWindow"))?,
        max_combo: max_combo.ok_or_else(|| missing(ctx, "maxCombo"))?,
    })
}

fn parse_stars(val: &Value, ctx: &str) -> Result<Stars, Error> {
    let mut live = None;
    let mut aim = None;
    let mut speed = None;
    let mut flashlight = None;
    let mut slider_factor = None;
    let mut stamina = None;
    let mut rhythm = None;
    let mut color = None;
    let mut reading = None;
    let mut hw = None;
    let mut total = None;
    for (sk, sv) in as_map(val, ctx)? {
        match sk.as_text() {
            Some("live") => live = Some(as_f64(sv, ctx)?),
            Some("aim") => aim = as_opt_f64(sv, ctx)?,
            Some("speed") => speed = as_opt_f64(sv, ctx)?,
            Some("flashlight") => flashlight = as_opt_f64(sv, ctx)?,
            Some("sliderFactor") => slider_factor = as_opt_f64(sv, ctx)?,
            Some("stamina") => stamina = as_opt_f64(sv, ctx)?,
            Some("rhythm") => rhythm = as_opt_f64(sv, ctx)?,
            Some("color") => color = as_opt_f64(sv, ctx)?,
            Some("reading") => reading = as_opt_f64(sv, ctx)?,
            Some("hitWindow") => hw = as_opt_f64(sv, ctx)?,
            Some("total") => total = Some(as_f64(sv, ctx)?),
            _ => {}
        }
    }
    Ok(Stars {
        live: live.ok_or_else(|| missing(ctx, "stars.live"))?,
        aim,
        speed,
        flashlight,
        slider_factor,
        stamina,
        rhythm,
        color,
        reading,
        hit_window: hw,
        total: total.ok_or_else(|| missing(ctx, "stars.total"))?,
    })
}

fn parse_bpm(val: &Value, ctx: &str) -> Result<Bpm, Error> {
    let mut realtime = None;
    let mut common = None;
    let mut min = None;
    let mut max = None;
    for (bk, bv) in as_map(val, ctx)? {
        match bk.as_text() {
            Some("realtime") => realtime = Some(as_f64(bv, ctx)?),
            Some("common") => common = Some(as_f64(bv, ctx)?),
            Some("min") => min = Some(as_f64(bv, ctx)?),
            Some("max") => max = Some(as_f64(bv, ctx)?),
            _ => {}
        }
    }
    Ok(Bpm {
        realtime: realtime.ok_or_else(|| missing(ctx, "bpm.realtime"))?,
        common: common.ok_or_else(|| missing(ctx, "bpm.common"))?,
        min: min.ok_or_else(|| missing(ctx, "bpm.min"))?,
        max: max.ok_or_else(|| missing(ctx, "bpm.max"))?,
    })
}

fn parse_objects(val: &Value, ctx: &str) -> Result<Objects, Error> {
    let mut circles = None;
    let mut sliders = None;
    let mut spinners = None;
    let mut holds = None;
    let mut total = None;
    for (ok, ov) in as_map(val, ctx)? {
        match ok.as_text() {
            Some("circles") => circles = Some(as_i64(ov, ctx)?),
            Some("sliders") => sliders = Some(as_i64(ov, ctx)?),
            Some("spinners") => spinners = Some(as_i64(ov, ctx)?),
            Some("holds") => holds = Some(as_i64(ov, ctx)?),
            Some("total") => total = Some(as_i64(ov, ctx)?),
            _ => {}
        }
    }
    Ok(Objects {
        circles: circles.ok_or_else(|| missing(ctx, "objects.circles"))?,
        sliders: sliders.ok_or_else(|| missing(ctx, "objects.sliders"))?,
        spinners: spinners.ok_or_else(|| missing(ctx, "objects.spinners"))?,
        holds: holds.ok_or_else(|| missing(ctx, "objects.holds"))?,
        total: total.ok_or_else(|| missing(ctx, "objects.total"))?,
    })
}

fn parse_hit_window(val: &Value, ctx: &str) -> Result<HitWindow, Error> {
    let mut hw = HitWindow::default();
    for (hk, hv) in as_map(val, ctx)? {
        match hk.as_text() {
            Some("miss") => hw.miss = as_opt_f64(hv, ctx)?,
            Some("meh") => hw.meh = as_opt_f64(hv, ctx)?,
            Some("ok") => hw.ok = as_opt_f64(hv, ctx)?,
            Some("good") => hw.good = as_opt_f64(hv, ctx)?,
            Some("great") => hw.great = as_opt_f64(hv, ctx)?,
            Some("perfect") => hw.perfect = as_opt_f64(hv, ctx)?,
            _ => {}
        }
    }
    Ok(hw)
}

pub(super) fn parse_accuracy(v: &Value, ctx: &str) -> Result<Accuracy, Error> {
    let mut a90 = None;
    let mut a91 = None;
    let mut a92 = None;
    let mut a93 = None;
    let mut a94 = None;
    let mut a95 = None;
    let mut a96 = None;
    let mut a97 = None;
    let mut a98 = None;
    let mut a99 = None;
    let mut a100 = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("90") => a90 = Some(as_f64(val, ctx)?),
            Some("91") => a91 = Some(as_f64(val, ctx)?),
            Some("92") => a92 = Some(as_f64(val, ctx)?),
            Some("93") => a93 = Some(as_f64(val, ctx)?),
            Some("94") => a94 = Some(as_f64(val, ctx)?),
            Some("95") => a95 = Some(as_f64(val, ctx)?),
            Some("96") => a96 = Some(as_f64(val, ctx)?),
            Some("97") => a97 = Some(as_f64(val, ctx)?),
            Some("98") => a98 = Some(as_f64(val, ctx)?),
            Some("99") => a99 = Some(as_f64(val, ctx)?),
            Some("100") => a100 = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Accuracy {
        acc_90: a90.ok_or_else(|| missing(ctx, "90"))?,
        acc_91: a91.ok_or_else(|| missing(ctx, "91"))?,
        acc_92: a92.ok_or_else(|| missing(ctx, "92"))?,
        acc_93: a93.ok_or_else(|| missing(ctx, "93"))?,
        acc_94: a94.ok_or_else(|| missing(ctx, "94"))?,
        acc_95: a95.ok_or_else(|| missing(ctx, "95"))?,
        acc_96: a96.ok_or_else(|| missing(ctx, "96"))?,
        acc_97: a97.ok_or_else(|| missing(ctx, "97"))?,
        acc_98: a98.ok_or_else(|| missing(ctx, "98"))?,
        acc_99: a99.ok_or_else(|| missing(ctx, "99"))?,
        acc_100: a100.ok_or_else(|| missing(ctx, "100"))?,
    })
}

pub(super) fn parse_paths<'a>(v: &'a Value, lazer: bool, ctx: &str) -> Result<PathsRef<'a>, Error> {
    let mut stable: Option<(&'a str, &'a str)> = None;
    let mut lazer_p: Option<(&'a str, &'a str)> = None;
    let mut current = CurrentPathsRef::default();
    let mut skin_folder = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("stable") if !matches!(val, Value::Null) => {
                let mut game = "";
                let mut songs = "";
                for (sk, sv) in as_map(val, ctx)? {
                    match sk.as_text() {
                        Some("game") => game = as_opt_str(sv, ctx)?.unwrap_or(""),
                        Some("songs") => songs = as_opt_str(sv, ctx)?.unwrap_or(""),
                        _ => {}
                    }
                }
                stable = Some((game, songs));
            }
            Some("lazer") if !matches!(val, Value::Null) => {
                let mut data = "";
                let mut files = "";
                for (lk, lv) in as_map(val, ctx)? {
                    match lk.as_text() {
                        Some("data") => data = as_opt_str(lv, ctx)?.unwrap_or(""),
                        Some("files") => files = as_opt_str(lv, ctx)?.unwrap_or(""),
                        _ => {}
                    }
                }
                lazer_p = Some((data, files));
            }
            Some("current") if !matches!(val, Value::Null) => {
                for (ck, cv) in as_map(val, ctx)? {
                    match ck.as_text() {
                        Some("beatmapFolder") => current.beatmap_folder = as_opt_str(cv, ctx)?,
                        Some("beatmapFile") => current.beatmap_file = as_opt_str(cv, ctx)?,
                        Some("background") => current.background = as_opt_str(cv, ctx)?,
                        Some("audio") => current.audio = as_opt_str(cv, ctx)?,
                        _ => {}
                    }
                }
            }
            Some("skinFolder") => skin_folder = as_opt_str(val, ctx)?,
            _ => {}
        }
    }
    let (game, songs) = if lazer {
        lazer_p.ok_or(Error::NotReady)?
    } else {
        stable.ok_or(Error::NotReady)?
    };
    let skin = vosu_adapter_support::skin_folder(lazer, skin_folder);
    Ok(PathsRef {
        game,
        songs,
        skin,
        current,
    })
}

pub(super) fn parse_f64_vec(v: &Value, ctx: &str) -> Result<Vec<f64>, Error> {
    let items = as_array(v, ctx)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(as_f64(item, ctx)?);
    }
    Ok(out)
}
