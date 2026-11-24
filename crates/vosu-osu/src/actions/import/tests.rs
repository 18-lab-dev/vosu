#![expect(
    clippy::disallowed_methods,
    reason = "isolated temporary test fixtures"
)]

use std::io::Write;
use std::path::Path;

use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use vosu_plugin_api::{Bitness, SessionAttached};
use zip::write::SimpleFileOptions;

use super::*;

const CHART: &[u8] = b"osu file format v14\n\n[General]\nMode:0\n\n[Metadata]\nTitle:Import sentinel\nArtist:Distinct\nCreator:Fixture\nVersion:Bounded\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:6\nApproachRate:7\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,1,100,1,0\n\n[HitObjects]\n256,192,1000,1,0,0:0:0:0:\n";

fn archive(root: &TempDir, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
    let source = root.path().join("private-source-sentinel.osz");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
    for (name, bytes) in entries {
        zip.start_file(
            *name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    source
}
fn validated(root: &TempDir) -> ValidatedArchive {
    let source = archive(
        root,
        &[
            ("set/chart.osu", CHART),
            ("set/audio.mp3", b"audio sentinel"),
        ],
    );
    ValidatedArchive::open(&source, ArchiveLimits::default(), &CancellationToken::new()).unwrap()
}
fn assert_invalid(source: &Path, limits: ArchiveLimits) {
    assert!(ValidatedArchive::open(source, limits, &CancellationToken::new()).is_err());
}

#[test]
fn bounded_archive_rejects_cross_platform_paths_and_no_chart() {
    let root = tempfile::tempdir().unwrap();
    for name in [
        "../escape.osu",
        "/absolute.osu",
        "C:/drive.osu",
        "a\\..\\escape.osu",
        "normal:stream.osu",
    ] {
        let source = archive(&root, &[(name, CHART)]);
        assert_invalid(&source, ArchiveLimits::default());
    }
    for bytes in [
        b"not a chart".as_slice(),
        b"osu file format v14\n".as_slice(),
    ] {
        let source = archive(&root, &[("pretend.osu", bytes)]);
        assert_invalid(&source, ArchiveLimits::default());
    }
    let source = archive(&root, &[("audio.mp3", CHART)]);
    assert_invalid(&source, ArchiveLimits::default());
}

#[test]
fn bounded_archive_checks_each_budget_and_crc() {
    let root = tempfile::tempdir().unwrap();
    let source = archive(&root, &[("chart.osu", CHART), ("extra", b"extra")]);
    // Each limit is the only failing dimension, with a valid positive control.
    ValidatedArchive::open(&source, ArchiveLimits::default(), &CancellationToken::new()).unwrap();
    for limits in [
        ArchiveLimits {
            compressed_bytes: 20,
            ..ArchiveLimits::default()
        },
        ArchiveLimits {
            expanded_bytes: CHART.len() as u64 - 1,
            ..ArchiveLimits::default()
        },
        ArchiveLimits {
            entries: 1,
            ..ArchiveLimits::default()
        },
        ArchiveLimits {
            chart_bytes: CHART.len() as u64 - 1,
            ..ArchiveLimits::default()
        },
    ] {
        assert_invalid(&source, limits);
    }
    let mut bytes = std::fs::read(&source).unwrap();
    let index = bytes
        .windows(CHART.len())
        .position(|window| window == CHART)
        .unwrap();
    bytes[index + 30] ^= 1;
    std::fs::write(&source, bytes).unwrap();
    assert_invalid(&source, ArchiveLimits::default());
}

#[test]
fn bounded_archive_rejects_symlink_and_malformed_zip() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("link.osz");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
    zip.add_symlink("chart.osu", "outside", SimpleFileOptions::default())
        .unwrap();
    zip.finish().unwrap();
    assert_invalid(&source, ArchiveLimits::default());
    std::fs::write(&source, b"PK\x03\x04 truncated private-path-sentinel").unwrap();
    let error =
        ValidatedArchive::open(&source, ArchiveLimits::default(), &CancellationToken::new())
            .err()
            .unwrap();
    assert_eq!(error, ImportError::InvalidArchive);
    assert!(!error.to_string().contains("private-path-sentinel"));
    assert!(!error.to_string().contains(root.path().to_str().unwrap()));
}

#[test]
fn stable_uses_snapshot_selected_root_and_exact_result() {
    let root = tempfile::tempdir().unwrap();
    let mut validated = validated(&root);
    let original = std::fs::read(validated.snapshot.path()).unwrap();
    std::fs::write(
        root.path().join("private-source-sentinel.osz"),
        b"replaced after validation",
    )
    .unwrap();
    let selected = root.path().join("selected Songs with spaces");
    let other = root.path().join("unselected Songs");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&other).unwrap();
    let cancel = CancellationToken::new();
    let stage = StableStage::prepare(&mut validated, &selected, &cancel).unwrap();
    let destination = stage.destination().to_owned();
    assert_eq!(stage.commit(&cancel).unwrap(), ImportResult::queued());
    cancel.cancel(); // After the commit boundary, success and its artifact stand.
    assert_eq!(std::fs::read(destination).unwrap(), original);
    assert_eq!(std::fs::read_dir(other).unwrap().count(), 0);
    assert_eq!(
        serde_json::to_value(ImportResult::queued()).unwrap(),
        serde_json::json!({"client":"stable","disposition":"queued","refreshRequired":true})
    );
}

