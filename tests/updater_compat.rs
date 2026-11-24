#![expect(
    clippy::disallowed_methods,
    reason = "test fixtures write scratch files and sleep"
)]

use std::{
    ffi::OsString,
    fs, io,
    path::Path,
    process::Child,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use semver::Version;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use vosu_core::updater::{
    EncodedOsString, INSTALL_RECORD_NAME, InstallPhase, InstallRecord, MajorRelease,
    ProcessIdentity, RecoveryAction, ReleaseDecision, StartupIntent, TransactionGuard, UpdateError,
    UpdatePlatform, classify_recovery, load_record, parse_latest_release,
    parse_startup_update_args, user_agent,
};

fn release(tag: &str, draft: bool, prerelease: bool, assets: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "tag_name": tag,
        "draft": draft,
        "prerelease": prerelease,
        "assets": assets,
    }))
    .unwrap()
}

fn release_with_html_url(
    tag: &str,
    draft: bool,
    prerelease: bool,
    assets: serde_json::Value,
    html_url: &str,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "tag_name": tag,
        "draft": draft,
        "prerelease": prerelease,
        "html_url": html_url,
        "assets": assets,
    }))
    .unwrap()
}

fn asset(name: &str, digest: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "browser_download_url": format!("https://downloads.example/{name}"),
        "digest": digest,
    })
}

