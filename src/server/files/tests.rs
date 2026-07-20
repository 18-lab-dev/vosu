use super::*;

#[test]
fn decoding_is_exactly_once_and_path_safe() {
    assert_eq!(
        decode_request_path("folder/a+b%20c.osu").unwrap(),
        "folder/a+b c.osu"
    );
    assert_eq!(
        decode_request_path("%252e%252e/file").unwrap(),
        "%2e%2e/file"
    );
    assert!(decode_request_path("bad%2").is_err());
    assert!(decode_request_path("bad%GG").is_err());

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("folder")).unwrap();
    let child = safe_child(root.path(), "folder/map.osu").unwrap();
    assert_eq!(child.root, root.path());
    assert_eq!(child.relative, Path::new("folder/map.osu"));
    for hostile in ["/etc/passwd", "../secret", "folder/../../secret"] {
        assert!(matches!(
            safe_child(root.path(), hostile),
            Err(SafePathError::InvalidComponent)
        ));
    }
    for encoded in [
        "%2e%2e/secret",
        "folder/%2E%2E/%2e%2e/secret",
        "%2Fetc/passwd",
    ] {
        let decoded = decode_request_path(encoded).unwrap();
        assert!(safe_child(root.path(), &decoded).is_err(), "{encoded}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn symlinks_cannot_escape_and_open_handle_survives_path_swap() {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"outside").unwrap();
    symlink(outside.path(), root.path().join("escape")).unwrap();
    assert!(matches!(
        resolve_resource(
            Some(root.path()),
            "escape/secret",
            "/escape/secret",
            MissingKind::NotReady
        )
        .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotFound)
    ));

    fs::write(root.path().join("served.txt"), b"inside").unwrap();
    let ResourceOutcome::File(mut target) = resolve_resource(
        Some(root.path()),
        "served.txt",
        "/served.txt",
        MissingKind::NotFound,
    )
    .unwrap() else {
        panic!("expected opened file");
    };
    fs::rename(
        root.path().join("served.txt"),
        root.path().join("old-served.txt"),
    )
    .unwrap();
    symlink(
        outside.path().join("secret"),
        root.path().join("served.txt"),
    )
    .unwrap();
    target.file.seek(SeekFrom::Start(0)).unwrap();
    let mut content = String::new();
    target.file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "inside");
}

#[cfg(target_os = "linux")]
#[test]
fn top_level_counter_directory_alias_is_an_explicit_mount() {
    use std::io::Read;
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let alias_target = tempfile::tempdir().unwrap();
    let escaped = tempfile::tempdir().unwrap();
    fs::create_dir_all(alias_target.path().join("counter/assets")).unwrap();
    fs::create_dir(alias_target.path().join("unmarked")).unwrap();
    fs::write(
        alias_target.path().join("counter/index.html"),
        b"counter index",
    )
    .unwrap();
    fs::write(
        alias_target.path().join("counter/assets/app.js"),
        b"counter asset",
    )
    .unwrap();
    fs::write(alias_target.path().join("unmarked/private.txt"), b"private").unwrap();
    fs::write(escaped.path().join("secret.txt"), b"secret").unwrap();
    symlink(alias_target.path(), root.path().join("alias")).unwrap();
    symlink(escaped.path(), alias_target.path().join("counter/escape")).unwrap();

    assert!(matches!(
        resolve_encoded_resource(
            Some(root.path()),
            "alias/counter/assets/app.js",
            "/alias/counter/assets/app.js",
            MissingKind::NotFound,
        )
        .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotFound)
    ));

    let ResourceOutcome::File(mut target) = resolve_encoded_counter_resource(
        Some(root.path()),
        "alias/counter/assets/app.js",
        "/alias/counter/assets/app.js",
        MissingKind::NotFound,
    )
    .unwrap() else {
        panic!("expected alias target asset");
    };
    let mut content = String::new();
    target.file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "counter asset");

    let ResourceOutcome::File(mut target) = resolve_encoded_counter_resource(
        Some(root.path()),
        "alias/unmarked/private.txt",
        "/alias/unmarked/private.txt",
        MissingKind::NotFound,
    )
    .unwrap() else {
        panic!("expected unmarked alias target asset");
    };
    content.clear();
    target.file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "private");
    assert!(matches!(
        resolve_encoded_counter_resource(
            Some(root.path()),
            "alias/counter/escape/secret.txt",
            "/alias/counter/escape/secret.txt",
            MissingKind::NotFound,
        )
        .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotFound)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn nested_directory_alias_is_not_mounted_and_file_alias_uses_generic_resolution() {
    use std::io::Read;
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let alias_target = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("ordinary")).unwrap();
    fs::write(alias_target.path().join("index.html"), b"outside").unwrap();
    symlink(
        alias_target.path(),
        root.path().join("ordinary/nested-alias"),
    )
    .unwrap();
    assert!(matches!(
        resolve_encoded_counter_resource(
            Some(root.path()),
            "ordinary/nested-alias/index.html",
            "/ordinary/nested-alias/index.html",
            MissingKind::NotFound,
        )
        .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotFound)
    ));

    fs::write(root.path().join("real.txt"), b"inside").unwrap();
    symlink("real.txt", root.path().join("counter.css")).unwrap();
    let ResourceOutcome::File(mut target) = resolve_encoded_counter_resource(
        Some(root.path()),
        "counter.css",
        "/counter.css",
        MissingKind::NotFound,
    )
    .unwrap() else {
        panic!("expected in-root file alias");
    };
    assert_eq!(target.content_type, "text/css; charset=utf-8");
    let mut content = String::new();
    target.file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "inside");
}

