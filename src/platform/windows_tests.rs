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
                write!(file, "generation={generation:04}\n")?;
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
