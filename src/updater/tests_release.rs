    fn release(tag: &str, flags: &str, assets: &str) -> Vec<u8> {
        format!(r#"{{"tag_name":"{tag}",{flags}"assets":[{assets}]}}"#).into_bytes()
    }
    fn asset(name: &str, digest: &str) -> String {
        format!(
            r#"{{"name":"{name}","browser_download_url":"https://example.com/a.zip","digest":"{digest}"}}"#
        )
    }
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn validates_release_versions_assets_and_digest() {
        let current = Version::new(0, 1, 0);
        assert_eq!(
            parse_latest_release(&release("v0.1.0", "", ""), &current, "linux").unwrap(),
            ReleaseDecision::Latest(current.clone())
        );
        assert_eq!(
            parse_latest_release(&release("v0.0.9", "", ""), &current, "linux").unwrap(),
            ReleaseDecision::Latest(Version::new(0, 0, 9))
        );
        let good = asset("vosu-linux-v0.2.0.zip", &format!("sha256:{HASH}"));
        assert!(matches!(
            parse_latest_release(&release("v0.2.0", "", &good), &current, "linux").unwrap(),
            ReleaseDecision::Update(_)
        ));
        assert_eq!(
            parse_latest_release(&release("v0.2.0", "", ""), &current, "linux").unwrap(),
            ReleaseDecision::NoFiles
        );
        // The former tosu asset name is retained only as a cutover rejection fixture.
        let legacy = asset("tosu-linux-v0.2.0.zip", &format!("sha256:{HASH}"));
        assert_eq!(
            parse_latest_release(&release("v0.2.0", "", &legacy), &current, "linux").unwrap(),
            ReleaseDecision::NoFiles
        );
        assert!(
            parse_latest_release(
                &release("v0.2.0", "", &format!("{good},{good}")),
                &current,
                "linux"
            )
            .is_err()
        );
        assert!(
            parse_latest_release(
                &release("v0.2.0", "", &asset("vosu-linux-v0.2.0.zip", "sha256:no")),
                &current,
                "linux"
            )
            .is_err()
        );
        assert_eq!(
            parse_latest_release(&release("v0.2.0-beta.1", "", ""), &current, "linux").unwrap(),
            ReleaseDecision::Latest(current.clone())
        );
        assert_eq!(
            parse_latest_release(
                &release("v0.2.0", "\"prerelease\":true,", ""),
                &current,
                "linux"
            )
            .unwrap(),
            ReleaseDecision::Latest(current.clone())
        );
        let forced = Version::parse("0.1.0-forced").unwrap();
        assert_eq!(
            parse_latest_release(&release("v0.2.0", "", &good), &forced, "linux").unwrap(),
            ReleaseDecision::SuppressedForced
        );
        assert!(parse_latest_release(br#"{"tag_name":7}"#, &current, "linux").is_err());
    }

    #[test]
    fn forced_and_development_startup_policies_short_circuit_release_lookup() {
        assert!(is_forced_version(&Version::parse("0.1.0-forced").unwrap()));
        assert!(!is_forced_version(&Version::new(0, 1, 0)));
        if cfg!(debug_assertions) {
            assert!(!startup_uses_remote_release());
        }
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const READY_TOKEN: &str = "fedcba9876543210fedcba9876543210";

    fn os_arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_exact_internal_startup_roles_and_ignores_public_arguments() {
        assert_eq!(
            parse_startup_update_args(os_arguments(&["--update=true", "--unknown=yes"])).unwrap(),
            StartupIntent::Ordinary
        );
        assert_eq!(
            parse_startup_update_args(os_arguments(&[
                "--update-helper",
                &format!("--update-token={TOKEN}"),
                "--update-record=/tmp/record",
            ]))
            .unwrap(),
            StartupIntent::Helper {
                record_path: PathBuf::from("/tmp/record"),
                transaction_token: TOKEN.into(),
            }
        );
        let candidate = parse_startup_update_args(os_arguments(&[
            &format!("--update-candidate={READY_TOKEN}"),
            "--update-ready=127.0.0.1:24050",
            &format!("--update-token={TOKEN}"),
            "--update-record=/tmp/record",
        ]))
        .unwrap();
        assert!(matches!(
            candidate,
            StartupIntent::Candidate(StartupReadiness {
                transaction_token,
                readiness_token,
                address,
                ..
            }) if transaction_token == TOKEN
                && readiness_token == READY_TOKEN
                && address == "127.0.0.1:24050".parse::<SocketAddr>().unwrap()
        ));
        assert!(matches!(
            parse_startup_update_args(os_arguments(&[
                &format!("--update-rollback={READY_TOKEN}"),
                "--update-ready=[::1]:24050",
                &format!("--update-token={TOKEN}"),
                "--update-record=/tmp/record",
            ]))
            .unwrap(),
            StartupIntent::Rollback(_)
        ));
    }

    #[test]
    fn rejects_partial_conflicting_duplicate_and_malformed_internal_roles() {
        let cases = [
            vec!["--update-helper".to_owned()],
            vec![
                "--update-helper".to_owned(),
                format!("--update-candidate={READY_TOKEN}"),
                "--update-ready=127.0.0.1:1".to_owned(),
                format!("--update-token={TOKEN}"),
                "--update-record=/tmp/r".to_owned(),
            ],
            vec![
                format!("--update-candidate={READY_TOKEN}"),
                format!("--update-rollback={READY_TOKEN}"),
                "--update-ready=127.0.0.1:1".to_owned(),
                format!("--update-token={TOKEN}"),
                "--update-record=/tmp/r".to_owned(),
            ],
            vec![
                "--update-helper".to_owned(),
                format!("--update-token={TOKEN}"),
                format!("--update-token={TOKEN}"),
                "--update-record=/tmp/r".to_owned(),
            ],
            vec!["--update-ready".to_owned()],
            vec![
                "--update-candidate=short".to_owned(),
                "--update-ready=127.0.0.1:1".to_owned(),
                format!("--update-token={TOKEN}"),
                "--update-record=/tmp/r".to_owned(),
            ],
        ];
        for arguments in cases {
            assert!(
                matches!(
                    parse_startup_update_args(
                        arguments.iter().map(|value| OsString::from(value.as_str()))
                    ),
                    Err(UpdateError::InternalArguments(_))
                ),
                "{arguments:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_internal_record_path() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let mut record_argument = b"--update-record=/tmp/".to_vec();
        record_argument.push(0xff);
        let intent = parse_startup_update_args([
            OsString::from("--update-helper"),
            OsString::from(format!("--update-token={TOKEN}")),
            OsString::from_vec(record_argument),
        ])
        .unwrap();
        let StartupIntent::Helper { record_path, .. } = intent else {
            panic!("expected helper intent");
        };
        assert_eq!(record_path.as_os_str().as_bytes(), b"/tmp/\xff");
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_two_update_generations_without_losing_non_utf8_user_args() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let user_value = OsString::from_vec(vec![b'u', b's', b'e', b'r', 0xff]);
        let first = canonical_original_args([
            OsString::from("--user-option"),
            user_value.clone(),
            OsString::from(format!("--update-candidate={READY_TOKEN}")),
            OsString::from("--update-ready=127.0.0.1:1"),
            OsString::from(format!("--update-token={TOKEN}")),
            OsString::from("--update-record=/tmp/first"),
        ]);
        let mut replay = first
            .iter()
            .map(EncodedOsString::restore)
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        replay.extend([
            OsString::from("--update-rollback"),
            OsString::from(READY_TOKEN),
            OsString::from("--update-ready"),
            OsString::from("127.0.0.1:2"),
            OsString::from("--update-token"),
            OsString::from(TOKEN),
            OsString::from("--update-record"),
            OsString::from_vec(b"/tmp/second-\xff".to_vec()),
        ]);
        let second = canonical_original_args(replay);
        let restored = second
            .iter()
            .map(EncodedOsString::restore)
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0], OsString::from("--user-option"));
        assert_eq!(
            restored[1].as_os_str().as_bytes(),
            user_value.as_os_str().as_bytes()
        );
    }
