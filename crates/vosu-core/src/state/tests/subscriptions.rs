use super::super::*;
use std::sync::Arc;
use vosu_plugin_api::{CborValue, StateEntry, StateUpdate, VOSU_FRAME_MAX_BYTES};

fn selectors(p: &str) -> Vec<(Arc<str>, Pointer)> {
    vec![(Arc::from("vosu.play"), Pointer::parse(p).unwrap())]
}
fn state(key: SessionKey, score: i64, combo: i64) -> Arc<SessionState> {
    Arc::new(SessionState::new(
        key,
        1,
        vec![(
            Arc::from("vosu.play"),
            StateValue::Object(Arc::new(StateObject::new(vec![
                (Arc::from("score"), StateValue::I64(score)),
                (Arc::from("combo"), StateValue::I64(combo)),
            ]))),
        )],
    ))
}
fn entry(pointer: &str, value: CborValue) -> StateEntry {
    StateEntry {
        ns: "vosu.play".into(),
        pointer: pointer.into(),
        value: Some(value),
        error: None,
    }
}
fn encoded(update: &StateUpdate) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(update, &mut bytes).unwrap();
    bytes
}

#[test]
fn replace_is_wholesale_and_empty_unsubscribes() {
    let schemas = SchemaRegistry::with_core().unwrap();
    let mut registry = SubscriptionRegistry::default();
    let who = SubscriberId::Plugin(5401);
    let key = SessionKey(5401);
    registry
        .replace(
            &schemas,
            who,
            key,
            selectors("/score"),
            InternalGrant::default(),
        )
        .unwrap();
    registry
        .replace(
            &schemas,
            who,
            key,
            selectors("/combo"),
            InternalGrant::default(),
        )
        .unwrap();
    assert_eq!(
        registry.baseline(&schemas, who, &state(key, 1, 2))[0].pointer,
        "/combo"
    );
    registry
        .replace(&schemas, who, key, vec![], InternalGrant::default())
        .unwrap();
    assert!(
        registry
            .baseline(&schemas, who, &state(key, 1, 2))
            .is_empty()
    );
    assert_eq!(
        registry.demand(key, Some("play")),
        DemandSnapshot::default()
    );
    assert!(registry.remove_subscriber(who).is_empty());
}

#[test]
fn changes_report_only_touched_selected_values() {
    let schemas = SchemaRegistry::with_core().unwrap();
    let mut registry = SubscriptionRegistry::default();
    let who = SubscriberId::Plugin(5501);
    let key = SessionKey(5501);
    let mut selected = selectors("/score");
    selected.extend(selectors("/combo"));
    registry
        .replace(&schemas, who, key, selected, InternalGrant::default())
        .unwrap();
    let outcome = CommitOutcome {
        previous: state(key, 1, 2),
        current: state(key, 3, 4),
        touched: selectors("/score"),
    };
    let changes = registry.changes(&schemas, &outcome);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].0, who);
    assert_eq!(
        changes[0].1,
        vec![entry("/score", CborValue::Integer(3.into()))]
    );
    let unchanged = CommitOutcome {
        current: state(key, 1, 2),
        ..outcome
    };
    assert!(registry.changes(&schemas, &unchanged).is_empty());
    assert_eq!(registry.remove_session(key), vec![who]);
    assert!(registry.changes(&schemas, &unchanged).is_empty());
}

#[test]
fn split_never_cuts_an_entry_and_marks_more() {
    let entries: Vec<_> = (0..30)
        .map(|i| entry(&format!("/{i}"), CborValue::Text("x".repeat(100))))
        .collect();
    let cap = 400;
    let parts = split_update(u64::MAX, entries.clone(), cap).unwrap();
    assert!(parts.len() > 1);
    for (i, part) in parts.iter().enumerate() {
        assert!(encoded(part).len() <= cap);
        assert_eq!(part.more, i + 1 < parts.len());
        assert_eq!(part.session, u64::MAX);
    }
    assert_eq!(
        parts.into_iter().flat_map(|p| p.values).collect::<Vec<_>>(),
        entries
    );
}

#[test]
fn array_header_boundary_and_exact_cap_are_counted() {
    let entries: Vec<_> = (0..25)
        .map(|i| entry(&format!("/{i}"), CborValue::Null))
        .collect();
    let exact = encoded(&StateUpdate {
        session: 0,
        values: entries.clone(),
        more: false,
    })
    .len();
    assert_eq!(split_update(0, entries.clone(), exact).unwrap().len(), 1);
    let parts = split_update(0, entries, exact - 1).unwrap();
    assert_eq!(parts.len(), 2);
    assert!(parts.iter().all(|p| encoded(p).len() < exact));
}

