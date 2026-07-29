use std::sync::Arc;

use indexmap::IndexMap;

use super::*;
use crate::model::enums::ClientType;
use crate::model::memory::*;
use crate::model::states::{CalculateMods, HitErrorArray};
use crate::process::{MemoryError, MemoryRead, ReadStatus};

fn mods(checksum: &str) -> CalculateMods {
    CalculateMods {
        checksum: checksum.into(),
        number: 0,
        name: String::new(),
        array: Vec::new(),
        rate: 1.0,
    }
}

fn global() -> Global {
    Global {
        is_watching_replay: false,
        is_replay_ui_hidden: false,
        is_multi_spectating: false,
        show_interface: true,
        chat_status: 0,
        status: 0,
        play_time: 10.0,
        game_time: 10,
        menu_mods: mods("menu"),
        skin_folder: "skin".into(),
        memory_songs_folder: "Songs".into(),
    }
}

fn user() -> User {
    User {
        name: "player".into(),
        accuracy: 100.0,
        ranked_score: 0.0,
        id: 1,
        level: 1.0,
        play_count: 0,
        play_mode: 0,
        rank: 1,
        country_code: 0,
        performance_points: 0.0,
        raw_bancho_status: 0,
        background_colour: 0,
        raw_login_status: 0,
        matchmaking: None,
    }
}

fn regular_builder(source: ClientType, menu: MemoryRead<Menu>) -> RawRegularSnapshotBuilder {
    RawRegularSnapshotBuilder::new(
        RegularCaptureContext::new(source, 7, 3, 11),
        Ok(ReadStatus::Ready(global())),
    )
    .menu(menu)
    .settings(Ok(ReadStatus::Ready(IndexMap::new())))
    .user(Ok(ReadStatus::Ready(user())))
}

#[test]
fn preserves_each_error_and_status_without_defaulting() {
    let read_error = MemoryError::Read {
        address: 0x1234,
        len: 8,
        message: "short read".into(),
    };
    let snapshot = regular_builder(ClientType::Stable, Ok(ReadStatus::NotReady))
        .mp3_length(Ok(ReadStatus::Unchanged))
        .result_screen(Err(read_error.clone()))
        .ranked_play(Ok(ReadStatus::Unsupported(
            "Stable: ranked play not implemented",
        )))
        .publish();

    assert!(matches!(snapshot.menu(), Some(Ok(ReadStatus::NotReady))));
    assert!(matches!(
        snapshot.mp3_length(),
        Some(Ok(ReadStatus::Unchanged))
    ));
    assert_eq!(snapshot.result_screen(), Some(&Err(read_error)));
    assert!(matches!(
        snapshot.ranked_play(),
        Some(Ok(ReadStatus::Unsupported(
            "Stable: ranked play not implemented"
        )))
    ));
}

#[test]
fn menu_checksum_and_full_update_remain_distinct_payloads() {
    let checksum = regular_builder(
        ClientType::Stable,
        Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
            gamemode: 2,
            ranked_status: 4.0,
        }))),
    )
    .publish();
    assert!(matches!(
        checksum.menu(),
        Some(Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
            gamemode: 2,
            ranked_status: 4.0
        }))))
    ));

    let full = regular_builder(
        ClientType::Stable,
        Ok(ReadStatus::Ready(Menu::Full(MenuFull {
            gamemode: 2,
            checksum: "new".into(),
            filename: "map.osu".into(),
            plays: 1,
            artist: String::new(),
            artist_original: String::new(),
            title: String::new(),
            title_original: String::new(),
            ar: 9.0,
            cs: 4.0,
            hp: 6.0,
            od: 8.0,
            audio_filename: String::new(),
            audio_file_mimetype: String::new(),
            background_filename: String::new(),
            background_file_mimetype: String::new(),
            folder: String::new(),
            creator: String::new(),
            difficulty: String::new(),
            map_id: 0,
            set_id: 0,
            ranked_status: 4.0,
            object_count: 0,
        }))),
    )
    .publish();
    assert!(matches!(
        full.menu(),
        Some(Ok(ReadStatus::Ready(Menu::Full(MenuFull { checksum, .. })))) if checksum == "new"
    ));
}

#[test]
fn publication_owns_and_isolates_nested_vectors_and_strings() {
    let mut source = vec![1.0, 2.0];
    let snapshot = regular_builder(
        ClientType::Stable,
        Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
            gamemode: 0,
            ranked_status: 0.0,
        }))),
    )
    .audio_velocity_base(Ok(ReadStatus::Ready(source.clone())))
    .publish();
    source.push(3.0);

    let cloned_arc = Arc::clone(&snapshot);
    let Some(Ok(ReadStatus::Ready(samples))) = cloned_arc.audio_velocity_base() else {
        panic!("audio sample missing");
    };
    assert_eq!(samples, &[1.0, 2.0]);
    assert!(Arc::ptr_eq(&snapshot, &cloned_arc));
    assert!(Arc::get_mut(&mut Arc::clone(&snapshot)).is_none());
}

