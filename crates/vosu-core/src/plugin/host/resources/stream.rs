use super::super::resource_requests::MAX_PENDING_RESOURCE_REQUESTS;
use super::byte_tests::BytesTest;
use super::*;
use crate::plugin::StreamLease;
use std::future::{Future, poll_fn};
use std::task::Poll;

#[test]
fn independent_streams_use_offsets_short_eof_and_exactly_once_close() {
    let fixture = BytesTest::new("ranges", 99301);
    let (first, second) = fixture
        .runtime
        .block_on(async { (fixture.lease().await, fixture.lease().await) });
    fixture.chunk(b"range");
    assert_eq!(
        fixture.runtime.block_on(first.read(7, 5)).unwrap(),
        b"range"
    );
    assert_eq!(
        fixture.reads.recv_timeout(Duration::from_secs(2)).unwrap(),
        (101, 7, 5)
    );
    fixture.chunk(b"xy");
    assert_eq!(fixture.runtime.block_on(first.read(2, 6)).unwrap(), b"xy");
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        101
    );
    assert_eq!(
        fixture.runtime.block_on(first.read(0, 1)),
        Err(ResourceRequestError::Cancelled)
    );
    assert!(second.is_current());
    fixture.chunk(b"123");
    assert_eq!(
        fixture.runtime.block_on(second.read(14, 8)).unwrap(),
        b"123"
    );
    assert!(fixture.closed.try_recv().is_err());
    first.close();
    drop(first);
    second.close();
    second.close();
    drop(second);
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        102
    );
    let third = fixture.runtime.block_on(fixture.lease());
    assert!(third.is_current());
    third.close();
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        103
    );
    assert!(fixture.closed.try_recv().is_err());
}

#[test]
fn overflow_and_oversized_requests_never_dispatch() {
    let fixture = BytesTest::new("bounds", 99302);
    let lease = fixture.runtime.block_on(fixture.lease());
    assert_eq!(
        fixture.runtime.block_on(lease.read(u64::MAX, 2)),
        Err(ResourceRequestError::InvalidRequest)
    );
    assert_eq!(
        fixture
            .runtime
            .block_on(lease.read(0, vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES + 1)),
        Err(ResourceRequestError::InvalidRequest)
    );
    assert_eq!(
        fixture.runtime.block_on(lease.read(18, 1)),
        Err(ResourceRequestError::InvalidRequest)
    );
    assert!(fixture.reads.try_recv().is_err());
    assert!(lease.is_current());
    lease.close();
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        101
    );
}

#[test]
fn dropping_read_future_timeout_detach_and_body_drop_release_leases() {
    let fixture = BytesTest::new("cancel", 99303);
    let lease = fixture.runtime.block_on(fixture.lease());
    fixture.runtime.block_on(async {
        let future = lease.read(3, 4);
        tokio::pin!(future);
        poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    });
    assert!(!lease.is_current());
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        101
    );
    assert_eq!(
        fixture.host.resource_state.lock().unwrap().requests.len(),
        0
    );
    let timed = fixture.runtime.block_on(fixture.lease());
    assert_eq!(
        fixture.runtime.block_on(timed.read(5, 2)),
        Err(ResourceRequestError::TimedOut)
    );
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        102
    );
    let detached = fixture.runtime.block_on(fixture.lease());
    fixture.host.process_exited(99303);
    assert!(!detached.is_current());
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        103
    );
    drop((lease, timed, detached));
    assert!(fixture.closed.try_recv().is_err());
}

#[test]
fn body_drop_and_duplicate_provider_stream_do_not_close_another_request() {
    let fixture = BytesTest::new("duplicates", 99304);
    let first = fixture.runtime.block_on(fixture.lease());
    fixture.fixture.next.store(101, Ordering::SeqCst);
    let duplicate = fixture.runtime.block_on(
        fixture
            .host
            .resource_requests(Duration::from_secs(2))
            .request(
                fixture.owner,
                ResourceRequest::Resolve(vosu_plugin_api::ResourceResolve {
                    handle: fixture.owner.handle,
                    kind: "file".into(),
                    key: vosu_plugin_api::CborValue::Null,
                }),
            ),
    );
    assert_eq!(duplicate, Err(ResourceRequestError::InvalidReply));
    assert!(first.is_current());
    assert!(fixture.closed.try_recv().is_err());
    drop(first);
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        101
    );
    assert!(fixture.closed.try_recv().is_err());
}

#[test]
fn instance_loss_revokes_live_stream() {
    let fixture = BytesTest::new("loss", 99305);
    let lease: StreamLease = fixture.runtime.block_on(fixture.lease());
    fixture.host.destroy_all(Duration::from_secs(2));
    assert!(!lease.is_current());
    assert_eq!(
        fixture.runtime.block_on(lease.read(0, 1)),
        Err(ResourceRequestError::Cancelled)
    );
}

#[test]
fn overlong_chunk_is_rejected_and_closes_its_lease() {
    let fixture = BytesTest::new("overlong", 99306);
    let lease = fixture.runtime.block_on(fixture.lease());
    fixture.chunk(b"too-many");
    assert_eq!(
        fixture.runtime.block_on(lease.read(0, 3)),
        Err(ResourceRequestError::InvalidReply)
    );
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        101
    );
    assert!(!lease.is_current());
    assert!(fixture.closed.try_recv().is_err());
}

#[test]
fn live_stream_capacity_rejects_and_closes_excess_without_losing_live_leases() {
    let fixture = BytesTest::new("capacity", 99307);
    let leases: Vec<_> = fixture.runtime.block_on(async {
        let mut leases = Vec::new();
        for _ in 0..MAX_PENDING_RESOURCE_REQUESTS {
            leases.push(fixture.lease().await);
        }
        leases
    });
    let excess = fixture.runtime.block_on(
        fixture
            .host
            .resource_requests(Duration::from_secs(2))
            .request(
                fixture.owner,
                ResourceRequest::Resolve(vosu_plugin_api::ResourceResolve {
                    handle: fixture.owner.handle,
                    kind: "file".into(),
                    key: vosu_plugin_api::CborValue::Null,
                }),
            ),
    );
    assert_eq!(excess, Err(ResourceRequestError::Busy));
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        165
    );
    assert!(leases.iter().all(StreamLease::is_current));
    drop(leases);
    let mut closed = std::collections::HashSet::new();
    for _ in 0..MAX_PENDING_RESOURCE_REQUESTS {
        assert!(closed.insert(fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap()));
    }
    assert_eq!(closed.len(), 64);
    assert!(fixture.closed.try_recv().is_err());
    let fresh = fixture.runtime.block_on(fixture.lease());
    fresh.close();
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        166
    );
}

#[test]
fn zero_chunk_is_eof_before_advertised_size() {
    let fixture = BytesTest::new("zero", 99308);
    let lease = fixture.runtime.block_on(fixture.lease());
    fixture.chunk(b"");
    assert_eq!(fixture.runtime.block_on(lease.read(4, 7)).unwrap(), b"");
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        101
    );
    assert!(!lease.is_current());
}
