use super::*;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

#[test]
fn concurrent_readers_never_observe_missing_file() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("vosu.toml");
    fs::write(&target, b"generation=0000\n").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let missing = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(Barrier::new(3));
    let readers = [false, true].map(|no_share_delete| {
        let target = target.clone();
        let stop = stop.clone();
        let missing = missing.clone();
        let reads = reads.clone();
        let start = start.clone();
        thread::spawn(move || {
            start.wait();
            while !stop.load(Ordering::Acquire) {
                let result = if no_share_delete {
                    OpenOptions::new()
                        .read(true)
                        .share_mode(FILE_SHARE_READ)
                        .open(&target)
                        .and_then(|mut file| {
                            let mut bytes = Vec::new();
                            file.read_to_end(&mut bytes)?;
                            Ok(bytes)
                        })
                } else {
                    fs::read(&target)
                };
                match result {
                    Ok(bytes) => {
                        assert_eq!(
                            bytes.len(),
                            b"generation=0000\n".len(),
                            "reader saw a partial file"
                        );
                        reads.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        missing.fetch_add(1, Ordering::Relaxed);
                    }
                    // A no-share-delete open can race the writer's delete access;
                    // this is not a missing name and the reader tries again.
                    Err(error)
                        if error
                            .raw_os_error()
                            .is_some_and(|code| code == 5 || code == 32) => {}
                    Err(error) => panic!("reader failed: {error}"),
                }
                if no_share_delete {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        })
    });
    start.wait();
    let result = (|| -> io::Result<()> {
        for generation in 0..200 {
            for _ in 0..32 {
                let temp = root.path().join("pending");
                let mut file = fs::File::create(&temp)?;
                writeln!(file, "generation={generation:04}")?;
                file.sync_all()?;
                drop(file);
                atomic_replace(&temp, &target)?;
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    for reader in readers {
        reader.join().unwrap();
    }
    let missing = missing.load(Ordering::Relaxed);
    let reads = reads.load(Ordering::Relaxed);
    eprintln!(
        "200 rounds, 6400 replacements: successful reads={reads}, missing-name observations={missing}, writer={result:?}"
    );
    assert_eq!(
        missing, 0,
        "a public file name disappeared during replacement"
    );
    result.unwrap();
    assert!(reads > 0);
}

#[test]
fn reader_without_share_delete_is_retried_until_released() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("vosu.toml");
    let temp = root.path().join("pending");
    fs::write(&target, "old").unwrap();
    fs::write(&temp, "new").unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&target)
        .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let target_copy = target.clone();
    let writer = thread::spawn(move || {
        send.send(atomic_replace(&temp, &target_copy)).unwrap();
    });
    assert!(
        receive.recv_timeout(Duration::from_millis(100)).is_err(),
        "writer must wait rather than remove an open target"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    drop(reader);
    receive
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
}

#[test]
fn replacement_preserves_target_metadata_and_cleans_staging() {
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, DACL_SECURITY_INFORMATION, GetFileSecurityW,
        GetLengthSid, GetSecurityDescriptorOwner, InitializeAcl, InitializeSecurityDescriptor,
        OWNER_SECURITY_INFORMATION, SECURITY_DESCRIPTOR, SetFileSecurityW,
        SetSecurityDescriptorDacl,
    };
    use windows_sys::Win32::Storage::FileSystem::{FILE_ALL_ACCESS, SetFileTime};
    const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

    fn security(path: &Path, information: u32) -> Vec<u32> {
        let path = wide(path);
        let mut needed = 0;
        // SAFETY: first call queries the required buffer size only.
        unsafe {
            GetFileSecurityW(
                path.as_ptr(),
                information,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        assert!(needed > 0);
        let mut descriptor = vec![0_u32; (needed as usize).div_ceil(4)];
        // SAFETY: the aligned buffer has at least `needed` writable bytes.
        assert_ne!(
            unsafe {
                GetFileSecurityW(
                    path.as_ptr(),
                    information,
                    descriptor.as_mut_ptr().cast(),
                    needed,
                    &mut needed,
                )
            },
            0
        );
        descriptor
    }
    fn dacl(path: &Path) -> Vec<u32> {
        security(path, DACL_SECURITY_INFORMATION)
    }
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("vosu.toml");
    let temp = root.path().join("pending");
    fs::write(&target, "old").unwrap();
    fs::write(&temp, "new").unwrap();
    fs::write(root.path().join("vosu.toml:retained"), "old stream").unwrap();
    fs::write(root.path().join("vosu.toml:collision"), "old collision").unwrap();
    fs::write(root.path().join("pending:collision"), "new collision").unwrap();
    let file = OpenOptions::new().write(true).open(&target).unwrap();
    let creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0x01d0_0000,
    };
    // SAFETY: the file handle is live and the FILETIME input is initialized.
    assert_ne!(
        unsafe {
            SetFileTime(
                file.as_raw_handle(),
                &creation,
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    drop(file);
    let mut original_security = security(&target, OWNER_SECURITY_INFORMATION);
    let mut owner = std::ptr::null_mut();
    let mut defaulted = 0;
    // SAFETY: Windows returned this self-relative descriptor; its owner SID
    // remains live in `original_security` until all ACL operations finish.
    assert_ne!(
        unsafe {
            GetSecurityDescriptorOwner(
                original_security.as_mut_ptr().cast(),
                &mut owner,
                &mut defaulted,
            )
        },
        0
    );
    let sid_bytes = unsafe { GetLengthSid(owner) } as usize;
    let acl_bytes = std::mem::size_of::<ACL>() + 8 + sid_bytes;
    let mut acl = vec![0_u32; acl_bytes.div_ceil(4)];
    let mut descriptor = std::mem::MaybeUninit::<SECURITY_DESCRIPTOR>::uninit();
    // An actual owner-only DACL must differ from inherited defaults. Merely
    // requesting protected inheritance through SetFileSecurityW is insufficient
    // on some Windows volumes and would not exercise metadata preservation.
    // SAFETY: buffers are aligned and sized for the initialized ACL/descriptor;
    // `owner` is the valid SID extracted above.
    unsafe {
        assert_ne!(
            InitializeAcl(acl.as_mut_ptr().cast(), acl_bytes as u32, ACL_REVISION),
            0
        );
        assert_ne!(
            AddAccessAllowedAce(
                acl.as_mut_ptr().cast(),
                ACL_REVISION,
                FILE_ALL_ACCESS,
                owner
            ),
            0
        );
        assert_ne!(
            InitializeSecurityDescriptor(
                descriptor.as_mut_ptr().cast(),
                SECURITY_DESCRIPTOR_REVISION
            ),
            0
        );
        assert_ne!(
            SetSecurityDescriptorDacl(
                descriptor.as_mut_ptr().cast(),
                1,
                acl.as_mut_ptr().cast(),
                0
            ),
            0
        );
        assert_ne!(
            SetFileSecurityW(
                wide(&target).as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast()
            ),
            0
        );
    }
    let before = dacl(&target);
    assert_ne!(before, dacl(&temp));
    // ReplaceFileW itself can normalize inheritance/protection control bits.
    // Compare the complete resulting security descriptor with that primitive,
    // rather than masking flags or requiring behavior the old path never had.
    let reference = root.path().join("reference");
    let reference_new = root.path().join("reference-new");
    fs::write(&reference, "old").unwrap();
    fs::write(&reference_new, "new").unwrap();
    // SAFETY: descriptor still contains the initialized owner-only DACL above.
    assert_ne!(
        unsafe {
            SetFileSecurityW(
                wide(&reference).as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
            )
        },
        0
    );
    assert_eq!(dacl(&reference), before);
    replace_file(&wide(&reference_new), &wide(&reference)).unwrap();
    let expected_dacl = dacl(&reference);
    fs::remove_file(&reference).unwrap();
    let before_created = fs::metadata(&target).unwrap().creation_time();
    atomic_replace(&temp, &target).unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    assert_eq!(
        fs::metadata(&target).unwrap().creation_time(),
        before_created
    );
    assert_eq!(dacl(&target), expected_dacl);
    assert_eq!(
        fs::read_to_string(root.path().join("vosu.toml:retained")).unwrap(),
        "old stream"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("vosu.toml:collision")).unwrap(),
        "new collision"
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn missing_target_unsupported_links_and_link_failure_leave_no_staging() {
    for mode in 0..3 {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("vosu.toml");
        let temp = root.path().join("pending");
        if mode != 0 {
            fs::write(&target, "old").unwrap();
        }
        fs::write(&temp, "new").unwrap();
        let result = atomic_replace_with_link(&temp, &target, |_, _| {
            if mode == 0 {
                panic!("a missing target must not need a hard link");
            }
            Err(io::Error::from_raw_os_error(
                if mode == 1 { ERROR_NOT_SUPPORTED } else { 87 } as i32,
            ))
        });
        if mode == 2 {
            assert_eq!(result.unwrap_err().raw_os_error(), Some(87));
            assert_eq!(fs::read_to_string(&target).unwrap(), "old");
            assert_eq!(fs::read_to_string(&temp).unwrap(), "new");
        } else {
            result.unwrap();
            assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        }
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STAGE_PREFIX)
        }));
    }
}

#[test]
fn sharing_timeout_preserves_target_and_leaves_no_link() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("vosu.toml");
    let temp = root.path().join("pending");
    fs::write(&target, "old").unwrap();
    fs::write(&temp, "new").unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&target)
        .unwrap();
    let started = Instant::now();
    assert!(atomic_replace(&temp, &target).is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    assert_eq!(fs::read_to_string(&temp).unwrap(), "new");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    drop(reader);
}

#[test]
fn startup_sweep_removes_only_inactive_owned_staging() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("vosu.toml");
    fs::write(&target, "original").unwrap();
    let active = Stage::new(root.path()).unwrap();
    fs::hard_link(&target, active.replacement()).unwrap();
    let stale = root.path().join(format!("{STAGE_PREFIX}stale"));
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("lease"), "").unwrap();
    fs::hard_link(&target, stale.join("replacement")).unwrap();
    let unknown = root.path().join(format!("{STAGE_PREFIX}not-ours"));
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join("user-file"), "retain").unwrap();
    cleanup_stale_replacements(root.path());
    assert!(!stale.exists());
    assert!(active.replacement().exists());
    assert_eq!(
        fs::read_to_string(unknown.join("user-file")).unwrap(),
        "retain"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    drop(active);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn private_metadata_merge_leaves_original_visible_until_publication() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("vosu.toml");
    let temp = root.path().join("pending");
    fs::write(&target, "old").unwrap();
    fs::write(&temp, "new").unwrap();
    let guard = OpenOptions::new()
        .read(true)
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&target)
        .unwrap();
    let stage = Stage::new(root.path()).unwrap();
    let candidate = stage.replacement();
    fs::hard_link(&target, &candidate).unwrap();
    replace_file(&wide(&temp), &wide(&candidate)).expect("merge metadata on private link");
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    assert_eq!(fs::read_to_string(&candidate).unwrap(), "new");
    let mut old_reader = File::open(&target).unwrap();
    publish_file(
        &candidate,
        &target,
        &wide(&target),
        Instant::now() + Duration::from_secs(2),
    )
    .expect("publish merged candidate while old handles remain open");
    let mut old_contents = String::new();
    old_reader.read_to_string(&mut old_contents).unwrap();
    assert_eq!(old_contents, "old");
    drop(guard);
    stage.finish(Ok(())).unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
}