#[test]
fn resource_outcomes_keep_readiness_and_current_file_distinct() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(MissingKind::NotReady.status_code(), 500);
    assert_eq!(MissingKind::NotFound.status_code(), 404);
    assert!(matches!(
        resolve_resource(None, "map.osu", "/map.osu", MissingKind::NotReady).unwrap(),
        ResourceOutcome::Missing(MissingKind::NotReady)
    ));
    assert!(matches!(
        resolve_resource(
            Some(root.path()),
            "map.osu",
            "/map.osu",
            MissingKind::NotFound
        )
        .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotFound)
    ));
    assert!(matches!(
        resolve_resource(
            Some(Path::new("/definitely/absent/vosu-root")),
            "map.osu",
            "/map.osu",
            MissingKind::NotReady,
        )
        .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotReady)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink("missing-target", root.path().join("broken")).unwrap();
        assert!(matches!(
            resolve_resource(
                Some(root.path()),
                "broken",
                "/broken",
                MissingKind::NotReady,
            )
            .unwrap(),
            ResourceOutcome::Missing(MissingKind::NotReady)
        ));
    }
    assert!(matches!(
        resolve_resource(
            Some(root.path()),
            "../secret",
            "/../secret",
            MissingKind::NotReady
        )
        .unwrap(),
        ResourceOutcome::Missing(MissingKind::NotFound)
    ));

    fs::create_dir(root.path().join("dir")).unwrap();
    assert!(matches!(
        resolve_resource(Some(root.path()), "dir", "/dir?x=1", MissingKind::NotFound).unwrap(),
        ResourceOutcome::Redirect { ref location } if location == "/dir/?x=1"
    ));
    assert!(matches!(
        resolve_resource(Some(root.path()), "dir", "/dir/", MissingKind::NotFound).unwrap(),
        ResourceOutcome::DirectoryIndex { .. }
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn mime_and_media_policy_use_requested_name_not_symlink_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("actual.mp3"), b"content").unwrap();
    symlink("actual.mp3", root.path().join("visible.txt")).unwrap();
    let ResourceOutcome::File(target) = resolve_encoded_resource(
        Some(root.path()),
        "visible%2Etxt",
        "/visible%2Etxt",
        MissingKind::NotFound,
    )
    .unwrap() else {
        panic!("expected opened file");
    };
    assert_eq!(target.content_type, "text/plain; charset=utf-8");
    assert!(!target.range_eligible);
}

#[cfg(target_os = "linux")]
#[test]
fn forced_fallback_follows_in_root_alias_and_rejects_outside_alias() {
    use std::io::Read;
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(root.path().join("inside.txt"), b"inside").unwrap();
    fs::write(outside.path().join("outside.txt"), b"outside").unwrap();
    symlink("inside.txt", root.path().join("inside-alias")).unwrap();
    symlink(
        outside.path().join("outside.txt"),
        root.path().join("outside-alias"),
    )
    .unwrap();
    let root_file = fs::File::open(root.path()).unwrap();

    let mut inside = secure_open_linux_fallback(
        &safe_child(root.path(), "inside-alias").unwrap(),
        &root_file,
    )
    .unwrap();
    let mut content = String::new();
    inside.read_to_string(&mut content).unwrap();
    assert_eq!(content, "inside");

    assert!(matches!(
        secure_open_linux_fallback(
            &safe_child(root.path(), "outside-alias").unwrap(),
            &root_file,
        ),
        Err(SafePathError::EscapesRoot)
    ));
}

#[test]
fn directory_index_escapes_text_and_encodes_hrefs() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("report & 'x'.osu"), b"").unwrap();
    fs::create_dir(root.path().join("space + ü")).unwrap();
    let opened = secure_open(&safe_child(root.path(), "").unwrap()).unwrap();
    let html = build_directory_index(&opened.file, "/Songs/").unwrap();
    assert!(html.contains("<ul id=\"overlays-list\">"));
    assert!(html.contains("<title>vosu - /Songs/</title>"));
    assert!(!html.contains("report & 'x'.osu"));
    assert!(html.contains("report &amp; &#39;x&#39;.osu"));
    assert!(html.contains("/Songs/report%20%26%20%27x%27.osu"));
    assert!(html.contains("/Songs/space%20%2B%20%C3%BC/"));
}

