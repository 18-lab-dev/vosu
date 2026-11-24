//! Bancho profile projection without cloning capture strings.
use super::settings::emit;
use super::{OsuPointer, ProjectionSink};
use std::fmt::Write;
use vosu_osu_model::names::*;
use vosu_osu_state::StateBundleSnapshot;

pub(super) fn project(snap: &StateBundleSnapshot, sink: &mut dyn ProjectionSink) {
    emit(sink, OsuPointer::OsuProfile, |c| {
        let user = snap.regular.user.profile.as_ref();
        c.map(14);
        for (key, n, name) in [
            (
                "userStatus",
                user.map(|u| u.raw_login_status),
                user.map_or("", |u| user_login_name(u.raw_login_status)),
            ),
            (
                "banchoStatus",
                user.map(|u| u.raw_bancho_status),
                user.map_or("", |u| bancho_name(u.raw_bancho_status)),
            ),
            (
                "mode",
                user.map(|u| u.play_mode),
                user.map_or("", |u| ruleset_name(u.play_mode)),
            ),
            (
                "countryCode",
                user.map(|u| u.country_code),
                user.map_or("", |u| source_country_name(u.country_code)),
            ),
        ] {
            c.text(key);
            c.number_name(n, name);
        }
        for (key, n) in [
            ("id", user.map(|u| u.id)),
            ("playCount", user.map(|u| u.play_count)),
            ("globalRank", user.map(|u| u.rank)),
        ] {
            c.text(key);
            if let Some(n) = n { c.int(n) } else { c.null() }
        }
        c.text("name");
        c.optional_text(user.map(|u| u.name.as_str()));
        for (key, n) in [
            ("rankedScore", user.map(|u| u.ranked_score)),
            ("level", user.map(|u| u.level)),
            ("accuracy", user.map(|u| u.accuracy)),
            ("pp", user.map(|u| u.performance_points)),
        ] {
            c.text(key);
            if let Some(n) = n {
                c.float(n)
            } else {
                c.null()
            }
        }
        c.text("backgroundColour");
        if let Some(u) = user {
            // An i64 hexadecimal rendering needs at most 17 bytes, including its sign.
            struct Hex {
                bytes: [u8; 17],
                len: usize,
            }
            impl Write for Hex {
                fn write_str(&mut self, s: &str) -> std::fmt::Result {
                    self.bytes[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
                    self.len += s.len();
                    Ok(())
                }
            }
            let mut hex = Hex {
                bytes: [0; 17],
                len: 0,
            };
            if u.background_colour < 0 {
                write!(&mut hex, "-{:x}", u.background_colour.unsigned_abs()).unwrap()
            } else {
                write!(&mut hex, "{:x}", u.background_colour).unwrap()
            };
            c.text(std::str::from_utf8(&hex.bytes[..hex.len]).unwrap());
        } else {
            c.null()
        }
        c.text("matchmaking");
        if let Some(m) = user.and_then(|u| u.matchmaking.as_ref()) {
            c.map(5);
            c.text("rating");
            c.float(m.rating);
            c.text("rank");
            if let Some(n) = m.rank {
                c.int(n)
            } else {
                c.null()
            }
            c.text("plays");
            c.int(m.plays);
            c.text("wins");
            c.int(m.wins);
            c.text("isProvisional");
            c.boolean(m.is_provisional)
        } else {
            c.null()
        }
    });
}