#[test]
fn latest_release_matrix_requires_stable_newer_exact_asset_and_sha256() {
    let current = Version::parse("0.1.0").unwrap();
    let good_digest = format!("sha256:{}", "ab".repeat(32));
    let good = release(
        "v0.2.0",
        false,
        false,
        serde_json::json!([
            asset("vosu-linux-v0.2.0.zip", Some(&good_digest)),
            asset("vosu-windows-v0.2.0.zip", Some(&good_digest)),
        ]),
    );
    let ReleaseDecision::Update(update) =
        parse_latest_release(&good, &current, "linux", false).unwrap()
    else {
        panic!("new release")
    };
    assert_eq!(update.version, Version::parse("0.2.0").unwrap());
    assert_eq!(update.asset_name, "vosu-linux-v0.2.0.zip");
    assert_eq!(update.digest, [0xab; 32]);

    for (tag, expected) in [
        ("v0.1.0", Version::parse("0.1.0").unwrap()),
        ("v0.0.9", Version::parse("0.0.9").unwrap()),
    ] {
        assert_eq!(
            parse_latest_release(
                &release(tag, false, false, json_empty()),
                &current,
                "linux",
                false
            )
            .unwrap(),
            ReleaseDecision::Latest(expected)
        );
    }
    assert_eq!(
        parse_latest_release(
            &release("v0.2.0", true, false, json_empty()),
            &current,
            "linux",
            false
        )
        .unwrap(),
        ReleaseDecision::Latest(current.clone())
    );
    assert_eq!(
        parse_latest_release(
            &release("v0.2.0", false, true, json_empty()),
            &current,
            "linux",
            false
        )
        .unwrap(),
        ReleaseDecision::Latest(current.clone())
    );
    assert_eq!(
        parse_latest_release(
            &release("v0.2.0-beta.1", false, false, json_empty()),
            &current,
            "linux",
            false
        )
        .unwrap(),
        ReleaseDecision::Latest(current.clone())
    );
    assert_eq!(
        parse_latest_release(
            &release(
                "v0.2.0",
                false,
                false,
                serde_json::json!([asset("vosu-linux-v0.2.0.zip.sig", Some(&good_digest))])
            ),
            &current,
            "linux",
            false
        )
        .unwrap(),
        ReleaseDecision::NoFiles
    );
    // The former tosu asset name is retained only as a public cutover rejection fixture.
    assert_eq!(
        parse_latest_release(
            &release(
                "v0.2.0",
                false,
                false,
                serde_json::json!([asset("tosu-linux-v0.2.0.zip", Some(&good_digest))])
            ),
            &current,
            "linux",
            false
        )
        .unwrap(),
        ReleaseDecision::NoFiles
    );

    let duplicate = release(
        "v0.2.0",
        false,
        false,
        serde_json::json!([
            asset("vosu-linux-v0.2.0.zip", Some(&good_digest)),
            asset("vosu-linux-v0.2.0.zip", Some(&good_digest)),
        ]),
    );
    assert!(matches!(
        parse_latest_release(&duplicate, &current, "linux", false),
        Err(UpdateError::Metadata(_))
    ));
    for digest in [
        None,
        Some("sha1:00"),
        Some("sha256:abc"),
        Some("sha256:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
    ] {
        let invalid = release(
            "v0.2.0",
            false,
            false,
            serde_json::json!([asset("vosu-linux-v0.2.0.zip", digest)]),
        );
        assert!(matches!(
            parse_latest_release(&invalid, &current, "linux", false),
            Err(UpdateError::Metadata(_))
        ));
    }
    assert!(matches!(
        parse_latest_release(
            &release("vv0.2.0", false, false, json_empty()),
            &current,
            "linux",
            false
        ),
        Err(UpdateError::Metadata(_))
    ));
}

#[test]
fn major_version_release_needs_consent_startup_never_supplies_and_asset_unvalidated_without_it() {
    let current = Version::parse("1.2.3").unwrap();
    let good_digest = format!("sha256:{}", "ab".repeat(32));
    let release_url = "https://github.com/18-lab-dev/vosu/releases/tag/v2.0.0";
    let major = release_with_html_url(
        "v2.0.0",
        false,
        false,
        serde_json::json!([asset("vosu-linux-v2.0.0.zip", Some(&good_digest))]),
        release_url,
    );
    // Without consent, a release asset that would otherwise validate is never
    // even looked at: the decision is report-only.
    assert_eq!(
        parse_latest_release(&major, &current, "linux", false).unwrap(),
        ReleaseDecision::MajorAvailable(MajorRelease {
            version: Version::parse("2.0.0").unwrap(),
            release_url: release_url.to_owned(),
        })
    );
    // A broken or missing asset would not stop the major-available report,
    // because the asset is never consulted until consent is given.
    let major_without_asset =
        release_with_html_url("v2.0.0", false, false, json_empty(), release_url);
    assert_eq!(
        parse_latest_release(&major_without_asset, &current, "linux", false).unwrap(),
        ReleaseDecision::MajorAvailable(MajorRelease {
            version: Version::parse("2.0.0").unwrap(),
            release_url: release_url.to_owned(),
        })
    );
    // With consent, the same release validates and installs exactly like an
    // ordinary same-major update.
    let ReleaseDecision::Update(update) =
        parse_latest_release(&major, &current, "linux", true).unwrap()
    else {
        panic!("consented major release must validate for install")
    };
    assert_eq!(update.version, Version::parse("2.0.0").unwrap());
    assert_eq!(update.asset_name, "vosu-linux-v2.0.0.zip");

    // Same-major releases are unaffected by the consent flag either way.
    let minor = release(
        "v1.3.0",
        false,
        false,
        serde_json::json!([asset("vosu-linux-v1.3.0.zip", Some(&good_digest))]),
    );
    assert!(matches!(
        parse_latest_release(&minor, &current, "linux", false).unwrap(),
        ReleaseDecision::Update(_)
    ));
}

fn json_empty() -> serde_json::Value {
    serde_json::json!([])
}

/// The updater compares against and identifies as the executable's own
/// version (this integration test's `CARGO_PKG_VERSION` is the root
/// package's), never `vosu-core`'s unrelated package version: 1.2.5 compared
/// against 0.1.0 and offered its own release as a major update.
#[test]
fn running_version_is_the_root_package_version_and_its_own_release_is_not_an_update() {
    let root = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let current = vosu::runtime::current_version();
    assert_eq!(current, root);
    assert_eq!(
        user_agent(&current),
        format!("vosu/{}", env!("CARGO_PKG_VERSION"))
    );

    let good_digest = format!("sha256:{}", "ab".repeat(32));
    let published = |version: &Version| {
        release(
            &format!("v{version}"),
            false,
            false,
            serde_json::json!([
                asset(&format!("vosu-linux-v{version}.zip"), Some(&good_digest)),
                asset(&format!("vosu-windows-v{version}.zip"), Some(&good_digest)),
            ]),
        )
    };
    // The published release of the running version is never an update,
    // whatever the platform or major-update consent.
    for platform in ["linux", "windows"] {
        for major_consent in [false, true] {
            assert_eq!(
                parse_latest_release(&published(&root), &current, platform, major_consent).unwrap(),
                ReleaseDecision::Latest(root.clone())
            );
        }
    }
    // The next patch release still is one.
    let next = Version::new(root.major, root.minor, root.patch + 1);
    let ReleaseDecision::Update(update) =
        parse_latest_release(&published(&next), &current, "linux", false).unwrap()
    else {
        panic!("the next patch release must be an update")
    };
    assert_eq!(update.version, next);
}

#[test]
fn forced_build_and_internal_startup_roles_are_unambiguous_and_token_bound() {
    let forced = Version::parse("0.1.0-forced").unwrap();
    assert_eq!(
        parse_latest_release(
            &release("v9.0.0", false, false, json_empty()),
            &forced,
            "linux",
            false
        )
        .unwrap(),
        ReleaseDecision::SuppressedForced
    );
    assert_eq!(
        parse_startup_update_args([OsString::from("vosu"), OsString::from("--update=false")])
            .unwrap(),
        StartupIntent::Ordinary
    );
    assert!(
        parse_startup_update_args([OsString::from("vosu"), OsString::from("--update-helper")])
            .is_err()
    );
    assert!(
        parse_startup_update_args([
            OsString::from("vosu"),
            OsString::from("--update-candidate"),
            OsString::from("--update-record=/tmp/r"),
            OsString::from("--update-token=t"),
            OsString::from("--readiness-token=r"),
            OsString::from("--readiness-address=not-an-address"),
        ])
        .is_err()
    );
}

struct FakePlatform {
    alive: AtomicBool,
}
impl FakePlatform {
    fn new(alive: bool) -> Self {
        Self {
            alive: AtomicBool::new(alive),
        }
    }
}
impl UpdatePlatform for FakePlatform {
    fn current_identity(&self) -> io::Result<ProcessIdentity> {
        Ok(ProcessIdentity {
            pid: 101,
            start: 202,
        })
    }
    fn identity_for_pid(&self, pid: u32) -> io::Result<ProcessIdentity> {
        Ok(ProcessIdentity { pid, start: 202 })
    }
    fn identity_is_alive(&self, identity: &ProcessIdentity) -> io::Result<bool> {
        Ok(identity.pid == 101 && self.alive.load(Ordering::Acquire))
    }
    fn random_128(&self) -> io::Result<[u8; 16]> {
        Ok([0x5a; 16])
    }
    fn atomic_swap(&self, target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
        fs::copy(target, backup)?;
        fs::rename(staged, target)
    }
    fn restore_backup(&self, target: &Path, backup: &Path) -> io::Result<()> {
        if target.exists() {
            fs::remove_file(target)?;
        }
        fs::rename(backup, target)
    }
    fn spawn(&self, _executable: &Path, _args: &[OsString], _cwd: &Path) -> io::Result<Child> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "not used by contract",
        ))
    }
    fn terminate_and_wait(
        &self,
        _identity: &ProcessIdentity,
        _timeout: Duration,
    ) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn transaction_is_single_flight_durable_and_pre_handoff_cleanup_is_reentrant() {
    let root = TempDir::new().unwrap();
    let target = root
        .path()
        .join(if cfg!(windows) { "vosu.exe" } else { "vosu" });
    fs::write(&target, b"old executable").unwrap();
    let platform = FakePlatform::new(true);

    let mut first = TransactionGuard::begin(root.path(), &target, &platform).unwrap();
    assert_eq!(first.record().phase, InstallPhase::Created);
    assert!(root.path().join(INSTALL_RECORD_NAME).is_file());
    assert!(matches!(
        TransactionGuard::begin(root.path(), &target, &platform),
        Err(UpdateError::AlreadyRunning)
    ));
    first.set_metadata([7; 32]).unwrap();
    first.persist_phase(InstallPhase::Downloading).unwrap();
    let persisted = load_record(&root.path().join(INSTALL_RECORD_NAME)).unwrap();
    assert_eq!(persisted.phase, InstallPhase::Downloading);
    assert_eq!(persisted.archive_digest, Some("07".repeat(32)));
    first.cancel().unwrap();
    assert!(!root.path().join(INSTALL_RECORD_NAME).exists());
    assert!(
        !root
            .path()
            .join(format!(".vosu-update-work-{}", "5a".repeat(16)))
            .exists()
    );

    let second = TransactionGuard::begin(root.path(), &target, &platform).unwrap();
    drop(second);
    assert!(
        !root.path().join(INSTALL_RECORD_NAME).exists(),
        "ordinary error/drop makes second update possible"
    );
    assert_eq!(fs::read(&target).unwrap(), b"old executable");
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn record(root: &Path, phase: InstallPhase, target_bytes: Option<&[u8]>) -> InstallRecord {
    let target = root.join("vosu");
    if let Some(bytes) = target_bytes {
        fs::write(&target, bytes).unwrap();
    }
    let backup = root.join("backup");
    fs::write(&backup, b"old").unwrap();
    InstallRecord {
        nonce: "n".into(),
        token: "t".into(),
        owner: ProcessIdentity { pid: 1, start: 1 },
        helper: None,
        original_args: Vec::new(),
        working_directory: EncodedOsString::capture(root.as_os_str().to_owned()),
        target,
        old_digest: digest(b"old"),
        archive_digest: Some(digest(b"archive")),
        new_digest: Some(digest(b"new")),
        phase,
        work: root.join("work"),
        part: root.join("part"),
        staging: root.join("stage"),
        backup,
        helper_path: root.join("helper"),
    }
}

#[test]
fn recovery_table_never_overwrites_unknown_target_and_distinguishes_swap_cut() {
    let root = TempDir::new().unwrap();
    assert_eq!(
        classify_recovery(&record(
            root.path(),
            InstallPhase::Downloading,
            Some(b"old")
        ))
        .unwrap(),
        RecoveryAction::CleanAndContinue
    );
    assert_eq!(
        classify_recovery(&record(root.path(), InstallPhase::HandedOff, Some(b"new"))).unwrap(),
        RecoveryAction::ResumeCandidate
    );
    assert_eq!(
        classify_recovery(&record(root.path(), InstallPhase::Swapped, Some(b"old"))).unwrap(),
        RecoveryAction::CleanAndContinue
    );
    assert_eq!(
        classify_recovery(&record(root.path(), InstallPhase::Rollback, Some(b"new"))).unwrap(),
        RecoveryAction::RestoreRollback
    );
    assert_eq!(
        classify_recovery(&record(root.path(), InstallPhase::Ready, Some(b"new"))).unwrap(),
        RecoveryAction::FinishCleanup
    );
    assert_eq!(
        classify_recovery(&record(
            root.path(),
            InstallPhase::Prepared,
            Some(b"unknown modified executable")
        ))
        .unwrap(),
        RecoveryAction::Quarantine
    );

    let mut missing = record(root.path(), InstallPhase::Swapped, Some(b"new"));
    fs::remove_file(&missing.target).unwrap();
    assert_eq!(
        classify_recovery(&missing).unwrap(),
        RecoveryAction::RestoreRollback
    );
    missing.phase = InstallPhase::Created;
    assert_eq!(
        classify_recovery(&missing).unwrap(),
        RecoveryAction::Quarantine
    );
}
