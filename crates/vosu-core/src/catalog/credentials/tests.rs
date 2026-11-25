use super::*;
use std::{
    fs,
    sync::{Arc, mpsc},
    time::Duration,
};

const FORGE: &str = "owner/repository";
const STATIC: &str = "https://catalog.example/catalog-v2.json";
const TOKEN: &str = "token-distinct-sentinel";
const USER: &str = "username-distinct-sentinel";
const PASSWORD: &str = "password-distinct-sentinel";
fn token(value: &str) -> CredentialInput {
    CredentialInput::Token {
        token: value.into(),
    }
}
fn basic() -> CredentialInput {
    CredentialInput::Basic {
        username: USER.into(),
        password: PASSWORD.into(),
    }
}
fn sources() -> Vec<String> {
    vec![FORGE.into(), STATIC.into()]
}
fn setup() -> (tempfile::TempDir, CredentialStore) {
    let root = tempfile::tempdir().unwrap();
    let store = CredentialStore::open(root.path());
    store.reconcile_sources(&sources()).unwrap();
    (root, store)
}
fn secret(store: &CredentialStore, source: &str) -> Arc<CredentialInput> {
    store.lease(source).unwrap().unwrap().0
}
fn assert_token(store: &CredentialStore, source: &str, expected: &str) {
    match secret(store, source).as_ref() {
        CredentialInput::Token { token } => assert_eq!(token, expected),
        CredentialInput::Basic { .. } => panic!("wrong credential kind"),
    }
}

#[test]
fn missing_exact_identity_roundtrip_and_redacted_views() {
    let (root, store) = setup();
    assert!(!store.presence(FORGE).has_credential);
    assert!(!root.path().join("catalog-credentials.toml").exists());
    let generation = store.set(FORGE, token(TOKEN)).unwrap();
    store.set(STATIC, basic()).unwrap();
    assert_token(&store, FORGE, TOKEN);
    assert!(!store.presence("Owner/repository").has_credential);
    let public = format!(
        "{:?} {:?} {:?}",
        store.presence(FORGE),
        CredentialError::InvalidInput,
        CredentialError::Unavailable
    );
    for sentinel in [TOKEN, USER, PASSWORD] {
        assert!(!public.contains(sentinel));
    }
    assert!(store.presence(FORGE).generation > generation);
    let reopened = CredentialStore::open(root.path());
    assert!(!reopened.presence(FORGE).has_credential); // S0 must authorize membership first.
    reopened.reconcile_sources(&sources()).unwrap();
    assert_token(&reopened, FORGE, TOKEN);
    match secret(&reopened, STATIC).as_ref() {
        CredentialInput::Basic { username, password } => {
            assert_eq!(username, USER);
            assert_eq!(password, PASSWORD);
        }
        CredentialInput::Token { .. } => panic!("wrong credential kind"),
    }
}