#[test]
fn oversize_entry_reports_value_too_large() {
    let parts = split_update(
        5601,
        vec![entry(
            "/score",
            CborValue::Bytes(vec![0; VOSU_FRAME_MAX_BYTES as usize + 1]),
        )],
        usize::MAX,
    )
    .unwrap();
    assert_eq!(parts.len(), 1);
    let e = &parts[0].values[0];
    assert!(e.value.is_none());
    assert_eq!(e.error.as_ref().unwrap().code, "value_too_large");
    assert!(encoded(&parts[0]).len() <= VOSU_FRAME_MAX_BYTES as usize);
    assert_eq!(
        split_update(0, vec![entry("/score", CborValue::Null)], 1),
        Err(UpdateError::CapTooSmall)
    );
}

#[test]
fn merge_newer_wins_per_key_and_keeps_older_others() {
    let mut pending = PendingUpdate::default();
    pending
        .merge(vec![entry("/score", 1.into()), entry("/combo", 2.into())])
        .unwrap();
    pending.merge(vec![entry("/score", 3.into())]).unwrap();
    assert_eq!(
        pending.take(),
        vec![entry("/score", 3.into()), entry("/combo", 2.into())]
    );
    assert!(pending.take().is_empty());
}

#[test]
fn internal_selector_without_grant_reports_absent() {
    let schemas = SchemaRegistry::with_core().unwrap();
    assert_eq!(
        schemas.hidden_subtrees("vosu.osu"),
        &[Pointer::parse("/internal").unwrap()]
    );
    let key = SessionKey(5701);
    let who = SubscriberId::Plugin(5701);
    let mut registry = SubscriptionRegistry::default();
    registry
        .replace(
            &schemas,
            who,
            key,
            vec![(Arc::from("vosu.osu"), Pointer::parse("/internal").unwrap())],
            InternalGrant::default(),
        )
        .unwrap();
    let document = SessionState::new(
        key,
        1,
        vec![(
            Arc::from("vosu.osu"),
            StateValue::Object(Arc::new(StateObject::new(vec![(
                Arc::from("internal"),
                StateValue::String(Arc::from("private")),
            )]))),
        )],
    );
    let baseline = registry.baseline(&schemas, who, &document);
    assert_eq!(baseline.len(), 1);
    assert!(baseline[0].value.is_none());
    assert!(baseline[0].error.is_none());
    let outcome = CommitOutcome {
        previous: Arc::new(document),
        current: Arc::new(SessionState::new(key, 2, vec![])),
        touched: vec![(Arc::from("vosu.osu"), Pointer::root())],
    };
    assert!(registry.changes(&schemas, &outcome).is_empty());
}

#[test]
fn registry_and_pending_bounds_are_transactional_and_released() {
    let schemas = SchemaRegistry::with_core().unwrap();
    let who = SubscriberId::Plugin(5801);
    let mut registry = SubscriptionRegistry::default();
    for i in 0..MAX_SUBSCRIPTIONS_PER_SUBSCRIBER {
        registry
            .replace(
                &schemas,
                who,
                SessionKey(i as u64),
                selectors("/score"),
                InternalGrant::default(),
            )
            .unwrap();
    }
    assert_eq!(
        registry.replace(
            &schemas,
            who,
            SessionKey(9999),
            selectors("/score"),
            InternalGrant::default()
        ),
        Err(SubscribeError::TooManySubscriptions)
    );
    registry
        .replace(
            &schemas,
            who,
            SessionKey(0),
            selectors("/combo"),
            InternalGrant::default(),
        )
        .unwrap();
    assert_eq!(
        registry.remove_subscriber(who).len(),
        MAX_SUBSCRIPTIONS_PER_SUBSCRIBER
    );
    registry
        .replace(
            &schemas,
            who,
            SessionKey(9999),
            selectors("/score"),
            InternalGrant::default(),
        )
        .unwrap();
    assert_eq!(
        registry.replace(
            &schemas,
            who,
            SessionKey(9999),
            (0..257)
                .map(|i| (
                    Arc::from("vosu.play"),
                    Pointer::parse(&format!("/{i}")).unwrap()
                ))
                .collect(),
            InternalGrant::default()
        ),
        Err(SubscribeError::TooManySelectors)
    );
    assert_eq!(registry.demand(SessionKey(9999), None).selectors.len(), 1);
    let mut pending = PendingUpdate::default();
    let entries: Vec<_> = (0..256)
        .map(|i| entry(&format!("/{i}"), CborValue::Null))
        .collect();
    pending.merge(entries.clone()).unwrap();
    assert_eq!(
        pending.merge(vec![entry("/extra", CborValue::Null)]),
        Err(UpdateError::TooManyEntries)
    );
    assert_eq!(pending.take(), entries);
}

