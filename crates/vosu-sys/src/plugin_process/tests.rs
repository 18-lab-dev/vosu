use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::SharedRegion;

const BOUND: Duration = Duration::from_secs(10);

/// Runs `body` on a helper thread and fails (rather than hangs) if it does
/// not finish within `BOUND`.
#[cfg(target_os = "linux")]
fn bounded<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(body());
    });
    receiver
        .recv_timeout(BOUND)
        .expect("operation blocked past the test bound")
}

#[test]
fn heap_region_round_trips_every_offset_and_length_across_word_edges() {
    let region = SharedRegion::heap(64).unwrap();
    for offset in 0..24 {
        for len in 0..=24 {
            let fill: Vec<u8> = (0..64).map(|index| 0xa0 ^ index as u8).collect();
            region.write(0, &fill).unwrap();
            let src: Vec<u8> = (0..len).map(|index| index as u8 + 1).collect();
            region.write(offset, &src).unwrap();
            let mut whole = vec![0_u8; 64];
            region.read(0, &mut whole).unwrap();
            assert_eq!(&whole[offset..offset + len], &src[..]);
            assert_eq!(&whole[..offset], &fill[..offset], "bytes before preserved");
            assert_eq!(
                &whole[offset + len..],
                &fill[offset + len..],
                "bytes after preserved"
            );
            let mut back = vec![0_u8; len];
            region.read(offset, &mut back).unwrap();
            assert_eq!(back, src);
        }
    }
}

#[test]
fn region_refuses_out_of_bounds_overflowing_and_misaligned_accesses() {
    let region = SharedRegion::heap(32).unwrap();
    assert!(region.read(30, &mut [0; 3]).is_err());
    assert!(region.write(usize::MAX, &[1]).is_err());
    assert!(region.read(usize::MAX - 1, &mut [0; 4]).is_err());
    assert!(
        region.read(32, &mut []).is_ok(),
        "an empty access at the end is in bounds"
    );
    assert!(region.word(4).is_err());
    assert!(region.word(32).is_err());
    assert!(region.word(usize::MAX - 7).is_err());
    region.word(24).unwrap().store(7, Ordering::SeqCst);
    let mut bytes = [0_u8; 8];
    region.read(24, &mut bytes).unwrap();
    assert_eq!(u64::from_ne_bytes(bytes), 7);
    assert!(SharedRegion::heap(0).is_err());
    assert!(SharedRegion::heap(12).is_err());
}

#[cfg(any(target_os = "linux", windows))]
mod native {
    use super::*;
    use crate::plugin_process::{Doorbell, RingObjects};

    const LEN: usize = 64 * 1024;

    #[cfg(target_os = "linux")]
    fn adopt_copy(objects: &RingObjects, len: usize) -> std::io::Result<RingObjects> {
        RingObjects::from_inherited(
            objects.mapping_fd().try_clone_to_owned()?,
            objects.doorbell_fd().try_clone_to_owned()?,
            len,
        )
    }

    #[cfg(windows)]
    fn adopt_copy(objects: &RingObjects, len: usize) -> std::io::Result<RingObjects> {
        RingObjects::from_inherited(
            objects.mapping_handle().try_clone_to_owned()?,
            objects.doorbell_handle().try_clone_to_owned()?,
            len,
        )
    }

    #[test]
    fn two_views_of_one_ring_object_share_bytes_and_doorbell() {
        let parent = RingObjects::create(LEN).unwrap();
        let child = adopt_copy(&parent, LEN).unwrap();
        assert_eq!(child.region().len(), LEN);
        let payload: Vec<u8> = (0..1000).map(|index| (index * 7) as u8).collect();
        parent.region().write(LEN - 1003, &payload).unwrap();
        let mut seen = vec![0; payload.len()];
        child.region().read(LEN - 1003, &mut seen).unwrap();
        assert_eq!(seen, payload);
        child.region().word(8).unwrap().store(42, Ordering::SeqCst);
        assert_eq!(parent.region().word(8).unwrap().load(Ordering::SeqCst), 42);

        parent.doorbell().ring().unwrap();
        assert!(child.doorbell().wait(Some(BOUND)).unwrap());
    }

