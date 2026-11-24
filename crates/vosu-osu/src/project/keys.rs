//! Latest-only precise histories and logical keys; legacy key aliases are not published.
use super::play::{Encode, array, field, map, text};
use super::{OsuPointer as P, ProjectionSink};
use vosu_osu_state::StateBundleSnapshot;

pub(super) fn project(s: &StateBundleSnapshot, sink: &mut dyn ProjectionSink) {
    let p = &s.precise;
    sink.put_cbor(P::OsuPlayHitErrors, &mut |b| {
        map(b, 3);
        field(b, "all", &p.hit_errors.all);
        field(b, "press", &p.hit_errors.press);
        field(b, "release", &p.hit_errors.release);
    });
    sink.put_cbor(P::OsuPlayHitOffsets, &mut |b| p.hit_offsets().write(b));
    sink.put_cbor(P::OsuPlayKeyHitErrors, &mut |b| {
        map(b, 2);
        for (name, values) in [
            ("press", p.hit_errors.key.as_ref().map(|k| &k.press)),
            ("release", p.hit_errors.key.as_ref().map(|k| &k.release)),
        ] {
            text(b, name);
            if let Some(values) = values {
                values.write(b);
            } else {
                array(b, 0);
            }
        }
    });
    sink.put_cbor(P::OsuPlayCatcher, &mut |b| {
        if let Some(c) = p.active_catcher() {
            map(b, 2);
            field(b, "x", &c.x);
            field(b, "dashing", &c.dashing);
        } else {
            b.push(0xf6);
        }
    });
    sink.put_cbor(P::OsuKeysAll, &mut |b| {
        array(b, p.keypresses.len());
        for key in p.keypresses.iter() {
            map(b, 5);
            field(b, "name", key.name.as_ref());
            field(b, "isPressed", &key.is_pressed);
            field(b, "count", &key.count);
            field(b, "pressedAt", &key.pressed_at);
            field(b, "releasedAt", &key.released_at);
        }
    });
}
