use super::commit::{KEY, field, grant, store};
use crate::state::{Pointer, StateValue};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;
use vosu_plugin_api::PublishValueRef;

#[test]
fn concurrent_batches_never_interleave() {
    let grant = Arc::new(grant("test.commit-concurrency", "", ""));
    let store = Arc::new(store(&grant));
    store
        .commit(
            &grant,
            KEY,
            ["/a", "/b", "/c", "/d"]
                .map(|p| field("test.commit-concurrency", p, PublishValueRef::I64(0)))
                .into_iter(),
        )
        .unwrap();
    let finished = Arc::new(AtomicUsize::new(0));
    let (done, received) = mpsc::sync_channel(3);
    let mut threads = Vec::new();
    let mut starts = Vec::new();
    for pair in [["/a", "/b"], ["/c", "/d"]] {
        let (start, begun) = mpsc::sync_channel(1);
        starts.push(start);
        let (store, grant, finished, done) =
            (store.clone(), grant.clone(), finished.clone(), done.clone());
        threads.push(std::thread::spawn(move || {
            begun.recv_timeout(Duration::from_secs(10)).unwrap();
            for i in 1..=20_000 {
                store
                    .commit(
                        &grant,
                        KEY,
                        pair.map(|p| field("test.commit-concurrency", p, PublishValueRef::I64(i)))
                            .into_iter(),
                    )
                    .unwrap();
            }
            finished.fetch_add(1, Ordering::Release);
            done.send(()).unwrap();
        }));
    }
    let (reader_store, reader_finished, reader_done) =
        (store.clone(), finished.clone(), done.clone());
    let (ready, ready_rx) = mpsc::sync_channel(1);
    threads.push(std::thread::spawn(move || {
        let pointers = ["/a", "/b", "/c", "/d"].map(|p| Pointer::parse(p).unwrap());
        let mut revision = 0;
        ready.send(()).unwrap();
        let deadline = std::time::Instant::now();
        loop {
            assert!(
                deadline.elapsed() < Duration::from_secs(30),
                "state reader did not observe both writers finishing within 30s"
            );
            let state = reader_store.current(KEY).unwrap();
            assert!(state.revision >= revision);
            revision = state.revision;
            assert_eq!(
                state.get("test.commit-concurrency", &pointers[0]),
                state.get("test.commit-concurrency", &pointers[1])
            );
            assert_eq!(
                state.get("test.commit-concurrency", &pointers[2]),
                state.get("test.commit-concurrency", &pointers[3])
            );
            if reader_finished.load(Ordering::Acquire) == 2 {
                break;
            }
        }
        reader_done.send(()).unwrap();
    }));
    ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    for start in starts {
        start.send(()).unwrap();
    }
    drop(done);
    for _ in 0..3 {
        received.recv_timeout(Duration::from_secs(30)).unwrap();
    }
    for thread in threads {
        vosu_proptest::wait::JoinBounded::join_bounded(thread).unwrap();
    }
    let final_state = store.current(KEY).unwrap();
    assert_eq!(final_state.revision, 40_001);
    for pointer in ["/a", "/b", "/c", "/d"] {
        assert_eq!(
            final_state.get("test.commit-concurrency", &Pointer::parse(pointer).unwrap()),
            Some(&StateValue::I64(20_000))
        );
    }
}
