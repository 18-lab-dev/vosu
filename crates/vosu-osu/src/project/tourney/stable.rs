//! Stable manager/spectator aggregation of accurate public projection values.
use super::super::{OsuPointer as P, ProjectionSink, SessionFacts, settings::Cbor};
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishValueRef;

pub(crate) fn encode(
    manager: Option<&StateBundleSnapshot>,
    clients: &[(f64, &StateBundleSnapshot)],
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut c = Cbor(&mut out);
    let score = manager.map(|snapshot| &snapshot.regular.tourney.scoreboard);
    c.map(9);
    for (name, value) in [
        ("scoreVisible", score.is_some_and(|s| s.score_visible)),
        ("starsVisible", score.is_some_and(|s| s.stars_visible)),
    ] {
        c.text(name);
        c.boolean(value);
    }
    c.text("ipcState");
    c.int(score.map_or(0, |s| s.ipc_state));
    c.text("bestOf");
    c.int(score.map_or(0, |s| s.best_of));
    c.text("team");
    c.map(2);
    c.text("left");
    c.text(score.map_or("", |s| s.first_team_name.as_str()));
    c.text("right");
    c.text(score.map_or("", |s| s.second_team_name.as_str()));
    c.text("points");
    c.map(2);
    c.text("left");
    c.int(score.map_or(0, |s| s.left_stars));
    c.text("right");
    c.int(score.map_or(0, |s| s.right_stars));
    c.text("chat");
    let messages = manager.map_or(&[][..], |s| s.regular.tourney.messages.as_slice());
    c.array(messages.len());
    for message in messages {
        let side = clients.iter().position(|(_, s)| {
            s.regular
                .tourney
                .user
                .as_ref()
                .is_some_and(|u| u.name == message.name)
        });
        let team = side.map_or(
            if message.name == "BanchoBot" {
                "bot"
            } else {
                "unknown"
            },
            |i| team(i, clients.len()),
        );
        c.map(4);
        for (name, value) in [
            ("team", team),
            ("timestamp", message.time.as_str()),
            ("name", message.name.as_str()),
            ("message", message.content.as_str()),
        ] {
            c.text(name);
            c.text(value);
        }
    }
    c.text("totalScore");
    c.map(2);
    c.text("left");
    c.float(score.map_or(0.0, |s| s.first_team_score as f64));
    c.text("right");
    c.float(score.map_or(0.0, |s| s.second_team_score as f64));
    c.text("clients");
    c.array(if manager.is_some() { clients.len() } else { 0 });
    if manager.is_some() {
        for (i, (ipc, snapshot)) in clients.iter().enumerate() {
            client(&mut c, *ipc, snapshot, i, clients.len());
        }
    }
    out
}
fn team(index: usize, len: usize) -> &'static str {
    if (index as f64) < len as f64 / 2.0 {
        "left"
    } else {
        "right"
    }
}
fn client(c: &mut Cbor<'_>, ipc: f64, snapshot: &StateBundleSnapshot, index: usize, len: usize) {
    let facts = SessionFacts {
        client: vosu_osu_model::enums::ClientType::Stable,
        settings: vosu_osu_model::launch::InstanceSettings {
            tournament_spectator: true,
            tournament_ipc_id: Some(ipc),
            devserver_endpoint: None,
        },
        foreground: None,
    };
    c.map(7);
    c.text("ipcId");
    c.float(ipc);
    c.text("team");
    c.text(team(index, len));
    c.text("keysOverlay");
    struct Overlay<'a>(&'a mut Vec<u8>);
    impl ProjectionSink for Overlay<'_> {
        fn put(&mut self, _: P, _: PublishValueRef<'_>) {}
        fn put_cbor(&mut self, at: P, write: &mut dyn FnMut(&mut Vec<u8>)) {
            if at == P::OsuKeysOverlay {
                write(self.0);
            }
        }
    }
    super::super::keys::project(snapshot, &mut Overlay(c.0));
    c.text("settings");
    c.map(1);
    c.text("mania");
    c.map(1);
    c.text("scrollSpeed");
    let speed = snapshot
        .regular
        .settings
        .values
        .get("mania")
        .and_then(|v| v.get("scrollSpeed"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as i64;
    c.int(speed);
    c.text("user");
    c.map(8);
    let user = snapshot.regular.tourney.user.as_ref();
    c.text("id");
    c.int(user.map_or(0, |u| u.id));
    c.text("name");
    c.text(user.map_or("", |u| u.name.as_str()));
    c.text("country");
    c.text(user.map_or("", |u| u.country.as_str()));
    c.text("accuracy");
    c.float(user.map_or(0.0, |u| u.accuracy));
    c.text("rankedScore");
    c.float(user.map_or(0.0, |u| u.ranked_score));
    c.text("playCount");
    c.int(user.map_or(0, |u| u.play_count));
    c.text("globalRank");
    c.int(0);
    c.text("totalPP");
    c.float(user.map_or(0.0, |u| u.pp));
    c.text("beatmap");
    c.map(1);
    super::super::beatmap::project(
        snapshot,
        &facts,
        &mut Fields {
            out: c.0,
            stats: true,
        },
    );
    c.text("play");
    c.map(18);
    super::super::play::project(
        snapshot,
        &facts,
        &mut Fields {
            out: c.0,
            stats: false,
        },
    );
}
struct Fields<'a> {
    out: &'a mut Vec<u8>,
    stats: bool,
}
impl Fields<'_> {
    fn name(&self, at: P) -> Option<&'static str> {
        if self.stats {
            return (at == P::OsuBeatmapStats).then_some("stats");
        }
        Some(match at {
            P::OsuPlayHealthRaw => "healthRaw",
            P::OsuPlayPreciseUnstableRate => "preciseUnstableRate",
            P::OsuPlaySource => "source",
            P::OsuPlayFailed => "failed",
            P::OsuPlayPlayerName => "playerName",
            P::OsuPlayMode => "mode",
            P::OsuPlayScore => "score",
            P::OsuPlayAccuracyPercent => "accuracy",
            P::OsuPlayCombo => "combo",
            P::OsuPlayHealthBar => "healthBar",
            P::OsuPlayJudgements => "judgements",
            P::OsuPlayStatistics => "statistics",
            P::OsuPlayHitErrors => "hitErrors",
            P::OsuPlayMods => "mods",
            P::OsuPlayRank => "rank",
            P::OsuPlayPp => "pp",
            P::OsuPlayUnstableRate => "unstableRate",
            _ => return None,
        })
    }
}
impl ProjectionSink for Fields<'_> {
    fn wants(&self, at: P) -> bool {
        self.name(at).is_some()
    }
    fn put(&mut self, at: P, value: PublishValueRef<'_>) {
        if let Some(name) = self.name(at) {
            Cbor(self.out).text(name);
            super::super::common::scalar(self.out, value);
        }
    }
    fn put_cbor(&mut self, at: P, write: &mut dyn FnMut(&mut Vec<u8>)) {
        if let Some(name) = self.name(at) {
            Cbor(self.out).text(name);
            write(self.out);
            if at == P::OsuPlayJudgements {
                Cbor(self.out).text("rawJudgements");
                write(self.out);
            }
        }
    }
}
