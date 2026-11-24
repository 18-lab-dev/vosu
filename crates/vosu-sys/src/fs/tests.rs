use super::*;
use std::{fs, io::Read, path::Path};

fn read(root: &SafeRoot, path: &str) -> String {
    let mut child = root
        .open_child(&SafeRelativePath::parse(path).unwrap())
        .unwrap();
    assert_eq!(child.metadata.len(), child.file.metadata().unwrap().len());
    let mut content = String::new();
    child.file.read_to_string(&mut content).unwrap();
    content
}

#[test]
fn decoded_paths_reject_both_platforms_prefixes_and_traversal() {
    for hostile in [
        "/etc/passwd",
        "../secret",
        "a/../secret",
        "a\0b",
        r"C:\secret",
        r"C:secret",
        r"\\server\share",
        r"\secret",
        r"a\..\secret",
        "a/C:secret",
    ] {
        assert!(
            matches!(
                SafeRelativePath::parse(hostile),
                Err(SafePathError::InvalidComponent)
            ),
            "{hostile:?}"
        );
    }
    let decoded = decode_request_path("%252e%252e/file").unwrap();
    assert_eq!(decoded, "%2e%2e/file");
    assert_eq!(
        SafeRelativePath::parse(&decoded).unwrap().as_path(),
        Path::new("%2e%2e/file")
    );
    assert!(decode_request_path("%ff").is_err());
    assert!(decode_request_path("%00").is_err());
    assert!(decode_request_path("%2").is_err());
}

#[test]
fn roots_require_host_native_absolute_directories() {
    for path in ["relative", "C:\\Windows", "C:Windows", "\\\\server\\share"] {
        #[cfg(target_os = "linux")]
        assert!(matches!(
            SafeRoot::open(Path::new(path)),
            Err(SafePathError::InvalidComponent)
        ));
        #[cfg(windows)]
        if !Path::new(path).is_absolute() {
            assert!(matches!(
                SafeRoot::open(Path::new(path)),
                Err(SafePathError::InvalidComponent)
            ));
        }
    }
    let file = tempfile::NamedTempFile::new().unwrap();
    assert!(SafeRoot::open(file.path()).is_err());
}

#[test]
fn retained_root_and_child_survive_pathname_replacement() {
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("root");
    let moved = parent.path().join("moved");
    fs::create_dir(&original).unwrap();
    fs::write(original.join("value"), b"retained-original").unwrap();
    let root = SafeRoot::open(&original).unwrap();
    assert!(
        root.open_child(&SafeRelativePath::parse(".").unwrap())
            .unwrap()
            .metadata
            .is_dir()
    );
    let mut child = root
        .open_child(&SafeRelativePath::parse("value").unwrap())
        .unwrap();
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(original.join("value"), b"replacement-root").unwrap();
    assert_eq!(read(&root, "value"), "retained-original");
    fs::rename(moved.join("value"), moved.join("old-value")).unwrap();
    fs::write(moved.join("value"), b"replacement-child").unwrap();
    let mut content = String::new();
    child.file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "retained-original");
    assert_eq!(child.metadata.len(), 17);
    assert_eq!(read(&root, "value"), "replacement-child");
}

#[cfg(target_os = "linux")]
#[test]
fn symlink_escape_and_sibling_prefix_are_rejected_in_both_openers() {
    use std::{ffi::CString, os::unix::fs::symlink};
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("root");
    let sibling = parent.path().join("root-escape");
    fs::create_dir(&path).unwrap();
    fs::create_dir(&sibling).unwrap();
    fs::write(path.join("inside.txt"), b"inside").unwrap();
    fs::write(sibling.join("outside.txt"), b"outside").unwrap();
    symlink("inside.txt", path.join("inside-alias")).unwrap();
    symlink(sibling.join("outside.txt"), path.join("outside-alias")).unwrap();
    symlink(&sibling, path.join("escape")).unwrap();
    let root = SafeRoot::open(&path).unwrap();
    assert_eq!(read(&root, "inside-alias"), "inside");
    for hostile in ["outside-alias", "escape/outside.txt"] {
        assert!(matches!(
            root.open_child(&SafeRelativePath::parse(hostile).unwrap()),
            Err(SafePathError::EscapesRoot)
        ));
    }
    let root_file = fs::File::open(&path).unwrap();
    let mut inside =
        super::unix::open_fallback(&root_file, &CString::new("inside-alias").unwrap()).unwrap();
    let mut content = String::new();
    inside.read_to_string(&mut content).unwrap();
    assert_eq!(content, "inside");
    assert!(matches!(
        super::unix::open_fallback(&root_file, &CString::new("outside-alias").unwrap()),
        Err(SafePathError::EscapesRoot)
    ));
}

#[cfg(windows)]
#[test]
fn junction_escape_is_checked_after_native_relative_open() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("root");
    let sibling = parent.path().join("root-escape");
    fs::create_dir(&path).unwrap();
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("secret"), b"outside-junction").unwrap();
    fs::create_dir(path.join("inside")).unwrap();
    fs::write(path.join("inside/value"), b"inside-junction").unwrap();
    for (name, destination) in [("escape", sibling), ("alias", path.join("inside"))] {
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/c", "mklink", "/J"])
            .arg(path.join(name))
            .arg(destination)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "creating junction failed");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                panic!("junction creation exceeded deadline");
            }
            std::thread::yield_now();
        }
    }
    let root = SafeRoot::open(&path).unwrap();
    assert_eq!(read(&root, "alias/value"), "inside-junction");
    assert!(matches!(
        root.open_child(&SafeRelativePath::parse("escape/secret").unwrap()),
        Err(SafePathError::EscapesRoot)
    ));
}