    #[test]
    fn adopting_a_mapping_of_the_wrong_size_is_refused() {
        let parent = RingObjects::create(LEN).unwrap();
        assert!(
            adopt_copy(&parent, LEN + 4096).is_err(),
            "larger than the object"
        );
        #[cfg(target_os = "linux")]
        assert!(
            adopt_copy(&parent, LEN - 8).is_err(),
            "Linux requires the exact size"
        );
    }

    #[test]
    fn doorbell_wakes_a_blocked_waiter_and_coalesces_stale_rings() {
        let bell = std::sync::Arc::new(Doorbell::create().unwrap());
        let start = Instant::now();
        assert!(!bell.wait(Some(Duration::from_millis(30))).unwrap());
        assert!(
            start.elapsed() >= Duration::from_millis(30),
            "timeout honored"
        );
        assert!(!bell.consume().unwrap(), "nothing pending");

        // Several rings before a wait are one pending wake, not a queue.
        bell.ring().unwrap();
        bell.ring().unwrap();
        bell.ring().unwrap();
        assert!(bell.wait(Some(Duration::ZERO)).unwrap());
        assert!(!bell.consume().unwrap(), "stale rings coalesced");

        let waiter = {
            let bell = bell.clone();
            std::thread::spawn(move || bell.wait(Some(BOUND)).unwrap())
        };
        // A ring issued before or after the waiter parks is never lost.
        bell.ring().unwrap();
        assert!(waiter.join().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hostile_peer_cannot_make_doorbell_operations_block() {
        use std::os::fd::AsRawFd;

        let parent = RingObjects::create(LEN).unwrap();
        let peer = adopt_copy(&parent, LEN).unwrap();
        let fd = peer.doorbell_fd().as_raw_fd();
        // The peer shares the open file description: clear O_NONBLOCK there
        // and saturate the counter, which would block a plain `write(1)`.
        // SAFETY: plain fcntl/write calls on a live descriptor this test owns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK), 0);
            let max = (u64::MAX - 1).to_ne_bytes();
            assert_eq!(libc::write(fd, max.as_ptr().cast(), 8), 8);
        }
        let parent = bounded(move || {
            parent.doorbell().ring().unwrap();
            parent
        });
        let parent = bounded(move || {
            assert!(
                parent.doorbell().consume().unwrap(),
                "saturated counter is a pending wake"
            );
            assert!(
                !parent.doorbell().consume().unwrap(),
                "empty blocking eventfd never blocks"
            );
            assert!(
                !parent
                    .doorbell()
                    .wait(Some(Duration::from_millis(20)))
                    .unwrap()
            );
            parent
        });
        drop(parent);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unsealed_memfd_and_non_eventfd_doorbell_are_refused() {
        use std::os::fd::{FromRawFd, OwnedFd};

        // SAFETY: creates and sizes a memfd this test owns; no seals added.
        let unsealed = unsafe {
            let raw = libc::memfd_create(c"unsealed".as_ptr(), libc::MFD_CLOEXEC);
            assert!(raw >= 0);
            assert_eq!(libc::ftruncate(raw, LEN as libc::off_t), 0);
            OwnedFd::from_raw_fd(raw)
        };
        let bell = Doorbell::create().unwrap();
        let error =
            RingObjects::from_inherited(unsealed, bell.as_fd().try_clone_to_owned().unwrap(), LEN)
                .unwrap_err();
        assert!(error.to_string().contains("not sealed"), "{error}");

        let parent = RingObjects::create(LEN).unwrap();
        let not_eventfd = parent.mapping_fd().try_clone_to_owned().unwrap();
        let error = RingObjects::from_inherited(
            parent.mapping_fd().try_clone_to_owned().unwrap(),
            not_eventfd,
            LEN,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not an eventfd"), "{error}");
    }
}
