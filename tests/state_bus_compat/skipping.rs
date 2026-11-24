//! Exercise real publisher skipping against an independent cold document.
use super::*;
use std::sync::{Mutex, atomic::AtomicBool};
use vosu_osu::publish::BusPublisher;
use vosu_osu_state::{Delivery, SnapshotPublisher};
use vosu_plugin_api::{HostServices, Message, vosu_opcode, vosu_status};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<Vec<u8>>>>);
impl HostServices for Capture {
    fn send(&self, _: &Message, _: u64) -> vosu_status {
        vosu_status::VOSU_OK
    }
    fn send_flat(&self, _: vosu_opcode, _: u32, _: u64, bytes: &[u8]) -> vosu_status {
        self.0.lock().unwrap().push(bytes.to_vec());
        vosu_status::VOSU_OK
    }
}
fn assert_cold(
    bus: &Bus,
    input: &oracle::Input,
    snapshot: &StateBundleSnapshot,
    f: &SessionFacts,
    label: &str,
) {
    let mut cold = Bus::new();
    cold.open(input, 10);
    cold.publish(snapshot, f);
    let grant = InternalGrant::from_subtrees(vec![Pointer::parse("/internal").unwrap()]);
    equal(
        label,
        "document",
        &cold.document(Audience::Plugin(&grant)),
        &bus.document(Audience::Plugin(&grant)),
    );
}

#[test]
fn isolated_sources_and_mixed_changes_converge_to_cold_projection() {
    let inventory: oracle::Inventory =
        serde_json::from_slice(&fs::read(oracle::fixture_root().join("inventory.json")).unwrap())
            .unwrap();
    for client in ["stable-defaults/input.json", "lazer-menu-hd/input.json"] {
        let input = load_input(client);
        let f = facts(&input);
        // Every source is changed independently, including inconsistent lane
        // combinations. This catches an undeclared cross-lane/group dependency
        // rather than relying on the reducer to change both inputs together.
        for case in &inventory.cases {
            let other = load_input(&case.input);
            if other.source.is_none() {
                continue;
            }
            let target = oracle::state(&other);
            let bus = Bus::new();
            bus.open(&input, 10);
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let publisher = BusPublisher::new(
                Capture(bytes.clone()),
                10,
                Arc::new(AtomicBool::new(true)),
                facts(&input),
            );
            let mut current = (*oracle::state(&input)).clone();
            let publish = |s: &StateBundleSnapshot, label: &str| {
                publisher
                    .publish(Arc::new(s.clone()), Delivery::All)
                    .unwrap();
                for encoded in bytes.lock().unwrap().drain(..) {
                    bus.store
                        .commit(
                            &bus.grant,
                            bus.key,
                            decode_publish_batch_ref(&encoded).unwrap().entries(),
                        )
                        .unwrap();
                }
                assert_cold(&bus, &input, s, &f, label);
            };
            publish(&current, "initial");
            macro_rules! swap {
                ($field:ident) => {{
                    let mut isolated = current.clone();
                    Arc::make_mut(&mut isolated.regular).$field = target.regular.$field.clone();
                    publish(&isolated, stringify!($field));
                    publish(&current, "restore");
                    Arc::make_mut(&mut current.regular).$field = target.regular.$field.clone();
                    publish(&current, concat!("mixed/", stringify!($field)));
                }};
            }
            swap!(global);
            swap!(settings);
            swap!(beatmap);
            swap!(menu);
            swap!(bass_density);
            swap!(gameplay);
            swap!(result_screen);
            swap!(user);
            swap!(lazer_multi_spectating);
            swap!(ranked_play);
            swap!(room);
            // Precise status alone must invalidate regular gameplay gating,
            // mode/mods, tourney availability and common status.
            for status in [2, 7, 11, 0] {
                Arc::make_mut(&mut current.precise).status = status;
                publish(&current, "precise/status");
            }
            current.precise = target.precise.clone();
            publish(&current, "precise/all");
            current.generation += 1;
            publish(&current, "generation");
            current.reset_epoch += 1;
            publish(&current, "reset");
        }
    }
}
