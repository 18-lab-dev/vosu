use super::*;
use crate::config::SystemAtomicReplace;
use tempfile::TempDir;

#[test]
fn prerelease_import_preserves_sources_and_never_reads_them_again() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("vosu.toml");
    let legacy = root.path().join("vosu.env");
    let old = root.path().join("tosu.env");
    let toml = "SERVER_PORT = 24413.0\nPOLL_RATE = 413.0\nUNKNOWN_SENTINEL = 'preserve me'\nSTATIC_FOLDER_PATH = 'C:\\normal\\writable'\n";
    fs::write(&source, toml).unwrap();
    fs::write(&legacy, "SERVER_PORT=24312\nDEBUG_LOG=true\n").unwrap();
    fs::write(&old, "SERVER_PORT=24211\nENABLE_AUTOUPDATE=false\n").unwrap();
    fs::write(root.path().join("broker.json"), "private broker credential").unwrap();
    fs::write(root.path().join("resource-secret"), "private secret").unwrap();
    let target = root.path().join("prerelease/vosu.toml");
    let imported = initialize(&target, &source, &SystemAtomicReplace).unwrap();
    assert_eq!(imported.config.server_port, 24413.0);
    assert!(imported.config.debug_log);
    assert!(!imported.config.enable_auto_update);
    let written = fs::read_to_string(&target).unwrap();
    assert!(written.contains("UNKNOWN_SENTINEL = \"preserve me\""));
    assert!(!written.contains("POLL_RATE"));
    assert!(!written.contains("STATIC_FOLDER_PATH"));
    assert_eq!(fs::read_to_string(&source).unwrap(), toml);
    assert_eq!(
        fs::read_to_string(&legacy).unwrap(),
        "SERVER_PORT=24312\nDEBUG_LOG=true\n"
    );
    assert_eq!(
        fs::read_to_string(&old).unwrap(),
        "SERVER_PORT=24211\nENABLE_AUTOUPDATE=false\n"
    );
    assert!(!target.parent().unwrap().join("broker.json").exists());
    assert!(!target.parent().unwrap().join("resource-secret").exists());
    // A directory instead of the source makes an accidental restart read fail.
    fs::remove_file(&source).unwrap();
    fs::create_dir(&source).unwrap();
    fs::remove_file(&legacy).unwrap();
    fs::create_dir(&legacy).unwrap();
    let restarted = initialize(&target, &source, &SystemAtomicReplace).unwrap();
    assert_eq!(restarted.config, imported.config);
}

#[test]
fn prerelease_alpha_inputs_win_and_keep_explicit_core_override() {
    let root = TempDir::new().unwrap();
    let target = root.path().join("prerelease/vosu.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(
        target.parent().unwrap().join("vosu.env"),
        "SERVER_PORT=24723\nALLOWED_IPS=127.0.0.1\n",
    )
    .unwrap();
    let unavailable = root.path().join("unavailable/vosu.toml");
    fs::create_dir_all(&unavailable).unwrap();
    let local = initialize(&target, &unavailable, &SystemAtomicReplace).unwrap();
    assert_eq!(local.config.server_port, 24723.0);
    assert_eq!(local.config.allowed_ips, "127.0.0.1");
}

#[test]
fn prerelease_malformed_source_leaves_both_trees_untouched() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("vosu.toml");
    let legacy = root.path().join("vosu.env");
    fs::write(&source, "SERVER_PORT = [broken").unwrap();
    fs::write(&legacy, "DEBUG_LOG=true").unwrap();
    let target = root.path().join("prerelease/vosu.toml");
    assert_eq!(
        initialize(&target, &source, &SystemAtomicReplace)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(!target.parent().unwrap().exists());
    assert_eq!(fs::read_to_string(source).unwrap(), "SERVER_PORT = [broken");
    assert_eq!(fs::read_to_string(legacy).unwrap(), "DEBUG_LOG=true");
}

#[test]
fn prerelease_missing_source_publishes_defaults_once() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("vosu.toml");
    let target = root.path().join("prerelease/vosu.toml");
    let first = initialize(&target, &source, &SystemAtomicReplace).unwrap();
    assert_eq!(first.config, crate::config::Config::default());
    fs::write(&source, "SERVER_PORT=24987.0").unwrap();
    let next = initialize(&target, &source, &SystemAtomicReplace).unwrap();
    assert_eq!(next.config.server_port, 24_050.0);
}

#[test]
fn prerelease_legacy_source_precedence_and_publication_failure_are_nondestructive() {
    struct Refuse;
    impl AtomicReplace for Refuse {
        fn replace(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture"))
        }
    }

    let root = TempDir::new().unwrap();
    let source = root.path().join("vosu.toml");
    let legacy = root.path().join("vosu.env");
    let older = root.path().join("tosu.env");
    fs::write(&legacy, "SERVER_PORT=24532\n").unwrap();
    fs::write(&older, "SERVER_PORT=24321\nDEBUG_LOG=true\n").unwrap();
    let target = root.path().join("prerelease/vosu.toml");
    assert_eq!(
        initialize(&target, &source, &Refuse).err().unwrap().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!target.exists());
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&legacy).unwrap(), "SERVER_PORT=24532\n");
    assert_eq!(
        fs::read_to_string(&older).unwrap(),
        "SERVER_PORT=24321\nDEBUG_LOG=true\n"
    );
    let retried = initialize(&target, &source, &SystemAtomicReplace).unwrap();
    assert_eq!(retried.config.server_port, 24532.0);
    assert!(retried.config.debug_log);
}

#[test]
fn prerelease_unreadable_source_does_not_publish_or_fall_back() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("vosu.toml");
    fs::create_dir(&source).unwrap();
    let target = root.path().join("prerelease/vosu.toml");
    assert!(initialize(&target, &source, &SystemAtomicReplace).is_err());
    assert!(!target.parent().unwrap().exists());
}

#[test]
fn prerelease_invalid_alpha_config_never_falls_back_to_source() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("vosu.toml");
    fs::create_dir(&source).unwrap();
    let target = root.path().join("prerelease/vosu.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let invalid = "SERVER_PORT = [invalid alpha";
    fs::write(&target, invalid).unwrap();
    assert_eq!(
        initialize(&target, &source, &SystemAtomicReplace)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read_to_string(target).unwrap(), invalid);
}
