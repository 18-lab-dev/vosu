//! Same image and forge, fixture CA absent from the OS trust store.

use tokio_util::sync::CancellationToken;
use vosu_core::catalog::sources::SourceFailure;
use vosu_net::StreamingClient;

use super::start;
use crate::{
    fixture::{PRIVATE_REPO, assert_os_store, run},
    recorder::{UNKNOWN_ISSUER, assert_certificate_error},
};

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn missing_ca_rejects_the_real_forge_before_any_request_or_credential() {
    run(|fixture| async move {
        assert_os_store(&fixture, false);
        let forge = start(&fixture).await;
        let host = fixture.forge_host.as_str();

        assert_certificate_error(
            StreamingClient::production()
                .request_scoped(
                    &format!("https://{host}/api/v1/version"),
                    "vosu",
                    &forge.probe_policy(host, true),
                    None,
                    &CancellationToken::new(),
                )
                .await,
            UNKNOWN_ISSUER,
        );

        // The whole fetch path with a saved token and the exact flag: probes
        // fail at certificate verification, so no request (and no
        // credential) ever reaches Forgejo.
        let source = forge.repository(PRIVATE_REPO);
        forge.configure(&source, Some(forge.token())).await;
        forge.allow_private(&source);
        let snapshot = forge.refresh().await;
        assert_eq!(
            snapshot.sources()[0].catalog().unwrap_err(),
            SourceFailure::UndetectedForge
        );

        let handshakes = forge.recorder.handshakes();
        assert!(
            handshakes.len() >= 3
                && handshakes
                    .iter()
                    .all(|handshake| handshake.sni.as_deref() == Some(host)
                        && handshake
                            .error
                            .as_deref()
                            .is_some_and(|e| e.contains("UnknownCA"))),
            "every handshake must fail on the unknown issuer: {handshakes:?}"
        );
        assert!(forge.recorder.received().is_empty());
        forge.shutdown().await;
    });
}
