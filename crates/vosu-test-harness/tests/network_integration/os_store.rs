//! OS trust-store evidence without a forge: the production TLS builder
//! against a loopback recorder presenting the fixture leaf. This is the part
//! that also runs natively on Windows (`--windows-vm`), where the runner adds
//! and removes the CA in the machine `Root` store around the two phases.
//!
//! Names resolve through an exact loopback map (no container DNS on Windows),
//! so the exact source private-network permission is what admits them; the
//! certificate decision is entirely the platform verifier's.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use tokio_util::sync::CancellationToken;
use url::Url;
use vosu_net::{
    Deadlines, DestinationPolicy, DownloadLimits, NetworkError, NetworkResponse, StreamingClient,
    streaming::{RequestPolicy, RequestPurpose, SourcePolicy},
};

use crate::{
    fixture::{Fixture, assert_os_store, run},
    recorder::{
        Handshake, LoopbackNames, Role, Sentinels, TlsRecorder, UNKNOWN_ISSUER, WRONG_NAME,
        assert_certificate_error,
    },
};

struct Probe {
    recorder: TlsRecorder,
    client: StreamingClient,
    port: u16,
}

async fn start(fixture: &Fixture) -> Probe {
    let recorder = TlsRecorder::start(
        fixture,
        Sentinels::default(),
        &[(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), Role::Fixed)],
    )
    .await;
    let port = recorder.address(0).port();
    let client = StreamingClient::new(
        Arc::new(LoopbackNames(vec![
            fixture.forge_host.clone(),
            fixture.wrong_host.clone(),
        ])),
        DestinationPolicy::PublicOnly,
        Deadlines {
            connect: Duration::from_secs(5),
            idle: Duration::from_secs(5),
            total: Duration::from_secs(10),
        },
        DownloadLimits::default(),
    );
    Probe {
        recorder,
        client,
        port,
    }
}

impl Probe {
    async fn get(&self, host: &str) -> Result<NetworkResponse, NetworkError> {
        let origin = Url::parse(&format!("https://{host}:{}/", self.port)).unwrap();
        let policy = RequestPolicy::source(
            RequestPurpose::Probe,
            SourcePolicy::repository(&origin, true).unwrap(),
        )
        .unwrap();
        self.client
            .request_scoped(
                origin.join("x1/ok").unwrap().as_str(),
                "vosu",
                &policy,
                None,
                &CancellationToken::new(),
            )
            .await
    }

    async fn tls_failure(&self, host: &str, alert: &str) {
        let handshakes = self.recorder.handshakes().await;
        assert!(
            !handshakes.is_empty()
                && handshakes.iter().all(|handshake| {
                    handshake.sni.as_deref() == Some(host)
                        && handshake
                            .error
                            .as_deref()
                            .is_some_and(|error| error.contains(alert))
                }),
            "{host}: expected only failed handshakes carrying {alert}: {handshakes:?}"
        );
        assert!(
            self.recorder.received().is_empty(),
            "{host}: no HTTP request may follow a rejected certificate"
        );
    }
}

mod trusted {
    use super::*;

    #[test]
    #[ignore = "D15: needs the internal CA in the OS store; run python scripts/test-catalog-forge.py"]
    fn installed_ca_trusts_the_exact_fixture_name_only() {
        run(|fixture| async move {
            assert_os_store(&fixture, true);
            let probe = start(&fixture).await;

            let response = probe
                .get(&fixture.forge_host)
                .await
                .expect("installed internal CA is trusted through the OS store");
            assert_eq!(response.status, 200);
            assert_eq!(response.body.as_ref(), b"vosu-x1 os store ok");
            assert_eq!(
                probe.recorder.handshakes().await,
                [Handshake {
                    port: probe.port,
                    sni: Some(fixture.forge_host.clone()),
                    error: None,
                }]
            );

            probe.recorder.clear();
            assert_certificate_error(probe.get(&fixture.wrong_host).await, WRONG_NAME);
            probe
                .tls_failure(&fixture.wrong_host, "BadCertificate")
                .await;
            probe.recorder.shutdown().await;
        });
    }
}

mod untrusted {
    use super::*;

    #[test]
    #[ignore = "D15: needs the CA-free OS store phase; run python scripts/test-catalog-forge.py"]
    fn missing_ca_rejects_the_fixture_leaf_before_any_request() {
        run(|fixture| async move {
            assert_os_store(&fixture, false);
            let probe = start(&fixture).await;
            assert_certificate_error(probe.get(&fixture.forge_host).await, UNKNOWN_ISSUER);
            probe.tls_failure(&fixture.forge_host, "UnknownCA").await;
            probe.recorder.shutdown().await;
        });
    }
}
