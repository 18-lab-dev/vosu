use super::*;

#[test]
fn helper_waits_past_legacy_two_second_window_for_durable_handoff_ack() {
    let mut fixture = Fixture::new(InstallPhase::Prepared, Some(b"old"));
    fs::remove_file(&fixture.record.backup).unwrap();
    fixture.record.helper = None;
    fixture.persist();
    let platform = Arc::new(NativePlatform::new());
    platform
        .live_owner_polls
        .store(usize::MAX, Ordering::Release);

    std::thread::scope(|scope| {
        let helper = scope.spawn({
            let platform = Arc::clone(&platform);
            let record_path = fixture.record_path.clone();
            let token = fixture.record.token.clone();
            move || {
                let result = run_helper(&record_path, &token, platform.as_ref());
                if let Err(error) = &result {
                    eprintln!("handoff helper exited: {error:?}");
                }
                result
            }
        });

        let load_deadline = Instant::now() + Duration::from_secs(5);
        while platform.live_owner_polls.load(Ordering::Acquire) == usize::MAX {
            assert!(
                Instant::now() < load_deadline,
                "helper did not load and authorize the initial handoff candidate"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        std::thread::sleep(Duration::from_millis(2_100));
        fixture.record.helper = Some(platform.identity.clone());
        fixture.record.phase = InstallPhase::HandedOff;
        fs::remove_file(&fixture.record_path).unwrap();
        let missing_poll = platform.live_owner_polls.load(Ordering::Acquire);
        let missing_deadline = Instant::now() + Duration::from_secs(5);
        while platform.live_owner_polls.load(Ordering::Acquire) == missing_poll {
            assert!(
                Instant::now() < missing_deadline,
                "helper did not retry the missing authorized handoff record"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        fs::write(&fixture.record_path, b"{").unwrap();
        let invalid_poll = platform.live_owner_polls.load(Ordering::Acquire);
        let invalid_deadline = Instant::now() + Duration::from_secs(5);
        while platform.live_owner_polls.load(Ordering::Acquire) == invalid_poll {
            assert!(
                Instant::now() < invalid_deadline,
                "helper did not retry the invalid authorized handoff record"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        fixture.persist();
        let ack = fixture.record.work.join("helper.ack");
        let deadline = Instant::now() + Duration::from_secs(5);
        let acknowledged = loop {
            if ack.is_file() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        platform.live_owner_polls.store(0, Ordering::Release);
        let helper_result = helper.join().unwrap();
        helper_result.unwrap();
        assert!(
            acknowledged,
            "helper did not durably acknowledge the committed owner identity"
        );
    });

    assert!(
        fixture.record.work.join("helper.ack").is_file(),
        "helper did not durably acknowledge the committed owner identity",
    );
    assert_eq!(
        load_record(&fixture.record_path).unwrap().phase,
        InstallPhase::Ready
    );
    fixture.assert_target(b"new");
    finish_ready_cleanup(
        &fixture.record_path,
        &fixture.record.token,
        platform.as_ref(),
    )
    .unwrap();
    fixture.assert_clean();
    assert!(
        recover_startup_update(fixture.root.path(), platform.as_ref())
            .unwrap()
            .is_none()
    );
    assert!(
        recover_startup_update(fixture.root.path(), platform.as_ref())
            .unwrap()
            .is_none()
    );
}

fn run_helper_with_authorized_record_replacement(replacement: &[u8]) -> (UpdateError, Duration) {
    let mut fixture = Fixture::new(InstallPhase::Prepared, Some(b"old"));
    fs::remove_file(&fixture.record.backup).unwrap();
    fixture.record.helper = None;
    fixture.persist();
    let platform = Arc::new(NativePlatform::new());
    platform
        .live_owner_polls
        .store(usize::MAX, Ordering::Release);

    std::thread::scope(|scope| {
        let helper = scope.spawn({
            let platform = Arc::clone(&platform);
            let record_path = fixture.record_path.clone();
            let token = fixture.record.token.clone();
            move || run_helper(&record_path, &token, platform.as_ref())
        });
        let load_deadline = Instant::now() + Duration::from_secs(5);
        while platform.live_owner_polls.load(Ordering::Acquire) == usize::MAX {
            assert!(
                Instant::now() < load_deadline,
                "helper did not load and authorize the initial handoff candidate"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        fs::write(&fixture.record_path, replacement).unwrap();
        let started = Instant::now();
        (helper.join().unwrap().unwrap_err(), started.elapsed())
    })
}

#[test]
fn helper_waits_past_legacy_two_second_window_for_durable_handoff_ack_rejects_non_eof_json() {
    let (error, elapsed) = run_helper_with_authorized_record_replacement(b"not json");
    assert!(matches!(error, UpdateError::Json(error) if !error.is_eof()));
    assert!(
        elapsed < Duration::from_secs(2),
        "non-EOF JSON was retried for {elapsed:?}"
    );
}

#[test]
fn helper_waits_past_legacy_two_second_window_for_durable_handoff_ack_bounds_persistent_eof() {
    let (error, elapsed) = run_helper_with_authorized_record_replacement(b"{");
    assert!(matches!(error, UpdateError::Json(error) if error.is_eof()));
    assert!(
        elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(5),
        "persistent EOF retry lasted {elapsed:?}"
    );
}

#[cfg(windows)]
fn run_helper_with_locked_authorized_record(persistent: bool) {
    use std::os::windows::fs::OpenOptionsExt;

    let mut fixture = Fixture::new(InstallPhase::Prepared, Some(b"old"));
    fs::remove_file(&fixture.record.backup).unwrap();
    fixture.record.helper = None;
    fixture.persist();
    let platform = Arc::new(NativePlatform::new());
    platform
        .live_owner_polls
        .store(usize::MAX, Ordering::Release);

    std::thread::scope(|scope| {
        let helper = scope.spawn({
            let platform = Arc::clone(&platform);
            let record_path = fixture.record_path.clone();
            let token = fixture.record.token.clone();
            move || run_helper(&record_path, &token, platform.as_ref())
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while platform.live_owner_polls.load(Ordering::Acquire) == usize::MAX {
            assert!(!helper.is_finished(), "helper exited before authorization");
            assert!(Instant::now() < deadline, "helper did not authorize record");
            std::thread::yield_now();
        }

        // An exclusive native handle deterministically produces ERROR_SHARING_VIOLATION
        // on reload; unlike CPU load alone, it does not depend on scanner timing.
        let lock = loop {
            match fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&fixture.record_path)
            {
                Ok(lock) => break lock,
                Err(error) if error.raw_os_error() == Some(32) => {
                    assert!(Instant::now() < deadline, "could not lock record");
                    std::thread::yield_now();
                }
                Err(error) => panic!("could not lock record: {error:?}"),
            }
        };
        let started = Instant::now();
        if persistent {
            let error = helper.join().unwrap().unwrap_err();
            assert!(matches!(error, UpdateError::Io(error) if error.raw_os_error() == Some(32)));
            assert!(
                started.elapsed() >= Duration::from_secs(2)
                    && started.elapsed() < Duration::from_secs(5),
                "persistent record lock must use the documented two-second retry window"
            );
        } else {
            let before = platform.live_owner_polls.load(Ordering::Acquire);
            // One poll might already have been in flight when the handle was
            // acquired. Two polls prove that a failed reload checked its owner.
            while platform.live_owner_polls.load(Ordering::Acquire) > before - 2
                && !helper.is_finished()
                && started.elapsed() < Duration::from_secs(2)
            {
                std::thread::yield_now();
            }
            let retried = platform.live_owner_polls.load(Ordering::Acquire) <= before - 2;
            platform.live_owner_polls.store(0, Ordering::Release);
            let result = helper.join().unwrap();
            drop(lock);
            assert!(
                retried,
                "helper stopped instead of retrying locked record: {result:?}"
            );
            assert!(matches!(result, Err(UpdateError::HelperHandoff)));
        }
    });
    fixture.assert_target(b"old");
}

#[cfg(windows)]
#[test]
fn helper_retries_locked_authorized_record_while_owner_lives() {
    run_helper_with_locked_authorized_record(false);
}

#[cfg(windows)]
#[test]
fn helper_bounds_persistently_locked_authorized_record() {
    run_helper_with_locked_authorized_record(true);
}

#[cfg(windows)]
#[test]
fn helper_does_not_retry_locked_record_without_prior_authorization() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = Fixture::new(InstallPhase::Prepared, Some(b"old"));
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&fixture.record_path)
        .unwrap();
    let started = Instant::now();
    let error = run_helper(
        &fixture.record_path,
        &fixture.record.token,
        &NativePlatform::new(),
    )
    .unwrap_err();
    assert!(matches!(error, UpdateError::Io(error) if error.raw_os_error() == Some(32)));
    assert!(started.elapsed() < Duration::from_secs(2));
    fixture.assert_target(b"old");
}