#[test]
fn overlapping_selectors_and_invalid_names_leave_old_plan_intact() {
    let schemas = SchemaRegistry::with_core().unwrap();
    let mut registry = SubscriptionRegistry::default();
    let who = SubscriberId::Plugin(5901);
    let key = SessionKey(5901);
    registry
        .replace(
            &schemas,
            who,
            key,
            selectors("/score"),
            InternalGrant::default(),
        )
        .unwrap();
    let mut overlap = selectors("");
    overlap.extend(selectors("/score"));
    assert_eq!(
        registry.replace(&schemas, who, key, overlap, InternalGrant::default()),
        Err(SubscribeError::Overlap)
    );
    for ns in ["", "vosu.sessions", "invalid/name", &"x".repeat(129)] {
        assert_eq!(
            registry.replace(
                &schemas,
                who,
                key,
                vec![(Arc::from(ns), Pointer::root())],
                InternalGrant::default()
            ),
            Err(SubscribeError::InvalidNamespace)
        );
    }
    assert_eq!(
        registry.demand(key, Some("play")).selectors,
        vec![(Arc::from("vosu.play"), Arc::from("/score"))]
    );
}

#[test]
fn aggregate_demand_and_registry_are_bounded() {
    let schemas = SchemaRegistry::with_core().unwrap();
    let mut registry = SubscriptionRegistry::default();
    let key = SessionKey(6001);
    for id in 0..4 {
        let selectors = (0..256)
            .map(|i| {
                (
                    Arc::from("vosu.play"),
                    Pointer::parse(&format!("/{}", id * 256 + i)).unwrap(),
                )
            })
            .collect();
        registry
            .replace(
                &schemas,
                SubscriberId::Plugin(6001 + id),
                key,
                selectors,
                InternalGrant::default(),
            )
            .unwrap();
    }
    assert_eq!(registry.demand(key, None).selectors.len(), 1024);
    assert_eq!(
        registry.replace(
            &schemas,
            SubscriberId::Plugin(6005),
            key,
            selectors("/extra"),
            InternalGrant::default()
        ),
        Err(SubscribeError::TooManySelectors)
    );
    // Identical keys add no demand-table entries and remain admissible.
    registry
        .replace(
            &schemas,
            SubscriberId::Plugin(6005),
            key,
            selectors("/0"),
            InternalGrant::default(),
        )
        .unwrap();
    registry.remove_session(key);
    for i in 0..MAX_SUBSCRIPTIONS {
        registry
            .replace(
                &schemas,
                SubscriberId::Surface(7000 + i as u64),
                SessionKey(i as u64),
                selectors("/score"),
                InternalGrant::default(),
            )
            .unwrap();
    }
    assert_eq!(
        registry.replace(
            &schemas,
            SubscriberId::Surface(99999),
            key,
            selectors("/score"),
            InternalGrant::default()
        ),
        Err(SubscribeError::TooManySubscriptions)
    );
    assert_eq!(registry.remove_session(SessionKey(0)).len(), 1);
    registry
        .replace(
            &schemas,
            SubscriberId::Surface(99999),
            key,
            selectors("/score"),
            InternalGrant::default(),
        )
        .unwrap();
}

#[test]
fn split_accounts_for_metadata_and_pending_rejects_unrepresentable_keys() {
    let large = entry(&format!("/{}", "x".repeat(300)), CborValue::Null);
    assert_eq!(
        split_update(1, vec![large.clone()], 200),
        Err(UpdateError::CapTooSmall)
    );
    let mut pending = PendingUpdate::default();
    pending.merge(vec![entry("/score", 1.into())]).unwrap();
    let invalid = StateEntry {
        ns: "x".repeat(VOSU_FRAME_MAX_BYTES as usize),
        ..large
    };
    assert_eq!(pending.merge(vec![invalid]), Err(UpdateError::CapTooSmall));
    assert_eq!(pending.take(), vec![entry("/score", 1.into())]);
    let entries: Vec<_> = (0..257)
        .map(|i| entry(&format!("/{i}"), CborValue::Null))
        .collect();
    assert_eq!(
        split_update(1, entries, 1024),
        Err(UpdateError::TooManyEntries)
    );
}

#[test]
fn pending_preserves_values_that_fit_the_actual_session_envelope() {
    let cap = VOSU_FRAME_MAX_BYTES as usize;
    let empty = entry("/score", CborValue::Bytes(Vec::new()));
    let base = encoded(&StateUpdate {
        session: 1,
        values: vec![empty],
        more: false,
    })
    .len();
    // A large byte string widens its own CBOR length header from one to five bytes.
    let value = entry("/score", CborValue::Bytes(vec![0; cap - base - 4]));
    assert_eq!(
        encoded(&StateUpdate {
            session: 1,
            values: vec![value.clone()],
            more: false
        })
        .len(),
        cap
    );
    let mut pending = PendingUpdate::default();
    pending.merge(vec![value.clone()]).unwrap();
    let parts = split_update(1, pending.take(), cap).unwrap();
    assert_eq!(parts[0].values, vec![value.clone()]);
    let too_wide = split_update(u64::MAX, vec![value], cap).unwrap();
    assert_eq!(
        too_wide[0].values[0].error.as_ref().unwrap().code,
        "value_too_large"
    );
}
