//! Multiplayer lobby and ranked-play snapshots.
use super::settings::emit;
use super::{OsuPointer as P, ProjectionSink};
use vosu_osu_state::StateBundleSnapshot;

pub(super) fn project(snap: &StateBundleSnapshot, sink: &mut dyn ProjectionSink) {
    emit(sink, P::OsuRoom, |c| {
        let Some(r) = &snap.regular.room.info else {
            c.null();
            return;
        };
        c.map(2);
        c.text("roomId");
        c.int(r.room_id);
        c.text("users");
        c.array(r.users.len());
        for u in &r.users {
            c.map(3 + usize::from(u.avatar_url.is_some()));
            c.text("id");
            c.int(u.id);
            c.text("username");
            c.text(&u.username);
            c.text("countryCode");
            c.int(u.country_code);
            if let Some(url) = &u.avatar_url {
                c.text("avatarUrl");
                c.text(url)
            }
        }
    });
    emit(sink, P::OsuRankedPlay, |c| {
        let Some(r) = &snap.regular.ranked_play.info else {
            c.null();
            return;
        };
        c.map(7);
        c.text("stage");
        c.int(i64::from(r.stage.value()));
        c.text("currentRound");
        c.int(r.current_round);
        c.text("starRating");
        c.float(r.star_rating);
        c.text("damageMultiplier");
        c.float(r.damage_multiplier);
        c.text("activeUserId");
        c.int(r.active_user_id);
        c.text("winningUserId");
        c.int(r.winning_user_id);
        c.text("users");
        c.array(r.users.len());
        for u in &r.users {
            c.map(5);
            c.text("id");
            c.int(u.id);
            c.text("rating");
            c.float(u.rating);
            c.text("life");
            c.float(u.life);
            c.text("roundsWon");
            c.int(u.rounds_won);
            c.text("damageMultiplier");
            c.float(u.damage_multiplier)
        }
    });
}
