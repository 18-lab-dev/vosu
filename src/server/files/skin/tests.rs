use super::*;

// Make metadata-only comparisons equal regardless of the filesystem's clock,
// directory-size convention, or deferred NTFS timestamp updates. Membership
// signatures must still detect additions and removals.
fn mask_directory_stamp_changes(cache: &mut SkinIndexCache, root: &Path) {
    let cached = cache.current.as_mut().unwrap();
    let index = Arc::get_mut(&mut cached.index).unwrap();
    for &offset in index.directories.values().flatten() {
        let (path, stamp) = &mut index.stamps[offset];
        let metadata = fs::metadata(root.join(path)).unwrap();
        stamp.size = metadata.len();
        stamp.modified = metadata.modified().ok();
    }
}

#[test]
fn directory_membership_invalidates_when_all_directory_metadata_matches() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("mania")).unwrap();
    fs::write(root.path().join("mania/note.png"), b"note").unwrap();
    let mut cache = SkinIndexCache::default();
    drop(cache.get(Some(root.path()), None, None, false, true));
    fs::write(root.path().join("mania/new.png"), b"new").unwrap();
    mask_directory_stamp_changes(&mut cache, root.path());
    assert_eq!(
        cache
            .get(Some(root.path()), None, None, false, true)
            .entries
            .len(),
        2
    );
    fs::remove_file(root.path().join("mania/new.png")).unwrap();
    mask_directory_stamp_changes(&mut cache, root.path());
    assert_eq!(
        cache
            .get(Some(root.path()), None, None, false, true)
            .entries
            .len(),
        1
    );
    fs::write(root.path().join("mania/added.png"), b"added").unwrap();
    mask_directory_stamp_changes(&mut cache, root.path());
    assert!(matches!(
        cache
            .resolve_folder(
                Some(root.path()),
                None,
                "MANIA/ADDED.PNG",
                "",
                MissingKind::NotFound
            )
            .unwrap(),
        ResourceOutcome::File(_)
    ));
    fs::create_dir(root.path().join("new-folder")).unwrap();
    fs::write(root.path().join("new-folder/new.png"), b"new folder").unwrap();
    mask_directory_stamp_changes(&mut cache, root.path());
    assert_eq!(
        cache
            .get(Some(root.path()), None, None, false, true)
            .entries
            .len(),
        3
    );
}

#[test]
#[ignore = "filesystem cost sample; run alone with --nocapture"]
fn skin_index_cost_for_1500_files() {
    use std::time::Instant;
    let root = tempfile::tempdir().unwrap();
    for i in 0..1500 {
        fs::write(root.path().join(format!("note-{i:04}.png")), b"asset").unwrap();
    }
    let mut cache = SkinIndexCache::default();
    let start = Instant::now();
    let first = cache.get(Some(root.path()), None, None, false, true);
    let cold = start.elapsed();
    assert_eq!(first.entries.len(), 1500);
    let start = Instant::now();
    for _ in 0..30 {
        assert!(Arc::ptr_eq(
            &first,
            &cache.get(Some(root.path()), None, None, false, true)
        ));
    }
    let validated = start.elapsed() / 30;
    let start = Instant::now();
    for _ in 0..30 {
        assert!(matches!(
            cache
                .resolve_folder(
                    Some(root.path()),
                    None,
                    "absent.png",
                    "",
                    MissingKind::NotFound
                )
                .unwrap(),
            ResourceOutcome::Missing(_)
        ));
    }
    let negative = start.elapsed() / 30;
    println!(
        "skin_index_1500 cold_us={} validated_report_us={} absent_lookup_us={}",
        cold.as_micros(),
        validated.as_micros(),
        negative.as_micros()
    );
}

#[test]
fn folder_listing_is_relative_cached_and_invalidates_files_directories_and_identity() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("mania")).unwrap();
    fs::write(root.path().join("mania/note.png"), b"note").unwrap();
    let mut cache = SkinIndexCache::default();
    let first = cache.get(Some(root.path()), None, Some("one"), false, true);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].path, "mania/note.png");
    assert_eq!(first.entries[0].size, 4);
    assert!(first.entries[0].modified.is_some());
    assert!(first.entries[0].hash.is_none());
    assert!(!first.truncated);
    assert!(Arc::ptr_eq(
        &first,
        &cache.get(Some(root.path()), None, Some("one"), false, true)
    ));
    fs::write(root.path().join("mania/note.png"), b"changed").unwrap();
    let changed = cache.get(Some(root.path()), None, Some("one"), false, true);
    assert!(!Arc::ptr_eq(&first, &changed));
    assert_eq!(changed.entries[0].size, 7);
    fs::write(root.path().join("mania/new.png"), b"new").unwrap();
    assert_eq!(
        cache
            .get(Some(root.path()), None, Some("one"), false, true)
            .entries
            .len(),
        2
    );
    fs::remove_file(root.path().join("mania/new.png")).unwrap();
    let removed = cache.get(Some(root.path()), None, Some("one"), false, true);
    assert_eq!(removed.entries.len(), 1);
    let identity = cache.get(Some(root.path()), None, Some("two"), false, true);
    assert!(!Arc::ptr_eq(&removed, &identity));
    let other = tempfile::tempdir().unwrap();
    assert!(
        cache
            .get(Some(other.path()), None, Some("two"), false, true)
            .entries
            .is_empty()
    );
}

