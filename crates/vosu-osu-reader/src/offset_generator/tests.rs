use super::*;

#[test]
fn manifest_rejects_platform_and_unsafe_paths() {
    let bad = Manifest {
        rid: "wrong".into(),
        files: BTreeMap::new(),
    };
    assert!(validate_manifest(&bad).is_err());
    let mut files = BTreeMap::from([
        (GENERATOR.to_owned(), "0".repeat(64)),
        (TARGET_HOST.to_owned(), "0".repeat(64)),
    ]);
    files.insert("../escape".into(), "0".repeat(64));
    let bad = Manifest {
        rid: RID.into(),
        files,
    };
    assert!(validate_manifest(&bad).is_err());
}

#[test]
fn signature_is_merged_only_for_matching_version() {
    let generated = br#"{"OsuVersion":"2026.1.0","GameBaseVtable":1}"#;
    let signature = br#"{"OsuVersion":"2026.1.0","GameBaseVtable":1234}"#;
    let value: serde_json::Value =
        serde_json::from_slice(&merge_game_base_vtable(generated, signature).unwrap()).unwrap();
    assert_eq!(value["GameBaseVtable"], 1234);
    let mismatch = br#"{"OsuVersion":"2026.2.0","GameBaseVtable":1234}"#;
    assert!(merge_game_base_vtable(generated, mismatch).is_err());
}

#[test]
fn signature_rejects_zero_vtable() {
    let generated = br#"{"OsuVersion":"2026.1.0"}"#;
    let signature = br#"{"OsuVersion":"2026.1.0","GameBaseVtable":0}"#;
    assert!(merge_game_base_vtable(generated, signature).is_err());
}

#[test]
fn checksum_requires_sha256_text() {
    assert_eq!(
        checksum(b"ABCDEF\n").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let digest = "a".repeat(64);
    assert_eq!(checksum(digest.as_bytes()).unwrap(), digest);
}

fn fixture_bundle() -> (Vec<u8>, String) {
    use std::io::Write;
    let files = [
        (GENERATOR, b"generator".as_slice()),
        (TARGET_HOST, b"target".as_slice()),
    ];
    let manifest = serde_json::json!({
        "rid": RID,
        "files": files.iter().map(|(name, content)| {
            ((*name).to_owned(), hash_reader(*content, MAX_ARCHIVE).unwrap())
        }).collect::<BTreeMap<_, _>>()
    });
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content).unwrap();
    }
    zip.start_file("bundle.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let digest = hash_reader(bytes.as_slice(), MAX_ARCHIVE).unwrap();
    (bytes, digest)
}

#[test]
fn embedded_checksum_anchors_extraction_and_verified_reuse() {
    let root = tempfile::tempdir().unwrap();
    let (archive, checksum) = fixture_bundle();
    let cancel = CancellationToken::new();
    let payload = install_bundle(root.path(), &archive, &checksum, &cancel).unwrap();
    let executable = payload.join(GENERATOR);
    let modified = fs::metadata(&executable).unwrap().modified().unwrap();
    assert_eq!(fs::read(&executable).unwrap(), b"generator");
    assert_eq!(
        install_bundle(root.path(), &archive, &checksum, &cancel).unwrap(),
        payload
    );
    assert_eq!(
        fs::metadata(&executable).unwrap().modified().unwrap(),
        modified
    );
    fs::write(&executable, b"corrupt").unwrap();
    assert_eq!(
        install_bundle(root.path(), &archive, &checksum, &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        install_bundle(root.path(), &archive, &"0".repeat(64), &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn concurrent_extraction_publishes_only_a_complete_verified_payload() {
    let root = tempfile::tempdir().unwrap();
    let (archive, checksum) = fixture_bundle();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let install = || {
            barrier.wait();
            install_bundle(root.path(), &archive, &checksum, &CancellationToken::new()).unwrap()
        };
        let left = scope.spawn(install);
        let right = scope.spawn(install);
        let payload = left.join().unwrap();
        assert_eq!(right.join().unwrap(), payload);
        assert_eq!(fs::read(payload.join(GENERATOR)).unwrap(), b"generator");
        assert_eq!(fs::read(payload.join(TARGET_HOST)).unwrap(), b"target");
    });
}

#[test]
fn cancelled_extraction_does_not_create_a_cache() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let (archive, checksum) = fixture_bundle();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        install_bundle(&cache, &archive, &checksum, &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(!cache.exists());
}
