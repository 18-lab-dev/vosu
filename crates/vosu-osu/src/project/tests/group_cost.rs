//! Opt-in diagnostic: no timing assertions and no waiting/ordering primitives.
use super::super::*;
use std::{hint::black_box, time::Instant};
use vosu_plugin_api::PublishValueRef;

// Time each actual production encoder/diff callback in a batch so timer overhead
// is paid once, not once per scalar. Dispatch and input lookup are excluded.
struct TimingSink<'a> {
    diff: DiffSink<'a>,
    nanos: Vec<f64>,
}
const ITERATIONS: u32 = 20_000;
impl ProjectionSink for TimingSink<'_> {
    fn put(&mut self, at: OsuPointer, value: PublishValueRef<'_>) {
        self.diff.put(at, value);
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            self.diff.put(black_box(at), black_box(value));
        }
        self.nanos[at as usize] += start.elapsed().as_nanos() as f64 / f64::from(ITERATIONS);
    }
    fn put_cbor(&mut self, at: OsuPointer, write: &mut dyn FnMut(&mut Vec<u8>)) {
        self.diff.put_cbor(at, write);
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            self.diff.put_cbor(black_box(at), black_box(&mut *write));
        }
        self.nanos[at as usize] += start.elapsed().as_nanos() as f64 / f64::from(ITERATIONS);
    }
}

#[test]
#[ignore = "manual per-group compare-versus-encode diagnostic; not a timing gate"]
fn projection_group_costs() {
    let a = o5_tests::sample();
    let b = StateBundleSnapshot {
        regular: Arc::new((*a.regular).clone()),
        ..a.clone()
    };
    let f = o5_tests::facts(ClientType::Lazer);
    let mut last = LastValues::default();
    let mut writer = PublishBatchWriter::new();
    writer.begin(10);
    let mut sink = TimingSink {
        diff: DiffSink::new(&mut last, &mut writer),
        nanos: vec![0.0; OsuPointer::ALL.len()],
    };
    Projector::default().project(&a, &f, &mut Caches::default(), &mut sink);
    println!(
        "source | compare ns | skippable encode+diff ns (default snapshots; result always dirty)"
    );
    macro_rules! measure {
        ($field:ident, $bit:ident) => {{
            let start = Instant::now();
            for _ in 0..ITERATIONS {
                black_box(black_box(&a.regular.$field) == black_box(&b.regular.$field));
            }
            let compare = start.elapsed().as_nanos() as f64 / f64::from(ITERATIONS);
            // RESULT is visited on regular ticks regardless of other groups.
            // Do not credit work that its dependency prevents us skipping.
            let blocked = if dirty::$bit == dirty::RESULT {
                0
            } else {
                dirty::RESULT
            };
            let encode: f64 = OsuPointer::ALL
                .iter()
                .filter(|p| {
                    let dependencies = dirty::dependencies(**p);
                    dependencies & dirty::$bit != 0 && dependencies & blocked == 0
                })
                .map(|p| sink.nanos[*p as usize])
                .sum();
            println!("{} | {:.2} | {:.2}", stringify!($field), compare, encode);
        }};
    }
    measure!(global, GLOBAL);
    measure!(settings, SETTINGS);
    measure!(beatmap, BEATMAP);
    measure!(menu, MENU);
    measure!(bass_density, BASS);
    measure!(gameplay, GAMEPLAY);
    measure!(result_screen, RESULT);
    measure!(user, USER);
    measure!(lazer_multi_spectating, TOURNEY);
    measure!(ranked_play, RANKED);
    measure!(room, ROOM);
}