#[test]
fn folder_case_lookup_handles_subfolders_new_files_and_traversal() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("mania")).unwrap();
    fs::write(root.path().join("mania/note.png"), b"note").unwrap();
    let mut cache = SkinIndexCache::default();
    let request = |cache: &mut SkinIndexCache, path: &str| {
        cache
            .resolve_folder(
                Some(root.path()),
                Some("skin"),
                path,
                path,
                MissingKind::NotFound,
            )
            .unwrap()
    };
    assert!(matches!(
        request(&mut cache, "Mania/NOTE.PNG"),
        ResourceOutcome::File(_)
    ));
    fs::write(root.path().join("mania/new.png"), b"new").unwrap();
    assert!(matches!(
        request(&mut cache, "MANIA/NEW.PNG"),
        ResourceOutcome::File(_)
    ));
    for path in [
        "../note.png",
        "%2e%2e/note.png",
        "mania/%2e%2e/note.png",
        "%2Fetc/passwd",
        "C:%5cfile",
        "mania%5c..%5cnote.png",
        "bad%GG",
        "a%00b",
    ] {
        assert!(
            matches!(
                request(&mut cache, path),
                ResourceOutcome::Missing(MissingKind::NotFound)
            ),
            "{path}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn folder_ambiguity_prefers_exact_then_lexicographically_first_confined_file() {
    use std::io::Read as _;
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("NOTE.PNG"), b"upper").unwrap();
    fs::write(root.path().join("note.png"), b"lower").unwrap();
    let mut cache = SkinIndexCache::default();
    for (path, expected) in [("note.png", "lower"), ("NoTe.PnG", "upper")] {
        let ResourceOutcome::File(mut file) = cache
            .resolve_folder(Some(root.path()), None, path, path, MissingKind::NotFound)
            .unwrap()
        else {
            panic!("file expected");
        };
        let mut content = String::new();
        file.file.read_to_string(&mut content).unwrap();
        assert_eq!(content, expected);
    }
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"private").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("leak.png"))
        .unwrap();
    let index = cache.get(Some(root.path()), None, None, false, true);
    assert!(!index.entries.iter().any(|e| e.path == "leak.png"));
    assert!(matches!(
        cache
            .resolve_folder(
                Some(root.path()),
                None,
                "LEAK.PNG",
                "LEAK.PNG",
                MissingKind::NotFound
            )
            .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotFound)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn cached_case_lookup_invalidates_nested_file_and_ancestor_collisions() {
    use std::io::Read as _;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("a/b/c/d/e");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("note.png"), b"lower").unwrap();
    let mut cache = SkinIndexCache::default();
    let mut read = |expected: &str| {
        let path = "A/B/C/D/E/NoTe.PnG";
        let ResourceOutcome::File(mut file) = cache
            .resolve_folder(Some(root.path()), None, path, path, MissingKind::NotFound)
            .unwrap()
        else {
            panic!("file expected");
        };
        let mut content = String::new();
        file.file.read_to_string(&mut content).unwrap();
        assert_eq!(content, expected);
    };
    read("lower");
    fs::write(directory.join("NOTE.PNG"), b"upper").unwrap();
    read("upper");
    let sibling = root.path().join("a/b/c/d/E");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("NOTE.PNG"), b"sibling").unwrap();
    read("sibling");
}

#[test]
fn lazer_listing_reports_hash_size_and_builtin_has_no_files() {
    let root = tempfile::tempdir().unwrap();
    let hash = "a".repeat(64);
    fs::create_dir_all(root.path().join("a/aa")).unwrap();
    fs::write(root.path().join(format!("a/aa/{hash}")), b"ini").unwrap();
    let skin = Arc::new(CurrentSkin {
        name: "skin".into(),
        builtin: None,
        base: None,
        files: [("Mania\\Note.png".into(), format!("a/aa/{hash}"))].into(),
    });
    let mut cache = SkinIndexCache::default();
    let index = cache.get(Some(root.path()), Some(&skin), Some("skin"), true, true);
    assert_eq!(index.entries[0].path, "Mania/Note.png");
    assert_eq!(index.entries[0].size, 3);
    assert_eq!(index.entries[0].hash.as_deref(), Some(hash.as_str()));
    assert_eq!(index.entries[0].modified, None);
    let builtin = Arc::new(CurrentSkin {
        name: "argon".into(),
        builtin: Some("argon".into()),
        base: Some("argon".into()),
        files: skin.files.clone(),
    });
    let index = cache.get(Some(root.path()), Some(&builtin), Some("argon"), true, true);
    assert!(index.entries.is_empty() && !index.truncated);
}

#[test]
fn folder_index_caps_entries_and_marks_truncation() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..=MAX_ENTRIES {
        fs::write(root.path().join(format!("{i:05}.png")), b"").unwrap();
    }
    let index = folder_index(Some(root.path()));
    assert_eq!(index.entries.len(), MAX_ENTRIES);
    assert!(index.truncated);
}
