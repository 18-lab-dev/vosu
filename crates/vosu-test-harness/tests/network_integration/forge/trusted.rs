//! Fixture CA installed in the container's OS trust store.

use tokio_util::sync::CancellationToken;
use url::Url;
use vosu_core::catalog::sources::SourceFailure;
use vosu_net::{
    NetworkError, StreamingClient,
    streaming::{RequestCredential, RequestPolicy, RequestPurpose},
};

use super::{
    ANY, Forge, JSON, OCTET, assert_no_secret_in, assert_secrets_only_in_credential_store, start,
};
use crate::{
    fixture::{
        CROSS_PORT_REPO, CROSS_PORT_TAG, FOREIGN_ASSET_REPO, FOREIGN_REDIRECT_REPO,
        FOREIGN_REDIRECT_TAG, ICON_ASSET, OWNER, PACKAGE_ASSET, PRIVATE_REPO, PRIVATE_TAG,
        SAME_ORIGIN_REPO, SAME_ORIGIN_TAG, TOOL_ID, TOOL_VERSION, assert_os_store, run,
    },
    recorder::{Auth, Handshake, WRONG_NAME, assert_certificate_error},
};

const CATALOG: &str = "catalog-v2.json";
const HOP: &str = "hop.json";

fn private_catalog_path() -> String {
    Forge::download_path(PRIVATE_REPO, PRIVATE_TAG, CATALOG)
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn production_client_trusts_the_installed_ca_for_the_exact_forge_name_only() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        // The unmodified production constructor: system resolver, PublicOnly
        // destinations and the platform verifier.
        let client = StreamingClient::production();
        let cancel = CancellationToken::new();
        let version = format!("https://{}/api/v1/version", forge.host());

        let denied = client
            .request_scoped(
                &version,
                "vosu",
                &forge.probe_policy(forge.host(), false),
                None,
                &cancel,
            )
            .await;
        assert!(
            matches!(denied, Err(NetworkError::Destination(_))),
            "private fixture address needs the exact source flag: {denied:?}"
        );
        assert!(forge.recorder.handshakes().await.is_empty());

        let response = client
            .request_scoped(
                &version,
                "vosu",
                &forge.probe_policy(forge.host(), true),
                None,
                &cancel,
            )
            .await
            .expect("installed internal CA is trusted through the OS store");
        assert_eq!(response.status, 200);
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["version"], fixture.forge().version.as_str());
        assert_eq!(
            forge.recorder.handshakes().await,
            [Handshake {
                port: 443,
                sni: Some(fixture.forge_host.clone()),
                error: None,
            }]
        );
        assert_eq!(
            forge.recorder.received(),
            [forge.received("/api/v1/version", Some(ANY), Auth::None, 200)]
        );

        forge.recorder.clear();
        let wrong = &fixture.wrong_host;
        assert_certificate_error(
            client
                .request_scoped(
                    &format!("https://{wrong}/api/v1/version"),
                    "vosu",
                    &forge.probe_policy(wrong, true),
                    None,
                    &cancel,
                )
                .await,
            WRONG_NAME,
        );
        let handshakes = forge.recorder.handshakes().await;
        assert!(
            !handshakes.is_empty()
                && handshakes
                    .iter()
                    .all(|handshake| handshake.sni.as_deref() == Some(wrong)
                        && handshake
                            .error
                            .as_deref()
                            .is_some_and(|e| e.contains("BadCertificate"))),
            "wrong hostname must fail certificate verification: {handshakes:?}"
        );
        assert!(forge.recorder.received().is_empty());
        forge.shutdown().await;
    });
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn private_source_is_refused_by_default_then_fetched_with_its_exact_flag() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        let source = forge.repository(PRIVATE_REPO);
        forge.configure(&source, Some(forge.token())).await;

        let refused = forge.refresh().await;
        assert_eq!(
            refused.sources()[0].catalog().unwrap_err(),
            SourceFailure::UndetectedForge
        );
        assert!(
            forge.recorder.handshakes().await.is_empty() && forge.recorder.received().is_empty(),
            "without the exact flag nothing may connect to the private forge"
        );
        forge.assert_private_answers(forge.host(), 0);

        forge.allow_private(&source);
        let snapshot = forge.refresh().await;
        let catalog = snapshot.sources()[0]
            .catalog()
            .expect("private catalog through the exact-host exception");
        assert_eq!(catalog.tools.len(), 1);
        assert_eq!(catalog.tools[0].id, TOOL_ID);
        assert_eq!(catalog.tools[0].version, TOOL_VERSION);
        assert_eq!(snapshot.candidates().count(), 1);
        // The real release JSON names the asset by its releases/download path
        // (the upload response's /attachments/ form is never used), and the
        // forge received the exact token on metadata and asset requests only.
        let mut expected = forge.probes().to_vec();
        expected.push(forge.metadata(PRIVATE_REPO, Auth::Token, 200));
        expected.push(forge.received(&private_catalog_path(), Some(OCTET), Auth::Token, 200));
        assert_eq!(forge.recorder.received(), expected);

        let token = fixture.forge().secrets.token.as_str();
        assert_secrets_only_in_credential_store(forge.root.path(), &[token]);
        assert_no_secret_in(
            &[
                format!("{:?}", snapshot.sources()[0].source()),
                format!("{catalog:?}"),
                format!("{:?}", forge.service.credential_presence(&source)),
                format!("{}", refused.sources()[0].catalog().unwrap_err()),
                format!("{:?}", forge.recorder.received()),
            ],
            &[token],
        );
        forge.shutdown().await;
    });
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn static_basic_credential_is_received_and_kept_out_of_storage() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        let source = format!("https://{}{}", forge.host(), private_catalog_path());
        forge.configure(&source, Some(forge.basic())).await;
        forge.allow_private(&source);

        let snapshot = forge.refresh().await;
        let catalog = snapshot.sources()[0]
            .catalog()
            .expect("static private catalog with Basic");
        assert_eq!(catalog.tools[0].id, TOOL_ID);
        // Static sources never probe and send only Basic to the literal URL.
        assert_eq!(
            forge.recorder.received(),
            [forge.received(&private_catalog_path(), Some(OCTET), Auth::Basic, 200)]
        );
        let secrets = &fixture.forge().secrets;
        let sentinels = [secrets.username.as_str(), secrets.password.as_str()];
        assert_secrets_only_in_credential_store(forge.root.path(), &sentinels);
        assert_no_secret_in(
            &[
                format!("{:?}", snapshot.sources()[0].source()),
                format!("{catalog:?}"),
                format!("{:?}", forge.service.credential_presence(&source)),
            ],
            &sentinels,
        );
        forge.shutdown().await;
    });
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn real_forge_same_origin_redirect_keeps_the_credential() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        let source = forge.repository(SAME_ORIGIN_REPO);
        forge.configure(&source, Some(forge.token())).await;
        forge.allow_private(&source);

        let snapshot = forge.refresh().await;
        assert_eq!(
            snapshot.sources()[0]
                .catalog()
                .expect("same-origin redirect")
                .tools[0]
                .id,
            TOOL_ID
        );
        // Forgejo itself answers the external asset with 303 to another path
        // on the same origin; the credential stays on that one origin.
        let mut expected = forge.probes().to_vec();
        expected.push(forge.metadata(SAME_ORIGIN_REPO, Auth::Token, 200));
        expected.push(forge.received(
            &Forge::download_path(SAME_ORIGIN_REPO, SAME_ORIGIN_TAG, HOP),
            Some(OCTET),
            Auth::Token,
            303,
        ));
        expected.push(forge.received(&private_catalog_path(), Some(OCTET), Auth::Token, 200));
        assert_eq!(forge.recorder.received(), expected);
        forge.shutdown().await;
    });
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn real_forge_cross_port_redirect_strips_the_credential_for_the_whole_chain() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        let source = forge.repository(CROSS_PORT_REPO);
        forge.configure(&source, Some(forge.token())).await;
        forge.allow_private(&source);

        let snapshot = forge.refresh().await;
        assert_eq!(
            snapshot.sources()[0].catalog().unwrap_err(),
            SourceFailure::Unavailable
        );
        // A (forge:443, token) -303 by Forgejo-> B (forge:8443) -302-> A. The
        // same host stays private-eligible on another port, but the credential
        // origin changed at B and never returns: Forgejo refuses the final
        // private request with 404 because it arrived without the token.
        let cross_port = fixture.forge().cross_port;
        let mut expected = forge.probes().to_vec();
        expected.push(forge.metadata(CROSS_PORT_REPO, Auth::Token, 200));
        expected.push(forge.received(
            &Forge::download_path(CROSS_PORT_REPO, CROSS_PORT_TAG, HOP),
            Some(OCTET),
            Auth::Token,
            303,
        ));
        expected.push(forge.received_on(
            cross_port,
            &format!("/x1/return{}", private_catalog_path()),
            Some(OCTET),
            Auth::None,
            302,
        ));
        expected.push(forge.received(&private_catalog_path(), Some(OCTET), Auth::None, 404));
        assert_eq!(forge.recorder.received(), expected);
        forge.shutdown().await;
    });
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn foreign_hosts_are_denied_at_redirect_and_initial_asset() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        let mirror = fixture.mirror_host.as_str();

        let source = forge.repository(FOREIGN_REDIRECT_REPO);
        forge.configure(&source, Some(forge.token())).await;
        forge.allow_private(&source);
        let lookups = forge.dns.lookups().len();
        let snapshot = forge.refresh().await;
        assert_eq!(
            snapshot.sources()[0].catalog().unwrap_err(),
            SourceFailure::Unavailable
        );
        let mut expected = forge.probes().to_vec();
        expected.push(forge.metadata(FOREIGN_REDIRECT_REPO, Auth::Token, 200));
        expected.push(forge.received(
            &Forge::download_path(FOREIGN_REDIRECT_REPO, FOREIGN_REDIRECT_TAG, HOP),
            Some(OCTET),
            Auth::Token,
            303,
        ));
        assert_eq!(forge.recorder.received(), expected);
        // The redirect target was resolved and refused by address policy: the
        // forge host's private permission does not extend to another host,
        // and the (validly certified) mirror never saw a ClientHello.
        forge.assert_private_answers(mirror, lookups);
        assert!(
            forge
                .recorder
                .handshakes()
                .await
                .iter()
                .all(|handshake| handshake.sni.as_deref() != Some(mirror))
        );

        forge.recorder.clear();
        let source = forge.repository(FOREIGN_ASSET_REPO);
        forge.configure(&source, Some(forge.token())).await;
        forge.allow_private(&source);
        let lookups = forge.dns.lookups().len();
        let snapshot = forge.refresh().await;
        assert_eq!(
            snapshot.sources()[0].catalog().unwrap_err(),
            SourceFailure::UnsafeAsset
        );
        let mut expected = forge.probes().to_vec();
        expected.push(forge.metadata(FOREIGN_ASSET_REPO, Auth::Token, 200));
        assert_eq!(forge.recorder.received(), expected);
        assert!(
            !forge.dns.hosts()[lookups..]
                .iter()
                .any(|host| host == mirror),
            "an initial foreign asset URL is rejected before any lookup"
        );
        forge.shutdown().await;
    });
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn package_and_art_on_the_private_source_host_stay_public_and_credential_free() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        let source = forge.repository(PRIVATE_REPO);
        forge.configure(&source, Some(forge.token())).await;
        forge.allow_private(&source);
        let snapshot = forge.refresh().await;
        let catalog = snapshot.sources()[0].catalog().expect("private catalog");
        let tool = &catalog.tools[0];
        let package = format!(
            "https://{}{}",
            forge.host(),
            Forge::download_path(PRIVATE_REPO, PRIVATE_TAG, PACKAGE_ASSET)
        );
        let icon = format!(
            "https://{}{}",
            forge.host(),
            Forge::download_path(PRIVATE_REPO, PRIVATE_TAG, ICON_ASSET)
        );
        assert_eq!(tool.packages["any"].url, package);
        assert_eq!(tool.icon_url.as_deref(), Some(icon.as_str()));

        forge.recorder.clear();
        let lookups = forge.dns.lookups().len();
        let cancel = CancellationToken::new();
        let token = RequestCredential::token(&fixture.forge().secrets.token).unwrap();
        for (url, purpose) in [
            (&package, RequestPurpose::Package),
            (&icon, RequestPurpose::Artwork),
        ] {
            // Even offered the source token, a public purpose attaches nothing
            // and gets no private-network exception on the same host.
            let scoped = forge
                .client
                .request_scoped(
                    url,
                    "vosu",
                    &RequestPolicy::public(purpose),
                    Some(&token),
                    &cancel,
                )
                .await;
            assert!(
                matches!(scoped, Err(NetworkError::Destination(_))),
                "{purpose:?}: {scoped:?}"
            );
            let legacy = forge.client.request_bytes(url, "vosu", &cancel).await;
            assert!(
                matches!(legacy, Err(NetworkError::Destination(_))),
                "{purpose:?}: {legacy:?}"
            );
            assert!(
                matches!(
                    RequestPolicy::source(purpose, forge.source_policy(true)),
                    Err(NetworkError::InvalidPolicy)
                ),
                "{purpose:?} cannot borrow source authority"
            );
        }
        assert!(
            forge.recorder.handshakes().await.is_empty() && forge.recorder.received().is_empty()
        );
        forge.assert_private_answers(forge.host(), lookups);

        // Minimal-difference control: the identical URL is reachable under
        // catalog authority, so only the purpose made the downloads fail.
        let control = forge
            .client
            .request_scoped(
                &package,
                "vosu",
                &RequestPolicy::source(RequestPurpose::CatalogAsset, forge.source_policy(true))
                    .unwrap(),
                Some(&token),
                &cancel,
            )
            .await
            .expect("same URL under catalog authority");
        assert_eq!(control.status, 200);
        assert_eq!(control.body.as_ref(), fixture.forge().package.as_slice());
        assert_eq!(
            forge.recorder.received(),
            [forge.received(
                &Forge::download_path(PRIVATE_REPO, PRIVATE_TAG, PACKAGE_ASSET),
                Some(OCTET),
                Auth::Token,
                200
            )]
        );
        assert_eq!(Url::parse(&package).unwrap().host_str(), Some(forge.host()));
        forge.shutdown().await;
    });
}

#[test]
#[ignore = "D15: needs the real forge fixture; run python scripts/test-catalog-forge.py"]
fn rejected_token_is_reported_without_echo() {
    run(|fixture| async move {
        assert_os_store(&fixture, true);
        let forge = start(&fixture).await;
        let source = forge.repository(PRIVATE_REPO);
        let wrong = format!("x1-rejected-{OWNER}-token");
        forge
            .configure(
                &source,
                Some(vosu_core::catalog::credentials::CredentialInput::Token {
                    token: wrong.clone(),
                }),
            )
            .await;
        forge.allow_private(&source);
        let snapshot = forge.refresh().await;
        let failure = snapshot.sources()[0].catalog().unwrap_err();
        assert_eq!(failure, SourceFailure::CredentialRequired);
        let mut expected = forge.probes().to_vec();
        expected.push(forge.received(
            &format!("/api/v1/repos/{OWNER}/{PRIVATE_REPO}/releases/latest"),
            Some(JSON),
            Auth::Other,
            401,
        ));
        assert_eq!(forge.recorder.received(), expected);
        assert_no_secret_in(
            &[failure.to_string(), format!("{failure:?}")],
            &[wrong.as_str()],
        );
        forge.shutdown().await;
    });
}