#[test]
fn directory_index_enumerates_open_handle_after_name_swap() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("served")).unwrap();
    fs::write(root.path().join("served/original.txt"), b"safe").unwrap();
    let opened = secure_open(&safe_child(root.path(), "served").unwrap()).unwrap();

    fs::rename(root.path().join("served"), root.path().join("held")).unwrap();
    fs::create_dir(root.path().join("served")).unwrap();
    fs::write(root.path().join("served/hostile.txt"), b"outside").unwrap();

    let html = build_directory_index(&opened.file, "/served/").unwrap();
    assert!(html.contains("original.txt"));
    assert!(!html.contains("hostile.txt"));
}

#[test]
fn byte_ranges_cover_full_open_suffix_and_rejections() {
    assert_eq!(parse_range(None, 100).unwrap(), ByteRange::Full);
    assert_eq!(
        parse_range(Some("bytes=0-9"), 100).unwrap(),
        ByteRange::Partial {
            start: 0,
            end_inclusive: 9
        }
    );
    assert_eq!(
        parse_range(Some("bytes=90-"), 100).unwrap(),
        ByteRange::Partial {
            start: 90,
            end_inclusive: 99
        }
    );
    assert_eq!(
        parse_range(Some("bytes=-10"), 100).unwrap(),
        ByteRange::Partial {
            start: 90,
            end_inclusive: 99
        }
    );
    assert_eq!(
        parse_range(Some("bytes=-200"), 100).unwrap(),
        ByteRange::Partial {
            start: 0,
            end_inclusive: 99
        }
    );

    for malformed in [
        "items=0-1",
        "bytes=",
        "bytes=a-b",
        "bytes=1-2-3",
        "bytes= 1-2",
        "Bytes=0-1",
    ] {
        assert_eq!(
            parse_range(Some(malformed), 100),
            Err(RangeError::Malformed),
            "{malformed}"
        );
    }
    assert_eq!(
        parse_range(Some("bytes=0-1,4-5"), 100),
        Err(RangeError::Multiple)
    );
    for invalid in ["bytes=-0", "bytes=100-", "bytes=20-10", "bytes=0-100"] {
        assert_eq!(
            parse_range(Some(invalid), 100),
            Err(RangeError::Unsatisfiable),
            "{invalid}"
        );
    }
    assert_eq!(
        parse_range(Some("bytes=0-0"), 0),
        Err(RangeError::Unsatisfiable)
    );
    assert_eq!(parse_range(None, 0), Ok(ByteRange::Full));
}

#[test]
fn response_plan_contains_handler_ready_headers() {
    let backing = tempfile::tempfile().unwrap();
    backing.set_len(100).unwrap();
    let target = FileTarget {
        file: backing,
        length: 100,
        content_type: content_type(Path::new("audio.mp3")),
        range_eligible: true,
    };
    let full = plan_file_response(&target, None).unwrap();
    assert_eq!(full.accept_ranges, Some("bytes"));
    let partial = plan_file_response(&target, Some("bytes=4-8")).unwrap();
    assert_eq!(partial.status, 206);
    assert_eq!(partial.offset, 4);
    assert_eq!(partial.content_length, 5);
    assert_eq!(partial.content_range.as_deref(), Some("bytes 4-8/100"));
    assert_eq!(partial.accept_ranges, Some("bytes"));
    assert_eq!(unsatisfied_content_range(100), "bytes */100");

    let non_media = FileTarget {
        file: tempfile::tempfile().unwrap(),
        length: 10,
        content_type: "text/plain; charset=utf-8",
        range_eligible: false,
    };
    assert_eq!(
        plan_file_response(&non_media, None).unwrap().accept_ranges,
        None
    );
    assert_eq!(
        plan_file_response(&non_media, Some("bytes=0-1"))
            .unwrap()
            .accept_ranges,
        Some("bytes")
    );
    for extension in ["mp3", "wav", "ogg", "gif", "webm", "mp4", "avi", "webp"] {
        assert!(media_range_eligible(Path::new(&format!(
            "file.{extension}"
        ))));
    }
    for extension in ["MP3", "oga", "png", "txt", "osu"] {
        assert!(!media_range_eligible(Path::new(&format!(
            "file.{extension}"
        ))));
    }
}

#[cfg(windows)]
#[test]
fn windows_final_path_check_rejects_junction_escape_and_prefix_collision() {
    assert!(windows_path_is_within(
        Path::new(r"C:\static"),
        Path::new(r"c:\static\counter")
    ));
    assert!(!windows_path_is_within(
        Path::new(r"C:\static"),
        Path::new(r"C:\static-escape\counter")
    ));
    // A junction/name-surrogate resolves to its target in the handle-derived
    // final path, so an outside target fails this same component comparison.
    assert!(!windows_path_is_within(
        Path::new(r"C:\static"),
        Path::new(r"D:\outside\counter")
    ));
}
