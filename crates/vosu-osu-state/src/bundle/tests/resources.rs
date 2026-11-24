use super::*;
use vosu_osu_model::memory::{BeatmapResources, CurrentSkin};
use vosu_proptest::wait::JoinBounded;

fn selection(checksum: &str) -> MenuFull {
    let mut full = menu(checksum);
    full.resources = Some(Arc::new(BeatmapResources {
        files: [
            ("map.osu", "chart-hash"),
            ("audio.mp3", "audio-hash"),
            ("bg.jpg", "image-hash"),
        ]
        .into_iter()
        .map(|(name, hash)| (name.into(), hash.into()))
        .collect(),
        audio_name: "audio.mp3".into(),
        background_name: "bg.jpg".into(),
    }));
    full
}

#[test]
fn cold_facts_share_indexes_and_checksum_ticks_preserve_generation() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 312, Arc::new(EchoCalculation));
    let full = selection("map-a");
    bundle
        .menu
        .apply(&RawMenu::Full(full.clone()), false, Instant::now());
    let captured = bundle.materialize_current().cold_resource_facts();
    assert!(Arc::ptr_eq(
        captured.set.as_ref().unwrap(),
        full.resources.as_ref().unwrap()
    ));
    assert_eq!(captured.set.as_ref().unwrap().files.len(), 3);
    assert_eq!(captured.set.as_ref().unwrap().files["bg.jpg"], "image-hash");
    bundle.menu.apply(
        &RawMenu::Checksum(vosu_osu_model::memory::MenuChecksum {
            gamemode: 2,
            ranked_status: 4.0,
        }),
        false,
        Instant::now(),
    );
    let current = bundle.materialize_current();
    assert!(captured.is_current(&current));
    assert!(Arc::ptr_eq(
        captured.set.as_ref().unwrap(),
        current.regular.menu.resources.as_ref().unwrap()
    ));
    let wire = serde_json::to_value(&full).unwrap();
    assert!(wire.get("resources").is_none());
    assert!(!wire.to_string().contains("chart-hash"));
}

#[test]
fn late_map_a_worker_cannot_replace_b_or_a_new_selection_of_a() {
    let mut bundle = StateBundle::new(ClientType::Lazer, 313, Arc::new(EchoCalculation));
    bundle
        .menu
        .apply(&RawMenu::Full(selection("map-a")), false, Instant::now());
    let facts = bundle.materialize_current().cold_resource_facts();
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let (completed, result) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        completed.send(facts).unwrap();
    });
    bundle
        .menu
        .apply(&RawMenu::Full(selection("map-b")), false, Instant::now());
    release.send(()).unwrap();
    let old = result.recv_timeout(Duration::from_secs(10)).unwrap();
    worker.join_bounded().unwrap();
    assert!(!old.is_current(&bundle.materialize_current()));
    bundle
        .menu
        .apply(&RawMenu::Full(selection("map-a")), false, Instant::now());
    assert!(!old.is_current(&bundle.materialize_current()));
    let current = bundle.materialize_current().cold_resource_facts();
    assert!(current.is_current(&bundle.materialize_current()));
}

#[test]
fn cold_results_are_invalidated_by_root_skin_reset_and_instance_changes() {
    let mut bundle = StateBundle::new(ClientType::Stable, 314, Arc::new(EchoCalculation));
    bundle
        .menu
        .apply(&RawMenu::Full(menu("stable-map")), false, Instant::now());
    bundle.global.game_folder = "/client/install".into();
    bundle.global.songs_folder = "/client/Songs".into();
    bundle.global.memory_songs_folder = "Z:\\custom-songs".into();
    bundle.global.skin_folder = "chosen-skin".into();
    let facts = bundle.materialize_current().cold_resource_facts();
    assert_eq!(facts.memory_songs_folder, "Z:\\custom-songs");
    assert!(facts.set.is_none());
    for field in 0..4 {
        let mut snapshot = (*bundle.materialize_current()).clone();
        let global = &mut Arc::make_mut(&mut snapshot.regular).global;
        match field {
            0 => global.game_folder.push_str("-new"),
            1 => global.songs_folder.push_str("-new"),
            2 => global.memory_songs_folder.push_str("-new"),
            _ => global.skin_folder.push_str("-new"),
        }
        assert!(!facts.is_current(&snapshot));
    }
    for field in 0..5 {
        let mut snapshot = (*bundle.materialize_current()).clone();
        let menu = &mut Arc::make_mut(&mut snapshot.regular).menu;
        match field {
            0 => menu.checksum = Some("new-checksum".into()),
            1 => menu.folder = Some("new-set".into()),
            2 => menu.filename = Some("new-chart.osu".into()),
            3 => menu.audio_filename = Some("new-audio.mp3".into()),
            _ => menu.background_filename = Some("parsed-background.jpg".into()),
        }
        assert!(!facts.is_current(&snapshot));
    }
    bundle.global.current_skin = Some(Arc::new(CurrentSkin {
        name: "imported".into(),
        builtin: None,
        base: None,
        files: [("cursor.png".into(), "skin-hash".into())]
            .into_iter()
            .collect(),
    }));
    let snapshot = bundle.materialize_current();
    assert!(!facts.is_current(&snapshot));
    let skin_facts = snapshot.cold_resource_facts();
    assert!(Arc::ptr_eq(
        skin_facts.skin.as_ref().unwrap(),
        snapshot.regular.global.current_skin.as_ref().unwrap()
    ));
    let mut reset = (*snapshot).clone();
    reset.reset_epoch += 1;
    assert!(!skin_facts.is_current(&reset));
    let mut replacement = (*snapshot).clone();
    replacement.generation += 1;
    assert!(!skin_facts.is_current(&replacement));
}