#[cfg(windows)]
#[test]
fn ads_are_rejected_and_native_errors_remain_io_errors() {
    let path = tempfile::tempdir().unwrap();
    fs::write(path.path().join("value"), b"ordinary-file").unwrap();
    let root = SafeRoot::open(path.path()).unwrap();
    for ads in [
        "value:stream",
        "value::$DATA",
        "nested/value:stream",
        "name:with-colon",
    ] {
        assert!(matches!(
            SafeRelativePath::parse(ads),
            Err(SafePathError::InvalidComponent)
        ));
    }
    let Err(SafePathError::Io(error)) =
        root.open_child(&SafeRelativePath::parse("missing-value").unwrap())
    else {
        panic!("missing native file must preserve its I/O error");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.raw_os_error().is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn unix_colon_names_remain_regular_filenames() {
    let path = tempfile::tempdir().unwrap();
    fs::write(path.path().join("name:with-colon"), b"unix-colon-file").unwrap();
    let root = SafeRoot::open(path.path()).unwrap();
    assert_eq!(read(&root, "name:with-colon"), "unix-colon-file");
}

#[cfg(windows)]
#[test]
fn in_root_file_symlink_follows_its_absolute_target() {
    use std::os::windows::fs::symlink_file;
    let path = tempfile::tempdir().unwrap();
    fs::write(path.path().join("value"), b"absolute-link-target").unwrap();
    symlink_file(path.path().join("value"), path.path().join("alias")).unwrap();
    let root = SafeRoot::open(path.path()).unwrap();
    assert_eq!(read(&root, "alias"), "absolute-link-target");
}

#[cfg(windows)]
#[test]
fn relative_file_symlink_resolves_through_retained_root_directory() {
    use std::os::windows::fs::symlink_file;
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("original");
    let moved = parent.path().join("moved");
    fs::create_dir(&original).unwrap();
    fs::write(original.join("value"), b"retained-relative-target").unwrap();
    symlink_file("value", original.join("alias")).unwrap();
    let root = SafeRoot::open(&original).unwrap();
    assert_eq!(read(&root, "alias"), "retained-relative-target");
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(original.join("value"), b"replacement-target").unwrap();
    assert_eq!(read(&root, "alias"), "retained-relative-target");
}

#[cfg(target_os = "linux")]
#[test]
fn stripping_first_component_does_not_reparse_unix_suffix() {
    let path = SafeRelativePath::parse(r"alias/\x").unwrap();
    assert_eq!(path.strip_first().as_path(), Path::new(r"\x"));
    assert!(SafeRelativePath::parse(r"\x").is_err());
    assert_eq!(
        SafeRelativePath::parse("alias")
            .unwrap()
            .strip_first()
            .as_path(),
        Path::new("")
    );
}

#[test]
fn directory_cursor_and_absolute_selection_retain_the_opened_root() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original");
    let moved = dir.path().join("moved");
    fs::create_dir(&original).unwrap();
    fs::write(original.join("original.osu"), b"original-content").unwrap();
    let root = SafeRoot::open(&original).unwrap();
    let mut cursor = root
        .directory(&SafeRelativePath::parse("").unwrap())
        .unwrap();
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(original.join("foreign.osu"), b"foreign-content").unwrap();
    let mut names = Vec::new();
    for _ in 0..8 {
        let Some(name) = cursor.next_name().unwrap() else {
            break;
        };
        names.push(name);
    }
    assert_eq!(names, vec![std::ffi::OsString::from("original.osu")]);
    assert_eq!(read(&root, "original.osu"), "original-content");
    assert!(
        root.relative_absolute(&original.join("foreign.osu"))
            .is_err()
    );
    let (child, _) = root.relative_absolute(&moved.join("original.osu")).unwrap();
    assert_eq!(child.as_path(), Path::new("original.osu"));
}

#[cfg(target_os = "linux")]
#[test]
fn retained_directory_metadata_refuses_symlink_escape() {
    let inside = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.osu"), b"secret").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.osu"),
        inside.path().join("escape.osu"),
    )
    .unwrap();
    let root = SafeRoot::open(inside.path()).unwrap();
    let mut cursor = root
        .directory(&SafeRelativePath::parse("").unwrap())
        .unwrap();
    assert_eq!(cursor.next_name().unwrap(), Some("escape.osu".into()));
    assert!(
        cursor
            .open_child(&SafeRelativePath::parse("escape.osu").unwrap())
            .is_err()
    );
    assert!(
        root.relative_absolute(&inside.path().join("escape.osu"))
            .is_err()
    );
}

#[cfg(windows)]
#[test]
fn calculation_absolute_spelling_cannot_normalize_ads_or_trailing_aliases() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("chart.osu"), b"inside").unwrap();
    let root = SafeRoot::open(dir.path()).unwrap();
    for name in ["chart.osu.", "chart.osu ", "chart.osu:secret"] {
        assert!(
            root.relative_absolute(&dir.path().join(name)).is_err(),
            "{name}"
        );
    }
    let (relative, _) = root
        .relative_absolute(&dir.path().join("chart.osu"))
        .unwrap();
    assert_eq!(relative.as_path(), Path::new("chart.osu"));
}
