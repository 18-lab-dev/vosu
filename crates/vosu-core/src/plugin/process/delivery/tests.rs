use vosu_plugin_api::{
    ActionInvoke, ActionProgress, ActionResult, ActionStatus, Caller, CallerKind, CborValue,
    DetachReason, FocusChanged, Message, SessionDetached, StateEntry, StateUpdate, StreamClose,
    TimerFired, vosu_opcode,
};

use super::super::fake_plugin::FakePlugin;
use super::*;

fn push(queue: &DeliveryQueue, message: &Message) -> Result<(), DeliveryError> {
    let header = FrameHeader {
        opcode: message.opcode() as u32,
        flags: 0,
        correlation: 0,
    };
    let mut buffer = PayloadBuffer::new();
    buffer.fill(&message.encode_payload().unwrap());
    let inbound = inspect(&header, buffer.bytes())?;
    queue.push(header, &mut buffer, inbound)
}

fn entry(pointer: &str, value: Option<u64>) -> StateEntry {
    StateEntry {
        ns: "acme.other".into(),
        pointer: pointer.into(),
        value: value.map(|value| CborValue::Integer(value.into())),
        error: None,
    }
}

fn update(session: u64, values: Vec<StateEntry>, more: bool) -> Message {
    Message::StateUpdate(StateUpdate {
        session,
        values,
        more,
    })
}

fn focus(handle: u64, focused: bool) -> Message {
    Message::FocusChanged(FocusChanged {
        handle,
        focused,
        foreground: None,
    })
}

fn invoke(invocation: u64) -> Message {
    Message::ActionInvoke(ActionInvoke {
        invocation,
        action: "acme.fixture.run".into(),
        session: None,
        caller: Caller {
            kind: CallerKind::Tool,
            id: "tool".into(),
        },
        args: CborValue::Null,
    })
}

fn result(invocation: u64) -> Message {
    Message::ActionResult(ActionResult {
        invocation,
        status: ActionStatus::Ok,
        value: None,
        code: None,
        message: None,
    })
}

/// Takes and dispatches everything queued, returning the decoded messages.
fn drain(queue: &DeliveryQueue) -> Vec<Message> {
    let plugin = FakePlugin::new();
    let target = plugin.target();
    let mut scratch = PayloadBuffer::new();
    queue.close();
    while let Some(mut delivery) = queue.next() {
        delivery.dispatch(&target, &mut scratch);
        queue.done(delivery);
    }
    plugin
        .calls()
        .into_iter()
        .map(|call| {
            assert!(call.aligned, "every dispatched payload is 8-byte aligned");
            let opcode = vosu_opcode::try_from(call.opcode).unwrap();
            Message::decode(opcode, call.flags, &call.payload).unwrap()
        })
        .collect()
}

fn updates(messages: &[Message]) -> Vec<StateUpdate> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::StateUpdate(update) => Some(update.clone()),
            _ => None,
        })
        .collect()
}

/// T1 counterexamples: a split sequence is never merged before its
/// `more: false` part, and merging never replaces wholesale (a preceding
/// update's removal survives a newer update of other keys).
#[test]
fn split_updates_merge_only_when_complete_and_keep_every_key() {
    let queue = DeliveryQueue::default();
    // A complete update: a value and a removal (`value: null`).
    push(
        &queue,
        &update(4, vec![entry("/a", Some(0)), entry("/gone", None)], false),
    )
    .unwrap();
    // A split sequence, first part only.
    push(&queue, &update(4, vec![entry("/a", Some(1))], true)).unwrap();
    // Only the complete update is deliverable; the part is held.
    let mut taken = queue.next().unwrap();
    let plugin = FakePlugin::new();
    let mut scratch = PayloadBuffer::new();
    taken.dispatch(&plugin.target(), &mut scratch);
    queue.done(taken);
    let delivered = plugin.calls();
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        Message::decode(vosu_opcode::VOSU_OP_STATE_UPDATE, 0, &delivered[0].payload).unwrap(),
        update(4, vec![entry("/a", Some(0)), entry("/gone", None)], false),
        "a single unsplit update is delivered as its own bytes"
    );
    // The sequence completes; another complete update merges with it.
    push(&queue, &update(4, vec![entry("/b", Some(2))], false)).unwrap();
    push(
        &queue,
        &update(4, vec![entry("/gone", None), entry("/c", Some(3))], false),
    )
    .unwrap();
    push(&queue, &update(4, vec![entry("/b", Some(5))], false)).unwrap();
    let merged = updates(&drain(&queue));
    assert_eq!(
        merged,
        vec![StateUpdate {
            session: 4,
            values: vec![
                entry("/a", Some(1)),
                entry("/b", Some(5)),
                entry("/gone", None),
                entry("/c", Some(3)),
            ],
            more: false,
        }],
        "one merged update, newest per key, no key lost"
    );
}