#[test]
fn precise_acceptance_requires_exact_base_and_new_sequence() {
    let current_mods = mods("mods-a");
    let capture =
        PreciseCaptureContext::new(ClientType::Lazer, 4, 9, 3, current_mods.clone(), 12, 20);
    let snapshot = RawPreciseSnapshotBuilder::new(
        capture,
        21,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: 2,
            time: 500.0,
        })),
    )
    .hit_errors(Ok(ReadStatus::Ready(HitErrors {
        index: 14,
        errors: HitErrorArray {
            all: vec![-2.0, 3.0],
            press: vec![-2.0, 3.0],
            release: Vec::new(),
        },
        release_corrections: Vec::new(),
        stable_replay_frames: None,
    })))
    .publish();

    assert!(snapshot.can_accept(
        ClientType::Lazer,
        InstanceEpoch::new(4, 9),
        3,
        &current_mods,
        12,
        20
    ));
    assert!(!snapshot.can_accept(
        ClientType::Lazer,
        InstanceEpoch::new(4, 10),
        3,
        &current_mods,
        12,
        20
    ));
    assert!(!snapshot.can_accept(
        ClientType::Lazer,
        InstanceEpoch::new(4, 9),
        3,
        &mods("mods-b"),
        12,
        20
    ));
    assert!(!snapshot.can_accept(
        ClientType::Stable,
        InstanceEpoch::new(4, 9),
        3,
        &current_mods,
        12,
        20
    ));

    let stale = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Lazer, 4, 9, 3, current_mods, 12, 20),
        20,
        Ok(ReadStatus::NotReady),
    )
    .publish();
    assert_eq!(
        stale.validate_sequence(),
        Err(PreciseSequenceError::ProducedSequenceNotNewer)
    );
}

#[test]
fn suffix_cannot_contain_more_hits_than_consumed_source_positions() {
    let snapshot = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Stable, 1, 0, 0, mods(""), 5, 8),
        9,
        Ok(ReadStatus::NotReady),
    )
    .hit_errors(Ok(ReadStatus::Ready(HitErrors {
        index: 5,
        errors: HitErrorArray {
            all: vec![1.0],
            press: vec![1.0],
            release: Vec::new(),
        },
        release_corrections: Vec::new(),
        stable_replay_frames: None,
    })))
    .publish();
    assert_eq!(
        snapshot.validate_sequence(),
        Err(PreciseSequenceError::HitErrorSuffixExceedsConsumedRange)
    );
}

#[test]
fn source_specific_absent_branches_are_not_pending_reads() {
    let stable = regular_builder(
        ClientType::Stable,
        Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
            gamemode: 0,
            ranked_status: 0.0,
        }))),
    )
    .publish();
    assert_eq!(stable.context().source, ClientType::Stable);
    assert!(stable.ranked_play().is_none());
    assert!(stable.lazer_spectating().is_none());

    let lazer = regular_builder(
        ClientType::Lazer,
        Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
            gamemode: 0,
            ranked_status: f64::NAN,
        }))),
    )
    .publish();
    assert_eq!(lazer.context().source, ClientType::Lazer);
    assert!(lazer.tourney().is_none());
    assert!(lazer.tourney_chat().is_none());
    assert!(lazer.tourney_user().is_none());
}

#[test]
fn precise_optional_branches_distinguish_skipped_from_not_ready() {
    let skipped = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Stable, 1, 0, 0, mods(""), 0, 0),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: 0,
            time: 0.0,
        })),
    )
    .publish();
    assert!(skipped.key_overlay().is_none());
    assert!(skipped.hit_errors().is_none());

    let attempted = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Stable, 1, 0, 0, mods(""), 0, 0),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: 2,
            time: 200.0,
        })),
    )
    .key_overlay(Ok(ReadStatus::NotReady))
    .hit_errors(Err(MemoryError::Backend {
        operation: "hit errors",
        message: "gone".into(),
    }))
    .publish();
    assert!(matches!(
        attempted.key_overlay(),
        Some(Ok(ReadStatus::NotReady))
    ));
    assert!(matches!(attempted.hit_errors(), Some(Err(_))));
    assert_eq!(attempted.ending_index(), attempted.capture().start_index());
}
#[test]
fn precise_global_preserves_nonfinite_raw_time() {
    let snapshot = RawPreciseSnapshotBuilder::new(
        PreciseCaptureContext::new(ClientType::Lazer, 1, 0, 0, mods(""), 0, 0),
        1,
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: 2,
            time: f64::NAN,
        })),
    )
    .publish();

    assert!(matches!(
        snapshot.global(),
        Ok(ReadStatus::Ready(GlobalPrecise { time, .. })) if time.is_nan()
    ));
}
