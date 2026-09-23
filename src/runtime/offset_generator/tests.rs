use super::*;
use std::io::Write;

fn fixture_bundle(directory: &Path, rid: &str) -> PathBuf {
    fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{BUNDLE_TAG}-{RID}.zip"));
    let mut archive = zip::ZipWriter::new(File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    let contents = [
        (GENERATOR, b"helper".as_slice()),
        (
            "generator/lazer-offset-generator.dll",
            b"managed".as_slice(),
        ),
        (TARGET_HOST, b"target".as_slice()),
    ];
    let files: BTreeMap<_, _> = contents
        .iter()
        .map(|(name, bytes)| (*name, hash_reader(*bytes, 1024).unwrap()))
        .collect();
    for (name, bytes) in contents {
        archive.start_file(name, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.start_file("bundle.json", options).unwrap();
    serde_json::to_writer(
        &mut archive,
        &serde_json::json!({
            "rid": rid, "files": files,
        }),
    )
    .unwrap();
    archive.finish().unwrap();
    fs::write(
        directory.join(format!("{BUNDLE_TAG}-{RID}.zip.sha256")),
        file_hash(&path, MAX_ARCHIVE).unwrap(),
    )
    .unwrap();
    path
}

#[tokio::test]
async fn verified_bundle_survives_source_removal_and_rejects_cache_corruption() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let cache = root.path().join("cache");
    fixture_bundle(&source, RID);
    let network = StreamingClient::production();
    let first = obtain_bundle(&cache, Some(&source), &network)
        .await
        .unwrap();
    assert_eq!(
        fs::read(first.join("generator/lazer-offset-generator.dll")).unwrap(),
        b"managed"
    );
    fs::remove_dir_all(&source).unwrap();
    let second = obtain_bundle(&cache, Some(&source), &network)
        .await
        .unwrap();
    assert_eq!(fs::read(second.join(GENERATOR)).unwrap(), b"helper");
    fs::write(second.join(TARGET_HOST), b"corrupt").unwrap();
    let error = obtain_bundle(&cache, Some(&source), &network)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("remove this cache directory"));
}

#[tokio::test]
async fn mismatched_platform_never_installs_executable_payload() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let cache = root.path().join("cache");
    fixture_bundle(&source, "unsupported-rid");
    let error = obtain_bundle(&cache, Some(&source), &StreamingClient::production())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!cache.join("payload").exists());
    assert!(!cache.join(format!("{BUNDLE_TAG}-{RID}.zip")).exists());
}

#[tokio::test]
async fn corrupt_download_does_not_publish_executable_payload() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let cache = root.path().join("cache");
    let archive = fixture_bundle(&source, RID);
    fs::write(archive, b"truncated download").unwrap();
    let error = obtain_bundle(&cache, Some(&source), &StreamingClient::production())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!cache.join("payload").exists());
    assert!(!cache.join(format!("{BUNDLE_TAG}-{RID}.zip")).exists());
}

#[test]
fn manifest_rejects_paths_that_escape_or_alias_platform_roots() {
    for path in ["../outside", "/outside", "C:/outside", "dac\\..\\outside"] {
        let manifest = Manifest {
            rid: RID.to_owned(),
            files: [
                (GENERATOR.to_owned(), "a".repeat(64)),
                ("lazer-offset-generator.dll".to_owned(), "b".repeat(64)),
                (TARGET_HOST.to_owned(), "d".repeat(64)),
                (path.to_owned(), "c".repeat(64)),
            ]
            .into(),
        };
        assert_eq!(
            validate_manifest(&manifest).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn client_discovery_requires_exactly_one_candidate() {
    let make = |pid| Client {
        pid,
        version: "2026.726.0".into(),
        root: PathBuf::from("client"),
    };
    assert_eq!(
        select_client(Vec::new()).err().unwrap().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(select_client(vec![make(10)]).unwrap().pid, 10);
    let error = select_client(vec![make(10), make(11)]).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("10, 11"));
}

#[test]
fn generated_fields_require_the_same_builds_measured_signature() {
    let generated = br#"{"OsuVersion":"2026.921.0","field":42}"#;
    let signature = br#"{"OsuVersion":"2026.921.0","GameBaseVtable":456}"#;
    let merged: serde_json::Value =
        serde_json::from_slice(&merge_game_base_vtable(generated, signature).unwrap()).unwrap();
    assert_eq!(
        merged,
        serde_json::json!({"OsuVersion":"2026.921.0","field":42,"GameBaseVtable":456})
    );
    for invalid in [
        br#"{"OsuVersion":"2026.921.0"}"#.as_slice(),
        br#"{"OsuVersion":"2026.921.0","GameBaseVtable":0}"#.as_slice(),
    ] {
        assert_eq!(
            merge_game_base_vtable(generated, invalid)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn game_base_signature_cannot_cross_client_versions() {
    let generated = br#"{"OsuVersion":"2026.921.0","field":42}"#;
    let prior_version = br#"{"OsuVersion":"2026.726.0","GameBaseVtable":123}"#;
    assert_eq!(
        merge_game_base_vtable(generated, prior_version)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn assembly_discovery_uses_the_client_installation_and_reports_missing_files() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        assembly_directory(root.path()).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    let bin = root.path().join("usr/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("osu.Game.dll"), b"fixture").unwrap();
    assert_eq!(assembly_directory(root.path()).unwrap(), bin);
}