/// A merged update larger than one frame is re-split at the same cap, with
/// `more` on every part but the last; sessions never mix.
#[test]
fn a_merged_update_over_the_frame_cap_is_re_split() {
    let queue = DeliveryQueue::default();
    let big = |pointer: &str| StateEntry {
        ns: "acme.other".into(),
        pointer: pointer.into(),
        value: Some(CborValue::Bytes(vec![9; 600 * 1024])),
        error: None,
    };
    push(&queue, &update(1, vec![big("/x")], false)).unwrap();
    push(&queue, &update(1, vec![big("/y")], false)).unwrap();
    push(&queue, &update(2, vec![entry("/z", Some(1))], false)).unwrap();
    let parts = updates(&drain(&queue));
    let session_one: Vec<_> = parts.iter().filter(|part| part.session == 1).collect();
    assert_eq!(session_one.len(), 2);
    assert!(session_one[0].more && !session_one[1].more);
    assert_eq!(parts.iter().filter(|part| part.session == 2).count(), 1);
}

/// Latest-only keys coalesce in place: the newest payload, at the oldest
/// position, one per key.
#[test]
fn latest_only_notifications_coalesce_per_key_in_place() {
    let queue = DeliveryQueue::default();
    push(&queue, &focus(7, false)).unwrap();
    push(&queue, &invoke(1)).unwrap();
    push(&queue, &focus(8, true)).unwrap();
    push(&queue, &focus(7, true)).unwrap();
    push(&queue, &Message::TimerFired(TimerFired { timer: 3 })).unwrap();
    push(&queue, &Message::TimerFired(TimerFired { timer: 3 })).unwrap();
    for fraction in [0.1, 0.5, 0.9] {
        push(
            &queue,
            &Message::ActionProgress(ActionProgress {
                invocation: 6,
                fraction: Some(fraction),
                message: None,
            }),
        )
        .unwrap();
    }
    let messages = drain(&queue);
    assert_eq!(
        messages,
        vec![
            focus(7, true),
            invoke(1),
            focus(8, true),
            Message::TimerFired(TimerFired { timer: 3 }),
            Message::ActionProgress(ActionProgress {
                invocation: 6,
                fraction: Some(0.9),
                message: None,
            }),
        ]
    );
}

/// T1 counterexample: a terminal result behind a backlog of ordinary work
/// is neither dropped nor delayed behind it, and no ordinary work is lost.
#[test]
fn terminal_results_overtake_an_ordinary_backlog_and_nothing_is_dropped() {
    let queue = DeliveryQueue::default();
    for invocation in 0..2_000 {
        push(&queue, &invoke(invocation)).unwrap();
    }
    push(&queue, &result(77)).unwrap();
    push(&queue, &Message::StreamClose(StreamClose { stream: 5 })).unwrap();
    let messages = drain(&queue);
    assert_eq!(messages.len(), 2_002);
    assert_eq!(messages[0], result(77));
    assert_eq!(messages[1], Message::StreamClose(StreamClose { stream: 5 }));
    for (index, message) in messages[2..].iter().enumerate() {
        assert_eq!(*message, invoke(index as u64));
    }
}