#[test]
fn mutation_body_fields_controls_and_kind_are_bounded() {
    let (_root, store) = setup();
    assert!(
        CredentialInput::from_body(br#"{"kind":"token","token":"good","extra":true}"#).is_err()
    );
    assert!(CredentialInput::from_body(&vec![b' '; MAX_MUTATION_BYTES + 1]).is_err());
    assert!(
        store
            .set(FORGE, token(&"x".repeat(MAX_FIELD_BYTES)))
            .is_ok()
    );
    assert_eq!(
        store.set(FORGE, token(&"x".repeat(MAX_FIELD_BYTES + 1))),
        Err(CredentialError::InvalidInput)
    );
    for bad in [
        "",
        "secret\rheader",
        "secret\nheader",
        "secret\0header",
        "secret\theader",
        "not a token",
        "é",
    ] {
        assert_eq!(
            store.set(FORGE, token(bad)),
            Err(CredentialError::InvalidInput)
        );
    }
    assert_eq!(
        store.set(FORGE, basic()),
        Err(CredentialError::InvalidInput)
    );
    assert_eq!(
        store.set(STATIC, token(TOKEN)),
        Err(CredentialError::InvalidInput)
    );
    let unicode = CredentialInput::Basic {
        username: "usér".into(),
        password: "pass phrase".into(),
    };
    assert!(store.set(STATIC, unicode).is_ok());
    assert_eq!(
        store.set(
            STATIC,
            CredentialInput::Basic {
                username: "user:name".into(),
                password: PASSWORD.into()
            }
        ),
        Err(CredentialError::InvalidInput)
    );
}

#[test]
fn malformed_duplicate_unknown_field_and_oversized_store_never_rewritten() {
    let valid = format!("[[sources]]\nsource = {FORGE:?}\nkind = \"token\"\ntoken = {TOKEN:?}\n");
    let cases = vec![
        b"malformed secret sentinel = [".to_vec(),
        [valid.as_bytes(), valid.as_bytes()].concat(),
        format!("{valid}unexpected = \"secret\"\n").into_bytes(),
        vec![b' '; MAX_DOCUMENT_BYTES + 1],
        vec![0xff],
        format!(
            "[[sources]]\nsource = {FORGE:?}\nkind = \"token\"\ntoken = {:?}\n",
            "x".repeat(MAX_FIELD_BYTES + 1)
        )
        .into_bytes(),
        (0..MAX_ENTRIES + 1)
            .map(|n| {
                format!(
                    "[[sources]]\nsource = \"owner/repo{n}\"\nkind = \"token\"\ntoken = \"t\"\n"
                )
            })
            .collect::<String>()
            .into_bytes(),
    ];
    for bytes in cases {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog-credentials.toml");
        fs::write(&path, &bytes).unwrap();
        let store = CredentialStore::open(root.path());
        store.reconcile_sources(&sources()).unwrap();
        assert_eq!(
            store.presence(FORGE).availability,
            CredentialAvailability::Unavailable
        );
        assert_eq!(
            store.set(FORGE, token(TOKEN)),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(store.delete(FORGE), Err(CredentialError::Unavailable));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        store.repair(vec![(FORGE.into(), token(TOKEN))]).unwrap();
        assert_token(&store, FORGE, TOKEN);
        assert!(!store.presence(STATIC).has_credential);
    }
}

#[test]
fn unreadable_store_and_failed_publication_preserve_state() {
    let (root, store) = setup();
    store.set(FORGE, token(TOKEN)).unwrap();
    let generation = store.presence(FORGE).generation;
    let path = root.path().join("catalog-credentials.toml");
    let old = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap(); // A directory cannot be atomically replaced by a file.
    assert_eq!(
        store.set(FORGE, token("new-secret")),
        Err(CredentialError::Persistence)
    );
    assert_eq!(store.delete(FORGE), Err(CredentialError::Persistence));
    assert_eq!(
        store.reconcile_sources(&[]),
        Err(CredentialError::Persistence)
    );
    assert_token(&store, FORGE, TOKEN);
    assert_eq!(store.presence(FORGE).generation, generation);
    assert!(path.is_dir());
    let unreadable = CredentialStore::open(root.path());
    assert_eq!(
        unreadable.presence(FORGE).availability,
        CredentialAvailability::Unavailable
    );
    fs::remove_dir(path).unwrap();
    fs::write(root.path().join("catalog-credentials.toml"), old).unwrap();
}

#[test]
fn source_removal_readd_and_channel_roots_cannot_revive_secrets() {
    let (root, store) = setup();
    store.set(FORGE, token(TOKEN)).unwrap();
    let lease = secret(&store, FORGE); // Already-issued request material cannot be recalled.
    let before = store.presence(FORGE).generation;
    store.reconcile_sources(&[STATIC.into()]).unwrap();
    assert!(store.presence(FORGE).generation > before);
    assert!(matches!(lease.as_ref(), CredentialInput::Token { .. }));
    assert_eq!(store.delete(FORGE), Err(CredentialError::UnknownSource));
    assert_eq!(
        store.set(FORGE, token(TOKEN)),
        Err(CredentialError::UnknownSource)
    );
    store.reconcile_sources(&sources()).unwrap();
    assert!(!store.presence(FORGE).has_credential);
    let generation = store.presence(FORGE).generation;
    assert_eq!(store.delete(FORGE).unwrap(), generation);
    let reopened = CredentialStore::open(root.path());
    reopened.reconcile_sources(&sources()).unwrap();
    assert!(!reopened.presence(FORGE).has_credential);
    let alpha = CredentialStore::open(&root.path().join("alpha"));
    alpha.reconcile_sources(&sources()).unwrap();
    alpha.set(FORGE, token("alpha-only")).unwrap();
    assert!(!store.presence(FORGE).has_credential);
    assert_token(&alpha, FORGE, "alpha-only");
}

#[test]
fn concurrent_mutations_and_removal_are_serialized_with_bounded_waits() {
    let (_root, store) = setup();
    let store = Arc::new(store);
    let (tx, rx) = mpsc::channel();
    let workers: Vec<_> = (0..8)
        .map(|n| {
            let store = Arc::clone(&store);
            let tx = tx.clone();
            std::thread::spawn(move || {
                for _ in 0..20 {
                    if n % 2 == 0 {
                        store.set(FORGE, token(TOKEN)).unwrap();
                    } else {
                        store.delete(FORGE).unwrap();
                    }
                }
                tx.send(()).unwrap();
            })
        })
        .collect();
    for _ in 0..workers.len() {
        rx.recv_timeout(Duration::from_secs(20)).unwrap();
    }
    for worker in workers {
        worker.join().unwrap();
    }
    store.delete(FORGE).unwrap();
    assert!(!store.presence(FORGE).has_credential);
    store.reconcile_sources(&[STATIC.into()]).unwrap();
    assert_eq!(
        store.set(FORGE, token(TOKEN)),
        Err(CredentialError::UnknownSource)
    );
}

#[test]
fn repair_rejects_duplicates_unknown_members_and_document_expansion() {
    let (_root, store) = setup();
    assert_eq!(
        store.repair(vec![
            (FORGE.into(), token(TOKEN)),
            (FORGE.into(), token("other"))
        ]),
        Err(CredentialError::InvalidInput)
    );
    assert_eq!(
        store.repair(vec![("other/repository".into(), token(TOKEN))]),
        Err(CredentialError::UnknownSource)
    );
    let identities: Vec<_> = (0..32)
        .map(|n| format!("https://source{n}.example/catalog-v2.json"))
        .collect();
    store.reconcile_sources(&identities).unwrap();
    let inputs = identities
        .iter()
        .map(|source| {
            (
                source.clone(),
                CredentialInput::Basic {
                    username: "u".repeat(MAX_FIELD_BYTES),
                    password: "p".repeat(MAX_FIELD_BYTES),
                },
            )
        })
        .collect();
    assert_eq!(store.repair(inputs), Err(CredentialError::InvalidInput));
    assert!(!store.presence(&identities[0]).has_credential);
}

#[cfg(unix)]
#[test]
fn unix_final_permissions_are_private_and_owner_is_current_user() {
    use std::os::unix::fs::MetadataExt;
    let (root, store) = setup();
    store.set(FORGE, token(TOKEN)).unwrap();
    for value in [TOKEN, "replacement-token"] {
        store.set(FORGE, token(value)).unwrap();
        let metadata = fs::metadata(root.path().join("catalog-credentials.toml")).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), fs::metadata(root.path()).unwrap().uid());
    }
}

