use super::super::dispatch::SerialWorker;
use super::*;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

fn message(id: u64) -> OwnedMessage {
    OwnedMessage {
        opcode: 0,
        flags: 0,
        correlation_id: id,
        payload: Vec::new(),
    }
}
unsafe extern "C" fn dispatched(
    ctx: *mut std::ffi::c_void,
    message: *const vosu_plugin_api::vosu_message,
) -> u32 {
    // SAFETY: tests retain the boxed sender until the worker has stopped.
    let sender = unsafe { &*ctx.cast::<std::sync::mpsc::SyncSender<u64>>() };
    // SAFETY: OwnedMessage::call borrows a live message for this callback.
    let id = unsafe { (*message).correlation_id };
    sender.try_send(id).unwrap();
    vosu_status::VOSU_OK as u32
}
#[test]
fn reserved_outbox_delivers_fifo_through_full_queue_and_releases_slots() {
    let mut worker = SerialWorker::spawn("reserved-outbox-test".into(), 1);
    let (seen_tx, seen_rx) = sync_channel::<u64>(4);
    let mut sender = Box::new(seen_tx);
    let vtable = vosu_plugin_vtable {
        struct_size: std::mem::size_of::<vosu_plugin_vtable>() as u32,
        abi_version: 1,
        create: None,
        dispatch: Some(dispatched),
        destroy: None,
    };
    let ctx = PluginCtx((&raw mut *sender).cast());
    let outbox = ReservedOutbox::new(worker.queue(), vtable, ctx, InFlightTracker::new());
    let (started_tx, started_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    worker
        .submit(Job {
            vtable,
            ctx,
            message: message(0),
            reply: None,
            in_flight: None,
            task: Some(Box::new(move |_, _| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            })),
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker
        .submit(Job {
            vtable,
            ctx,
            message: message(73),
            reply: None,
            in_flight: None,
            task: None,
        })
        .unwrap();
    assert!(
        worker
            .submit(Job {
                vtable,
                ctx,
                message: message(74),
                reply: None,
                in_flight: None,
                task: None
            })
            .is_err()
    );
    let mut first = outbox.reserve().unwrap().send(message(41));
    let mut second = outbox.reserve().unwrap().send(message(42));
    assert!(matches!(
        first.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    release_tx.send(()).unwrap();
    assert_eq!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 41);
    assert_eq!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 42);
    assert_eq!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 73);
    assert!(worker.shutdown_within(Duration::from_secs(5)));
    assert_eq!(first.try_recv().unwrap(), vosu_status::VOSU_OK);
    assert_eq!(second.try_recv().unwrap(), vosu_status::VOSU_OK);
    assert_eq!(outbox.state.lock().unwrap().used, 0);
    let slots: Vec<_> = (0..MAX_OUTBOX_SLOTS)
        .map(|_| outbox.reserve().unwrap())
        .collect();
    assert!(outbox.reserve().is_err());
    drop(slots);
    assert!(outbox.reserve().is_ok());
    outbox.close();
    assert!(outbox.reserve().is_err());
}
