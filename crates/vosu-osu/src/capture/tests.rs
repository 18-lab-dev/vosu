use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn memory_permission_denial_keeps_typed_identity_in_state_worker_errors() {
    let denied = StateWorkerError::from(vosu_plugin_api::memory::MemoryError::PermissionDenied {
        address: 0x1234,
        len: 16,
        message: "operation not permitted".into(),
    });
    assert!(matches!(
        denied,
        StateWorkerError::MemoryPermissionDenied {
            address: 0x1234,
            len: 16,
            ..
        }
    ));

    let fault = StateWorkerError::from(vosu_plugin_api::memory::MemoryError::Read {
        address: 0x1234,
        len: 16,
        message: "bad address".into(),
    });
    assert!(matches!(fault, StateWorkerError::Capture(_)));
}

#[tokio::test]
async fn permission_denial_logs_once_and_stops_initialization_without_retrying() {
    let cancel = CancellationToken::new();
    let attempts = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);

    let worker = tokio::time::timeout(
        Duration::from_millis(50),
        initialize_state_worker::<(), _, _, _>(
            &cancel,
            Duration::from_secs(60),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err(StateWorkerError::MemoryPermissionDenied {
                    address: 0x4000,
                    len: 8,
                    message: "permission denied".into(),
                }))
            },
            |_| {
                errors.fetch_add(1, Ordering::SeqCst);
            },
        ),
    )
    .await
    .expect("permission denial must terminate immediately");

    assert!(worker.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(errors.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ordinary_initialization_errors_keep_retrying_until_cancelled() {
    let cancel = CancellationToken::new();
    let attempts = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);

    let worker = initialize_state_worker::<(), _, _, _>(
        &cancel,
        Duration::ZERO,
        || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == 2 {
                cancel.cancel();
            }
            std::future::ready(Err(StateWorkerError::Capture("transient".into())))
        },
        |_| {
            errors.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    assert!(worker.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(errors.load(Ordering::SeqCst), 0);
}