#[test]
fn queued_bytes_are_bounded_and_overflow_is_a_violation() {
    let queue = DeliveryQueue::with_budget(4096);
    let message = invoke(1);
    let len = message.encode_payload().unwrap().len();
    let mut pushed = 0;
    let error = loop {
        match push(&queue, &invoke(pushed)) {
            Ok(()) => pushed += 1,
            Err(error) => break error,
        }
    };
    assert_eq!(error, DeliveryError::Budget);
    assert_eq!(pushed as usize, 4096 / len);
    let (bytes, high_water) = queue.bytes();
    assert!(bytes <= 4096 && high_water <= 4096);
    // Coalescing never grows the account.
    let queue = DeliveryQueue::with_budget(4096);
    for _ in 0..10_000 {
        push(&queue, &focus(1, true)).unwrap();
    }
    assert_eq!(
        queue.bytes().1,
        focus(1, true).encode_payload().unwrap().len()
    );
}

/// A resubscription discards what was not yet delivered for its session,
/// including the rest of a split sequence still arriving.
#[test]
fn a_resubscription_clears_the_session_and_the_rest_of_its_sequence() {
    let queue = DeliveryQueue::default();
    push(&queue, &update(3, vec![entry("/old", Some(1))], false)).unwrap();
    push(&queue, &update(3, vec![entry("/old2", Some(1))], true)).unwrap();
    queue.reset_state(3);
    push(&queue, &update(3, vec![entry("/old3", Some(1))], false)).unwrap();
    push(&queue, &update(3, vec![entry("/new", Some(2))], false)).unwrap();
    assert_eq!(
        updates(&drain(&queue)),
        vec![StateUpdate {
            session: 3,
            values: vec![entry("/new", Some(2))],
            more: false,
        }]
    );
    assert_eq!(queue.bytes().0, 0);
}

/// A session's frames never run concurrently or out of order; other work
/// may run beside them.
#[test]
fn session_affine_deliveries_are_serialized() {
    let queue = DeliveryQueue::default();
    push(&queue, &update(1, vec![entry("/a", Some(1))], false)).unwrap();
    push(&queue, &focus(1, true)).unwrap();
    push(&queue, &invoke(9)).unwrap();
    push(
        &queue,
        &Message::SessionDetached(SessionDetached {
            handle: 1,
            reason: DetachReason::Exited,
        }),
    )
    .unwrap();
    let first = queue.next().unwrap();
    // Session 1 is running: the next eligible item is the unrelated work.
    let second = queue.next().unwrap();
    assert!(matches!(&second, Delivery::Frame { session: None, .. }));
    queue.done(second);
    queue.done(first);
    let third = queue.next().unwrap();
    assert!(matches!(
        &third,
        Delivery::Frame {
            session: Some(1),
            ..
        }
    ));
    queue.done(third);
    let fourth = queue.next().unwrap();
    match &fourth {
        Delivery::Frame { entry, .. } => assert_eq!(
            entry.header.opcode,
            vosu_opcode::VOSU_OP_SESSION_DETACHED as u32
        ),
        other => panic!("{other:?}"),
    }
    queue.done(fourth);
}

#[test]
fn nothing_is_accepted_after_destroy_and_loss_ends_delivery() {
    let queue = DeliveryQueue::default();
    push(&queue, &invoke(1)).unwrap();
    assert!(queue.close());
    assert!(!queue.close(), "a second DESTROY is refused");
    assert_eq!(
        push(&queue, &invoke(2)),
        Err(DeliveryError::AfterDestroy(
            vosu_opcode::VOSU_OP_ACTION_INVOKE as u32
        ))
    );
    let taken = queue.next().unwrap();
    queue.done(taken);
    assert!(queue.next().is_none(), "drained after DESTROY");

    let queue = std::sync::Arc::new(DeliveryQueue::default());
    let waiting = std::thread::spawn({
        let queue = std::sync::Arc::clone(&queue);
        move || queue.next().is_none()
    });
    queue.abort();
    assert!(waiting.join().unwrap());
}

#[test]
fn malformed_core_frames_are_violations() {
    let header = FrameHeader {
        opcode: vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
        flags: 0,
        correlation: 0,
    };
    assert_eq!(
        inspect(&header, &[0xff, 0x00]).unwrap_err(),
        DeliveryError::Malformed(header.opcode)
    );
    let mut too_many = Vec::new();
    for index in 0..300 {
        too_many.push(entry(&format!("/{index}"), Some(1)));
    }
    let queue = DeliveryQueue::default();
    assert_eq!(
        push(&queue, &update(1, too_many, true)),
        Err(DeliveryError::StateOverflow(1))
    );
}