#[test]
fn stable_no_clobber_and_cancellation_remove_stages() {
    let root = tempfile::tempdir().unwrap();
    let mut validated = validated(&root);
    let songs = root.path().join("Songs");
    std::fs::create_dir(&songs).unwrap();
    let cancel = CancellationToken::new();
    let stage = StableStage::prepare(&mut validated, &songs, &cancel).unwrap();
    let destination = stage.destination().to_owned();
    std::fs::write(&destination, b"existing archive sentinel").unwrap();
    assert_eq!(stage.commit(&cancel), Err(ImportError::DestinationExists));
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"existing archive sentinel"
    );
    assert_eq!(std::fs::read_dir(&songs).unwrap().count(), 1);
    std::fs::remove_file(destination).unwrap();
    let stage = StableStage::prepare(&mut validated, &songs, &cancel).unwrap();
    cancel.cancel();
    assert_eq!(stage.commit(&cancel), Err(ImportError::Cancelled));
    assert_eq!(std::fs::read_dir(&songs).unwrap().count(), 0);
    assert!(matches!(
        StableStage::prepare(&mut validated, &songs, &cancel),
        Err(ImportError::Cancelled)
    ));
    let source = root.path().join("private-source-sentinel.osz");
    assert!(matches!(
        ValidatedArchive::open(&source, ArchiveLimits::default(), &cancel),
        Err(ImportError::Cancelled)
    ));
}

fn attached(path: &Path) -> SessionAttached {
    SessionAttached {
        session: "selected-lazer-913".into(),
        handle: 913,
        generation: 47,
        pid: 9_100_913,
        bitness: Bitness::X64,
        name: Some("osu!".into()),
        path: Some(path.to_str().unwrap().into()),
        command_line: None,
        argv: None,
    }
}
fn target(root: &TempDir, executable: &Path) -> LazerTarget {
    LazerTarget::from_attached(
        &attached(executable),
        root.path().to_owned(),
        vec![("SELECTED_CONTEXT".into(), "client-913".into())],
    )
    .unwrap()
}

#[test]
fn lazer_handoff_construction_uses_selected_executable_environment_and_storage() {
    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("selected client.exe");
    let unselected = root.path().join("default client.exe");
    std::fs::write(&selected, b"selected").unwrap();
    std::fs::write(unselected, b"default").unwrap();
    let target = target(&root, &selected);
    let archive = root.path().join("validated archive.osz");
    let command = target.command(&archive);
    let command = command.as_std();
    assert_eq!(command.get_program(), selected.as_os_str());
    assert_eq!(command.get_current_dir(), Some(root.path()));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec![archive.as_os_str()]
    );
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        vec![(
            std::ffi::OsStr::new("SELECTED_CONTEXT"),
            Some(std::ffi::OsStr::new("client-913"))
        )]
    );
    let mut no_path = attached(&selected);
    no_path.path = None;
    assert!(matches!(
        LazerTarget::from_attached(&no_path, root.path().into(), vec![("K".into(), "V".into())],),
        Err(ImportError::InvalidTarget)
    ));
    let mut tournament = attached(&selected);
    tournament.argv = Some(vec!["--tournament".into()]);
    assert!(matches!(
        LazerTarget::from_attached(
            &tournament,
            root.path().into(),
            vec![("K".into(), "V".into())]
        ),
        Err(ImportError::InvalidTarget)
    ));
    assert_eq!(
        serde_json::to_value(ImportResult::submitted()).unwrap(),
        serde_json::json!({"client":"lazer","disposition":"submitted","refreshRequired":false})
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lazer_submission_reaps_selected_child_and_keeps_snapshot_until_exit() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    // A dedicated fixture script, not a renamed coreutils executable. It checks
    // the archive still exists when the child consumes it and that env is exact.
    let executable = root.path().join("selected lazer fixture");
    std::fs::write(
        &executable,
        b"#!/bin/sh\n[ \"$SELECTED_CONTEXT\" = client-913 ] && [ \"$#\" = 1 ] && [ -s \"$1\" ]\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let archive = validated(&root);
    let snapshot = archive.snapshot.path().to_owned();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        target(&root, &executable).submit(archive, &CancellationToken::new()),
    )
    .await
    .unwrap();
    assert_eq!(result, Ok(ImportResult::submitted()));
    assert!(!snapshot.exists());
    let archive = validated(&root);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            target(&root, &executable).submit(archive, &cancel)
        )
        .await
        .unwrap(),
        Err(ImportError::Cancelled)
    );
}
