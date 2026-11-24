#[test]
fn request_shape_and_frozen_compact_frames_are_exact() {
    assert_eq!(
        parse_subscription_request(br#"["/a","/b"]"#),
        Ok(vec!["/a".into(), "/b".into()])
    );
    assert_eq!(
        parse_subscription_request(br#"{"a":1}"#),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert_eq!(
        parse_subscription_request(br#"[1]"#),
        Err(SubscriptionRequestError::InvalidRequest)
    );

    let initial = initial_frame(7, &[Some(json!(null)), None, Some(json!(-0.0))]).unwrap();
    assert_eq!(initial.as_ref(), br#"[0,7,[[0,null],[2,0]]]"#);
    let numeric = initial_frame(
        8,
        &[
            Some(json!(1e20)),
            Some(json!(1e21)),
            Some(json!(1e-6)),
            Some(json!(1e-7)),
            Some(json!(f64::INFINITY)),
        ],
    )
    .unwrap();
    assert_eq!(
        numeric.as_ref(),
        br#"[0,8,[[0,100000000000000000000],[1,1e+21],[2,0.000001],[3,1e-7],[4,null]]]"#
    );
    let update = update_frame(
        9,
        &[Some(json!(1)), Some(json!(2)), None],
        &[Some(json!(3)), None, Some(json!(null))],
    )
    .unwrap();
    assert_eq!(update.as_ref(), br#"[1,9,[[0,3],[2,null]],[1]]"#);
    assert_eq!(
        initial_frame(0, &[None, None]).unwrap().as_ref(),
        br#"[0,0,[]]"#
    );
    assert_eq!(
        update_frame(10, &[Some(json!(1))], &[None])
            .unwrap()
            .as_ref(),
        br#"[1,10,[],[0]]"#
    );
    assert_eq!(
        update_frame(11, &[Some(json!(1)), None], &[Some(json!(1)), None])
            .unwrap()
            .as_ref(),
        br#"[1,11,[],[]]"#
    );
    assert_eq!(
        update_frame(
            12,
            &[Some(json!(1)), None, Some(json!(3))],
            &[None, None, None],
        )
        .unwrap()
        .as_ref(),
        br#"[1,12,[],[0,2]]"#
    );
    assert_eq!(
        error_frame(INVALID_REQUEST, INVALID_REQUEST_MESSAGE).as_ref(),
        br#"[2,1,"invalid subscription request"]"#
    );
    assert_eq!(
        error_frame(INVALID_POINTER, INVALID_POINTER_MESSAGE).as_ref(),
        br#"[2,2,"invalid JSON Pointer"]"#
    );
    assert_eq!(
        error_frame(SUBSCRIPTION_TIMEOUT, SUBSCRIPTION_TIMEOUT_MESSAGE).as_ref(),
        br#"[2,3,"subscription timed out"]"#
    );
    assert_eq!(
        error_frame(SERIALIZATION_FAILED, SERIALIZATION_FAILED_MESSAGE).as_ref(),
        br#"[2,4,"state serialization failed"]"#
    );
    assert_eq!(
        error_frame(ALREADY_SUBSCRIBED, ALREADY_SUBSCRIBED_MESSAGE).as_ref(),
        br#"[2,5,"subscription already established"]"#
    );
}

#[test]
fn arena_frames_match_api_encoding_across_chunk_turnover_and_oversized_frames() {
    let values = [
        json!(null),
        json!(true),
        json!(-0.0),
        json!(0.1 + 0.2),
        json!(1e21),
        json!(1e-7),
        json!(f64::NEG_INFINITY),
        json!(u64::MAX),
        json!(i64::MIN),
        json!("\"q\\b\u{1}\u{7f}\u{2028}日本語🎵"),
        json!({"z": [1.5, {"a": null}], "a": []}),
    ];
    let oversized = json!("x".repeat(3 * FRAME_ARENA_CAPACITY));
    let pairs = |state: &[Option<Value>]| -> Vec<(u16, Value)> {
        state
            .iter()
            .enumerate()
            .filter_map(|(id, value)| value.clone().map(|value| (id as u16, value)))
            .collect()
    };

    let mut arena = FrameArena::default();
    let mut frames = Vec::new();
    let mut previous: Vec<Option<Value>> = Vec::new();
    for revision in 0..800_u64 {
        let len = (revision / 2) as usize % values.len() + 1;
        let current: Vec<Option<Value>> = (0..len)
            .map(|id| {
                if revision == 400 && id == 0 {
                    Some(oversized.clone())
                } else {
                    (!(id as u64 + revision).is_multiple_of(3))
                        .then(|| values[(id + revision as usize) % values.len()].clone())
                }
            })
            .collect();
        let (frame, expected) = if revision.is_multiple_of(2) || previous.len() != len {
            (
                arena.initial(revision, &current).unwrap(),
                Bytes::from(vosu_adapter_support::json::encode(&(0_u8, revision, pairs(&current)), 0).unwrap()),
            )
        } else {
            let changed: Vec<(u16, Value)> = pairs(&current)
                .into_iter()
                .filter(|(id, value)| previous[*id as usize].as_ref() != Some(value))
                .collect();
            let removed: Vec<u16> = (0..len)
                .filter(|&id| previous[id].is_some() && current[id].is_none())
                .map(|id| id as u16)
                .collect();
            (
                arena
                    .update(revision, &current, &{
                        let mut ids = Vec::new();
                        changes(&previous, &current, &mut ids);
                        ids
                    })
                    .unwrap(),
                Bytes::from(vosu_adapter_support::json::encode(&(1_u8, revision, changed, removed), 0).unwrap()),
            )
        };
        frames.push((frame, expected));
        previous = current;
    }
    // Compare only after every frame is encoded: later writes and chunk turnover must not
    // disturb frames already handed out.
    for (frame, expected) in &frames {
        assert_eq!(frame, expected);
    }
}
