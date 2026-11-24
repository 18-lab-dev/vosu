use super::*;
use crate::streaming::policy::CredentialChain;

fn source_policy(purpose: RequestPurpose, private: bool) -> RequestPolicy {
    RequestPolicy::source(
        purpose,
        SourcePolicy::repository(&Url::parse("https://forge.test/repo").unwrap(), private).unwrap(),
    )
    .unwrap()
}

#[test]
fn github_api_origin_is_exact_and_static_sources_are_literal() {
    let github =
        SourcePolicy::repository(&Url::parse("https://GITHUB.com:443/o/r").unwrap(), false)
            .unwrap();
    assert_eq!(
        github.credential_origin(),
        &Origin::parse("https://api.github.com:443").unwrap()
    );
    let policy = RequestPolicy::source(
        RequestPurpose::SourceMetadata(MetadataAccept::GitHub),
        github,
    )
    .unwrap();
    assert!(policy.credential_eligible(&Url::parse("https://api.github.com/repos/o/r").unwrap()));
    for url in [
        "https://github.com/o/r",
        "https://api.github.com:444/repos/o/r",
        "https://api.github.com.evil.test/repos/o/r",
        "https://sub.api.github.com/repos/o/r",
    ] {
        assert!(!policy.credential_eligible(&Url::parse(url).unwrap()));
    }
    let literal = SourcePolicy::static_source(
        &Url::parse("https://github.com/o/catalog-v2.json").unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(
        literal.credential_origin(),
        &Origin::parse("https://github.com").unwrap()
    );
    let enterprise = SourcePolicy::repository(
        &Url::parse("https://github.enterprise.test/o/r").unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(
        enterprise.credential_origin(),
        &Origin::parse("https://github.enterprise.test").unwrap()
    );
}

#[test]
fn all_auth_schemes_strip_permanently_on_cross_origin_and_initial_foreign_assets() {
    let policy = source_policy(RequestPurpose::CatalogAsset, false);
    let a = Url::parse("https://forge.test/a").unwrap();
    let same = Url::parse("https://FORGE.test:443/b").unwrap();
    for credential in [
        RequestCredential::bearer("token-sentinel").unwrap(),
        RequestCredential::token("gitea-sentinel").unwrap(),
        RequestCredential::basic("username-sentinel", "password-sentinel").unwrap(),
    ] {
        let mut chain = CredentialChain::new(&policy, &a, Some(&credential));
        assert!(chain.header().unwrap().is_sensitive());
        chain.redirect(&a, &same);
        assert!(chain.header().is_some());
        for b in ["https://storage.test/a", "https://forge.test:444/a"] {
            let b = Url::parse(b).unwrap();
            let mut chain = CredentialChain::new(&policy, &a, Some(&credential));
            chain.redirect(&a, &b);
            assert!(chain.header().is_none());
            chain.redirect(&b, &a);
            assert!(chain.header().is_none());
            let mut foreign = CredentialChain::new(&policy, &b, Some(&credential));
            assert!(foreign.header().is_none());
            foreign.redirect(&b, &a);
            assert!(foreign.header().is_none());
        }
    }
}

#[test]
fn purpose_cannot_promote_public_downloads_or_authenticate_probes() {
    let source =
        SourcePolicy::repository(&Url::parse("https://forge.test/repo").unwrap(), true).unwrap();
    let url = Url::parse("https://forge.test/a").unwrap();
    let credential = RequestCredential::bearer("sentinel").unwrap();
    for purpose in [
        RequestPurpose::Package,
        RequestPurpose::Artwork,
        RequestPurpose::Updater,
        RequestPurpose::Counter,
    ] {
        assert!(RequestPolicy::source(purpose, source.clone()).is_err());
        let public = RequestPolicy::public(purpose);
        assert!(!public.permits_private(&url));
        assert!(
            CredentialChain::new(&public, &url, Some(&credential))
                .header()
                .is_none()
        );
    }
    let probe = source_policy(RequestPurpose::Probe, true);
    assert!(
        CredentialChain::new(&probe, &url, Some(&credential))
            .header()
            .is_none()
    );
}

#[tokio::test]
async fn exact_host_private_exception_is_per_hop_and_port_not_credential_origin() {
    let client = test_client(vec![
        "1.1.1.1".parse().unwrap(),
        "::ffff:127.0.0.1".parse().unwrap(),
    ]);
    for purpose in [
        RequestPurpose::Probe,
        RequestPurpose::SourceMetadata(MetadataAccept::Json),
        RequestPurpose::CatalogAsset,
    ] {
        let enabled = source_policy(purpose, true);
        let disabled = source_policy(purpose, false);
        let own = Url::parse("https://forge.test:444/catalog-v2.json").unwrap();
        let resolved = bounded!(client.resolve_and_validate_policy(&own, Some(&enabled))).unwrap();
        assert_eq!(resolved.1, 444);
        assert_eq!(
            resolved.2,
            vec![
                "1.1.1.1".parse::<IpAddr>().unwrap(),
                "127.0.0.1".parse().unwrap()
            ]
        );
        assert!(!enabled.credential_eligible(&own));
        assert!(bounded!(client.resolve_and_validate_policy(&own, Some(&disabled))).is_err());
        for host in ["sub.forge.test", "forge.test.evil.test", "other.test"] {
            let foreign = Url::parse(&format!("https://{host}/a")).unwrap();
            assert!(
                bounded!(client.resolve_and_validate_policy(&foreign, Some(&enabled))).is_err()
            );
        }
        assert!(
            bounded!(client.resolve_and_validate_policy(
                &Url::parse("http://forge.test/a").unwrap(),
                Some(&enabled)
            ))
            .is_err()
        );
    }
    for purpose in [
        RequestPurpose::Package,
        RequestPurpose::Artwork,
        RequestPurpose::Updater,
        RequestPurpose::Counter,
    ] {
        let public = RequestPolicy::public(purpose);
        assert!(
            bounded!(client.resolve_and_validate_policy(
                &Url::parse("https://forge.test/a").unwrap(),
                Some(&public)
            ))
            .is_err()
        );
    }
    let empty = test_client(vec![]);
    assert!(
        bounded!(empty.resolve_and_validate_policy(
            &Url::parse("https://forge.test/a").unwrap(),
            Some(&source_policy(RequestPurpose::Probe, true))
        ))
        .is_err()
    );
}

#[test]
fn credentials_enforce_utf8_byte_bounds_controls_and_sensitive_basic_encoding() {
    assert!(RequestCredential::bearer(&"x".repeat(8192)).is_ok());
    assert!(RequestCredential::bearer(&"x".repeat(8193)).is_err());
    assert!(RequestCredential::basic("u", &"é".repeat(4097)).is_err());
    for value in [
        "secret\rvalue",
        "secret\nvalue",
        "secret\0value",
        "secret\tvalue",
        "secret\u{85}value",
    ] {
        assert!(RequestCredential::bearer(value).is_err());
        assert!(RequestCredential::token(value).is_err());
        assert!(RequestCredential::basic(value, "p").is_err());
        assert!(RequestCredential::basic("u", value).is_err());
    }
    assert!(RequestCredential::basic("u:bad", "p").is_err());
    let basic = RequestCredential::basic("user", "pass").unwrap().value();
    assert_eq!(basic.to_str().unwrap(), "Basic dXNlcjpwYXNz");
    assert!(basic.is_sensitive());
}

#[test]
fn userinfo_is_rejected_initially_and_on_relative_redirects() {
    for url in [
        "https://user:password@forge.test/a",
        "https://user@forge.test/a",
        "http://forge.test/a",
    ] {
        assert!(Origin::parse(url).is_err());
    }
    let current = Url::parse("https://forge.test/a").unwrap();
    let location = reqwest::header::HeaderValue::from_static("//u:p@forge.test/b");
    assert!(redirect_target(&current, Some(&location), 0, 5).is_err());
}

#[derive(Default)]
struct ProbeClock(std::sync::Mutex<Vec<Duration>>);
impl TimeSource for ProbeClock {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        self.0.lock().unwrap().push(duration);
        if duration == Duration::from_secs(5) {
            Box::pin(async {})
        } else {
            Box::pin(core::future::pending())
        }
    }
}

#[tokio::test]
async fn probe_has_independent_five_second_total_deadline_and_rejects_credentials() {
    let clock = Arc::new(ProbeClock::default());
    let client = StreamingClient::new(
        Arc::new(NeverResolver),
        DestinationPolicy::PublicOnly,
        Deadlines::default(),
        DownloadLimits::default(),
    )
    .with_time_source(clock.clone());
    let policy = source_policy(RequestPurpose::Probe, false);
    let cancel = CancellationToken::new();
    assert!(matches!(
        bounded!(client.request_scoped("https://forge.test/a", "test", &policy, None, &cancel)),
        Err(NetworkError::Timeout)
    ));
    assert!(clock.0.lock().unwrap().contains(&Duration::from_secs(5)));
    let credential = RequestCredential::bearer("sentinel").unwrap();
    assert!(matches!(
        bounded!(client.request_scoped(
            "https://forge.test/a",
            "test",
            &policy,
            Some(&credential),
            &cancel
        )),
        Err(NetworkError::InvalidPolicy)
    ));
}

#[test]
fn response_diagnostics_omit_body_and_presigned_query() {
    let response = NetworkResponse {
        status: 200,
        body: Bytes::from_static(b"token-sentinel"),
        final_url: Url::parse("https://forge.test/a?secret=presigned-sentinel").unwrap(),
    };
    let debug = format!("{response:?}");
    assert!(!debug.contains("token-sentinel"));
    assert!(!debug.contains("presigned-sentinel"));
}

#[test]
fn probe_initial_origin_must_be_the_configured_repository_origin() {
    let policy = source_policy(RequestPurpose::Probe, true);
    assert!(
        policy
            .validate_initial(&Url::parse("https://forge.test/api/v4/version").unwrap())
            .is_ok()
    );
    for url in [
        "https://foreign.test/api/v4/version",
        "https://forge.test:444/api/v4/version",
    ] {
        assert!(policy.validate_initial(&Url::parse(url).unwrap()).is_err());
    }
}

#[tokio::test]
async fn scoped_request_failure_redacts_presigned_query_and_does_not_allow_plaintext_tls() {
    use tokio::io::AsyncWriteExt as _;
    let listener = bounded!(tokio::net::TcpListener::bind(("127.0.0.1", 0))).unwrap();
    let socket = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = bounded!(listener.accept()).unwrap();
        bounded!(stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"))
            .unwrap();
    });
    let client = test_client(vec![socket.ip()]);
    let policy = source_policy(RequestPurpose::CatalogAsset, true);
    let url = format!(
        "https://forge.test:{}/a?secret=presigned-sentinel",
        socket.port()
    );
    let error =
        bounded!(client.request_scoped(&url, "test", &policy, None, &CancellationToken::new()))
            .unwrap_err();
    assert!(matches!(error, NetworkError::Request(_)));
    assert!(!format!("{error}").contains("presigned-sentinel"));
    assert!(!format!("{error:?}").contains("presigned-sentinel"));
    bounded!(server).unwrap();
}
