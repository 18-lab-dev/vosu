use super::*;
use crate::server::subscription_wire::{Mailbox, Outbound, Receiver, Selection};
use proptest::prelude::*;
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use vosu_proptest::vosu_proptest_config;

#[test]
fn shared_selection_preserves_request_order_and_utf8_byte_limits() {
    let selection = Selection::compile(vec!["/a~01b".into(), "/a~1b/c".into()]).unwrap();
    assert_eq!(
        selection.pointers(),
        &[
            vec!["a~1b".to_owned()],
            vec!["a/b".to_owned(), "c".to_owned()]
        ]
    );
    let document = json!({"a~1b": null, "a/b": {"c": 83}});
    assert_eq!(
        &*extract(&selection, Some(&document)),
        &[Some(Value::Null), Some(json!(83))]
    );
    let exact = format!("/{}a", "é".repeat(255));
    assert_eq!(exact.len(), 512);
    assert!(Selection::compile(vec![exact.clone()]).is_ok());
    assert_eq!(
        Selection::compile(vec![format!("{exact}b")]),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert_eq!(
        Selection::compile(vec!["/a~01b".into(), "/a~01b/c".into()]),
        Err(SubscriptionRequestError::InvalidPointer)
    );
}

#[test]
fn shared_mailbox_retains_bounded_slots_and_terminal_control_data_priority() {
    let mailbox = Arc::new(Mailbox::new());
    let mut receiver = Receiver::new(Arc::clone(&mailbox));
    let data = Outbound::Text(Bytes::from_static(b"data"));
    let pong = Outbound::Pong(Bytes::from_static(b"pong"));
    let error = Outbound::Error(Bytes::from_static(b"error"));
    mailbox.try_send_data(data.clone()).unwrap();
    assert_eq!(
        mailbox.try_send_data(Outbound::Text(Bytes::from_static(b"overflow"))),
        Err(Outbound::Text(Bytes::from_static(b"overflow")))
    );
    assert!(mailbox.try_send_control(pong.clone()));
    assert!(!mailbox.try_send_control(Outbound::Pong(Bytes::from_static(b"overflow"))));
    mailbox.try_send_terminal(error.clone());
    mailbox.try_send_terminal(Outbound::Close { code: 1008 });
    mailbox.try_send_terminal(Outbound::Close { code: 1011 });
    assert_eq!(receiver.try_recv(), Some(error));
    assert_eq!(receiver.try_recv(), Some(Outbound::Close { code: 1008 }));
    assert_eq!(receiver.try_recv(), Some(pong));
    assert_eq!(receiver.try_recv(), Some(data));
    assert_eq!(receiver.try_recv(), None);
    drop(receiver);
    assert!(mailbox.try_send_data(Outbound::Text(Bytes::new())).is_err());
    assert!(!mailbox.try_send_control(Outbound::Pong(Bytes::new())));
}

#[tokio::test]
async fn shared_mailbox_registers_wait_before_send_and_sender_close() {
    let mailbox = Arc::new(Mailbox::new());
    let mut receiver = Receiver::new(Arc::clone(&mailbox));
    {
        let mut pending = pin!(receiver.recv());
        assert!(matches!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        let pong = Outbound::Pong(Bytes::from_static(b"registered"));
        assert!(mailbox.try_send_control(pong.clone()));
        assert_eq!(bounded!(pending), Some(pong));
    }
    let mut pending = pin!(receiver.recv());
    assert!(matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    mailbox.close_senders();
    assert_eq!(bounded!(pending), None);
}

proptest! {
    #![proptest_config(vosu_proptest_config())]

    /// Removal ids are original request indices, never positions within the changed list.
    /// Explicit null is present and must not be confused with an absent selected field.
    #[test]
    fn prop_shared_frames_keep_sparse_removal_indices(
        count in 3_usize..=256,
        revision in 1_u64..u64::MAX,
        value in any::<i32>(),
    ) {
        let old: Vec<_> = (0..count).map(|id| Some(json!(id + 19))).collect();
        let mut current = old.clone();
        current[1] = None;
        current[count - 1] = Some(Value::Null);
        current[0] = Some(json!(value));
        let mut changed = Vec::new();
        changes(&old, &current, &mut changed);
        let mut arena = FrameArena::default();
        let bytes = arena.update(revision, &current, &changed).unwrap();
        let present: Vec<_> = changed.iter().filter_map(|&id| current[id].as_ref().map(|v| (id, v))).collect();
        let expected = Bytes::from(vosu_adapter_support::json::encode(&(1_u8, revision, present, [1_usize]), 0).unwrap());
        prop_assert_eq!(bytes, expected);
        prop_assert!(changed.contains(&(count - 1)));
    }
}
