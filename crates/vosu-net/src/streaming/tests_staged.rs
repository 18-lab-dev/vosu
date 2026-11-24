//! Staged (unpublished) extraction and portable-name rules shared by every installer.
use super::*;
use std::io::Write;

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn handle(bytes: &[u8]) -> std::fs::File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file
}

fn limits() -> ZipLimits {
    ZipLimits {
        max_entries: 100,
        max_expanded_bytes: 1024 * 1024,
    }
}

fn stage(entries: &[(&str, &[u8])], parent: &Path) -> Result<StagedTree, NetworkError> {
    extract_zip_staged(
        handle(&zip_bytes(entries)),
        parent,
        limits(),
        &CancellationToken::new(),
    )
}

fn leftovers(parent: &Path) -> usize {
    std::fs::read_dir(parent).map_or(0, Iterator::count)
}

#[test]
fn staged_extraction_publishes_nothing_and_cleans_up_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let staged = stage(
        &[
            ("manifest.toml", b"kind = 'tool'"),
            ("web/index.html", b"<p>"),
        ],
        root.path(),
    )
    .unwrap();
    assert!(staged.path().starts_with(root.path()));
    assert_eq!(
        std::fs::read(staged.path().join("web/index.html")).unwrap(),
        b"<p>"
    );
    assert_eq!(leftovers(root.path()), 1);
    drop(staged);
    assert_eq!(leftovers(root.path()), 0);
}

#[test]
fn windows_case_and_reserved_name_collisions_are_refused_on_every_platform() {
    let root = tempfile::tempdir().unwrap();
    // Positive control: distinct portable names and a repeated directory.
    assert!(
        stage(
            &[("schemas/a.json", b"1"), ("schemas/b.json", b"2")],
            root.path()
        )
        .is_ok()
    );
    for (entries, expected) in [
        (
            vec![("Schema.json", &b"1"[..]), ("schema.json", b"2")],
            "portable name collision",
        ),
        (
            vec![("Dir/a.json", &b"1"[..]), ("dir/b.json", b"2")],
            "portable name collision",
        ),
        (
            vec![
                ("\u{c9}t\u{e9}.json", &b"1"[..]),
                ("\u{e9}t\u{e9}.json", b"2"),
            ],
            "portable name collision",
        ),
        (vec![("CON.json", &b"1"[..])], "nonportable entry name"),
        (vec![("schemas/aux", &b"1"[..])], "nonportable entry name"),
        (vec![("lpt\u{b9}.txt", &b"1"[..])], "nonportable entry name"),
        (vec![("com7", &b"1"[..])], "nonportable entry name"),
        (vec![("trailing.", &b"1"[..])], "nonportable entry name"),
        (vec![("space ", &b"1"[..])], "nonportable entry name"),
    ] {
        let error = stage(&entries, root.path()).unwrap_err();
        assert!(
            matches!(&error, NetworkError::UnsafeZip(message) if message == expected),
            "{entries:?}: {error}"
        );
        assert_eq!(leftovers(root.path()), 0, "{entries:?}");
    }
    // Device names only match whole stems; a longer name is ordinary.
    assert!(stage(&[("console.json", b"1"), ("com10", b"2")], root.path()).is_ok());
}

#[test]
fn repeated_file_entry_is_refused_instead_of_overwritten() {
    let mut bytes = zip_bytes(&[("a.json", b"first"), ("b.json", b"other")]);
    // Rename the second entry in both local and central headers to `a.json`.
    let mut renamed = 0;
    for index in 0..bytes.len() - 6 {
        if &bytes[index..index + 6] == b"b.json" {
            bytes[index] = b'a';
            renamed += 1;
        }
    }
    assert_eq!(renamed, 2);
    let root = tempfile::tempdir().unwrap();
    let result = extract_zip_staged(
        handle(&bytes),
        root.path(),
        limits(),
        &CancellationToken::new(),
    );
    assert!(result.is_err(), "duplicate names must not be accepted");
    assert_eq!(leftovers(root.path()), 0);
}

#[test]
fn late_crc_error_in_the_last_entry_fails_before_any_tree_survives() {
    let body = b"late-entry-body-checked-only-at-its-end";
    let good = zip_bytes(&[("manifest.toml", b"kind = 'tool'"), ("last.bin", body)]);
    let root = tempfile::tempdir().unwrap();
    // Positive control: the untouched archive extracts.
    assert!(
        extract_zip_staged(
            handle(&good),
            root.path(),
            limits(),
            &CancellationToken::new()
        )
        .is_ok()
    );
    let mut bad = good.clone();
    let crc = zip::ZipArchive::new(std::io::Cursor::new(&good))
        .unwrap()
        .by_index(1)
        .unwrap()
        .crc32()
        .to_le_bytes();
    let mut patched = 0;
    for index in 0..bad.len() - 4 {
        if bad[index..index + 4] == crc {
            bad[index] ^= 0xff;
            patched += 1;
        }
    }
    assert_eq!(patched, 2, "local and central CRC fields");
    let error = extract_zip_staged(
        handle(&bad),
        root.path(),
        limits(),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        matches!(error, NetworkError::Io(_) | NetworkError::Zip(_)),
        "{error}"
    );
    assert_eq!(leftovers(root.path()), 0);
}

#[test]
fn no_replace_rename_refuses_an_existing_destination() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&destination).unwrap();
    assert!(rename_directory_no_replace(&source, &destination).is_err());
    assert!(source.is_dir());
    std::fs::remove_dir(&destination).unwrap();
    rename_directory_no_replace(&source, &destination).unwrap();
    assert!(!source.exists() && destination.is_dir());
}