#[test]
fn sync_and_replace_faults_preserve_old_bytes_memory_generation_and_clean_temps() {
    for fault in [
        super::store::PublishFault::Sync,
        super::store::PublishFault::Replace,
    ] {
        let (root, store) = setup();
        store.set(FORGE, token(TOKEN)).unwrap();
        let path = root.path().join("catalog-credentials.toml");
        let old = fs::read(&path).unwrap();
        let generation = store.presence(FORGE).generation;
        store.fail_publication(fault);
        assert_eq!(
            store.set(FORGE, token("replacement")),
            Err(CredentialError::Persistence)
        );
        assert_eq!(fs::read(&path).unwrap(), old);
        assert_eq!(store.presence(FORGE).generation, generation);
        assert_token(&store, FORGE, TOKEN);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[cfg(windows)]
fn icacls(args: &[&std::ffi::OsStr]) {
    use std::process::{Command, Stdio};
    let mut child = Command::new("icacls.exe")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "icacls fixture operation failed");
            return;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("icacls fixture operation timed out");
        }
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

#[cfg(windows)]
#[test]
fn windows_directory_acl_inheritance_and_existing_security_metadata_survive_replacement() {
    use std::ffi::OsStr;
    let (root, store) = setup();
    // The temporary directory's own ACL depends on the machine: hosted CI's
    // temp root carries no inheritable ACE, so a new file there has only
    // explicit ones. Grant one inheritable ACE on the store directory first,
    // so the test checks that replacement keeps inherited metadata rather
    // than whatever the runner's temp root happens to propagate.
    icacls(&[
        root.path().as_os_str(),
        OsStr::new("/grant"),
        OsStr::new("*S-1-5-32-545:(OI)(CI)(RX)"),
        OsStr::new("/q"),
    ]);
    store.set(FORGE, token(TOKEN)).unwrap();
    let path = root.path().join("catalog-credentials.toml");
    let acl = root.path().join("acl.txt");
    icacls(&[
        path.as_os_str(),
        OsStr::new("/save"),
        acl.as_os_str(),
        OsStr::new("/q"),
    ]);
    let bytes = fs::read(&acl).unwrap();
    let inherited = String::from_utf16(
        &bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(
        inherited.contains("ID"),
        "expected inherited ACEs in the new file"
    );
    // Copy inherited ACEs to explicit ACEs, then prove replacement preserves
    // that existing destination security metadata rather than resetting it.
    icacls(&[
        path.as_os_str(),
        OsStr::new("/inheritance:d"),
        OsStr::new("/q"),
    ]);
    fs::remove_file(&acl).unwrap();
    icacls(&[
        path.as_os_str(),
        OsStr::new("/save"),
        acl.as_os_str(),
        OsStr::new("/q"),
    ]);
    let before = fs::read(&acl).unwrap();
    store.set(FORGE, token("replacement-token")).unwrap();
    fs::remove_file(&acl).unwrap();
    icacls(&[
        path.as_os_str(),
        OsStr::new("/save"),
        acl.as_os_str(),
        OsStr::new("/q"),
    ]);
    assert_eq!(fs::read(&acl).unwrap(), before);
    let reopened = CredentialStore::open(root.path());
    reopened.reconcile_sources(&sources()).unwrap();
    assert_token(&reopened, FORGE, "replacement-token");
}

#[test]
fn credential_kind_uses_source_parser_without_identity_aliases() {
    let root = tempfile::tempdir().unwrap();
    let store = CredentialStore::open(root.path());
    let static_url = "HTTPS://static.test/catalog-v2.json";
    let repository = "HTTPS://forge.test/group/catalog";
    let lookalike = "https://forge.test/group/catalog-v2.json.evil";
    store
        .reconcile_sources(&[static_url.into(), repository.into(), lookalike.into()])
        .unwrap();
    assert!(store.set(static_url, basic()).is_ok());
    assert_eq!(
        store.set(static_url, token(TOKEN)),
        Err(CredentialError::InvalidInput)
    );
    assert!(store.set(repository, token(TOKEN)).is_ok());
    assert_eq!(
        store.set(repository, basic()),
        Err(CredentialError::InvalidInput)
    );
    assert!(store.set(lookalike, token(TOKEN)).is_ok());
    assert_eq!(
        store.set(lookalike, basic()),
        Err(CredentialError::InvalidInput)
    );
    for invalid in [
        "https://static.test/catalog-v2.json?query=1",
        "https://x/catalog-v2.json.evil",
    ] {
        assert!(crate::catalog::sources::SourceIdentity::parse(invalid).is_err());
        assert!(!basic().matches_source(invalid));
        assert!(!token(TOKEN).matches_source(invalid));
    }
    assert!(
        !store
            .presence("https://static.test/catalog-v2.json")
            .has_credential
    );
    assert!(store.presence(static_url).has_credential);
}
