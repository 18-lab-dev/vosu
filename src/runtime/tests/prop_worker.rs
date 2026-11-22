// Property tests for `api_state` (selection across cadence lanes). Commit
// 8fb8a6a fixed a bug where API consumers kept a stale regular component
// after a selection change; these generate random sequences of snapshot
// pairs rather than the single scenario the unit tests above assert.

use proptest::prelude::*;

use vosu_osu_state::RegularState;

fn prop_apply_selection(regular: &mut RegularState, selection: u8, title: u8) {
    regular.menu.checksum = Some(format!("checksum-{selection}"));
    regular.menu.filename = Some(format!("file-{selection}.osu"));
    regular.menu.folder = Some(format!("folder-{selection}"));
    regular.menu.audio_filename = Some(format!("audio-{selection}.mp3"));
    regular.menu.background_filename = Some(format!("bg-{selection}.jpg"));
    regular.global.songs_folder = format!("songs-{selection}");
    regular.menu.title = Some(format!("title-{title}"));
}

type SelectionFields = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

fn prop_selection_fields(regular: &RegularState) -> SelectionFields {
    (
        regular.menu.checksum.clone(),
        regular.menu.filename.clone(),
        regular.menu.folder.clone(),
        regular.menu.audio_filename.clone(),
        regular.menu.background_filename.clone(),
        regular.global.songs_folder.clone(),
    )
}

fn prop_build_snapshot(
    generation: u64,
    reset_epoch: u64,
    source: ClientType,
    selection: u8,
    title: u8,
) -> Arc<StateBundleSnapshot> {
    let mut snapshot = state_bundle(generation, reset_epoch);
    let bundle = Arc::make_mut(&mut snapshot);
    bundle.source = source;
    prop_apply_selection(Arc::make_mut(&mut bundle.regular), selection, title);
    snapshot
}

fn prop_client_type(lazer: bool) -> ClientType {
    if lazer { ClientType::Lazer } else { ClientType::Stable }
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]

    /// The API-visible regular selection (checksum, filename, folder, audio
    /// and background filenames, songs folder) always equals the live
    /// snapshot's selection, for every delivery lane and every prior state
    /// (including no prior state at all). `api_state` may retain a stale
    /// *non-selection* regular field, but never a stale selection.
    #[test]
    fn prop_api_state_selection_always_matches_live(
        has_previous in any::<bool>(),
        gen_prev in 0u64..3,
        reset_prev in 0u64..3,
        source_prev_lazer in any::<bool>(),
        selection_prev in 0u8..3,
        title_prev in 0u8..3,
        gen_live in 0u64..3,
        reset_live in 0u64..3,
        source_live_lazer in any::<bool>(),
        selection_live in 0u8..3,
        title_live in 0u8..3,
        same_arc in any::<bool>(),
    ) {
        let previous = prop_build_snapshot(
            gen_prev,
            reset_prev,
            prop_client_type(source_prev_lazer),
            selection_prev,
            title_prev,
        );
        let mut live = prop_build_snapshot(
            gen_live,
            reset_live,
            prop_client_type(source_live_lazer),
            selection_live,
            title_live,
        );
        if same_arc {
            let bundle = Arc::make_mut(&mut live);
            bundle.source = previous.source;
            bundle.generation = previous.generation;
            bundle.reset_epoch = previous.reset_epoch;
            bundle.regular = Arc::clone(&previous.regular);
        }
        let previous_arg = has_previous.then_some(&previous);

        for delivery in [Delivery::All, Delivery::Precise, Delivery::Live] {
            let result = api_state(previous_arg, &live, delivery);
            prop_assert_eq!(
                prop_selection_fields(&result.regular),
                prop_selection_fields(&live.regular),
                "selection diverged from live for {:?}",
                delivery
            );
        }

        let Some(previous) = previous_arg else { return Ok(()) };
        let same_identity = previous.source == live.source
            && previous.generation == live.generation
            && previous.reset_epoch == live.reset_epoch;

        if !same_identity {
            // Source, generation or reset-epoch changes always promote,
            // regardless of delivery lane or selection equality.
            for delivery in [Delivery::All, Delivery::Precise, Delivery::Live] {
                prop_assert!(Arc::ptr_eq(&api_state(previous_arg, &live, delivery), &live));
            }
            return Ok(());
        }

        let selection_unchanged = prop_selection_fields(&previous.regular) == prop_selection_fields(&live.regular);
        if !selection_unchanged {
            // A selection change on an otherwise-identical generation/reset
            // bypasses every cadence lane, not only the live stream.
            for delivery in [Delivery::All, Delivery::Precise, Delivery::Live] {
                prop_assert!(Arc::ptr_eq(&api_state(previous_arg, &live, delivery), &live));
            }
            return Ok(());
        }

        // Nothing selection-relevant changed: the latest-only contract (ADR
        // 0002) retains the previous regular component for API cadences,
        // and the live stream retains the whole previous publication.
        prop_assert!(Arc::ptr_eq(
            &api_state(previous_arg, &live, Delivery::All),
            &live
        ));
        prop_assert!(Arc::ptr_eq(
            &api_state(previous_arg, &live, Delivery::Precise).regular,
            &previous.regular
        ));
        prop_assert!(Arc::ptr_eq(
            &api_state(previous_arg, &live, Delivery::Live),
            previous
        ));
    }
}
