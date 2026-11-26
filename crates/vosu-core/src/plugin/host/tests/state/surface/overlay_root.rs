//! A7.1-OR counterexamples for the overlay host's read-only root lease.
use super::capture::reader;
use super::*;
use crate::plugin::host::PluginHost;
use std::path::Path;
use vosu_surface::files::{FileError, OpenedFile, RelativeChild, RootLease};

/// A live bundled `vosu.overlayhost` instance and its lease over `root`. The
/// lease holds only a weak host reference, so the caller keeps the host.
struct Minted {
    _logs: tempfile::TempDir,
    _host: Arc<PluginHost>,
    lease: RootLease,
    retirement: crate::plugin::OverlayRootRetirement,
}

fn lease(rt: &tokio::runtime::Runtime, root: &Path) -> Minted {
    let (logs, host) = test_host(rt.handle().clone());
    reader(&host, "vosu.overlayhost", 9301, &[]);
    let (lease, retirement) = host
        .overlay_root_lease(PluginInstanceId::new(9301), root)
        .unwrap();
    Minted {
        _logs: logs,
        _host: host,
        lease,
        retirement,
    }
}

fn open(
    rt: &tokio::runtime::Runtime,
    lease: &RootLease,
    path: &str,
) -> Result<OpenedFile, FileError> {
    rt.block_on(lease.open(&RelativeChild::parse(path)?))
}

fn read_all(rt: &tokio::runtime::Runtime, file: &OpenedFile) -> Result<Vec<u8>, FileError> {
    rt.block_on(async {
        let mut bytes = Vec::new();
        while (bytes.len() as u64) < file.metadata.size {
            let chunk = file.read(bytes.len() as u64, 64 * 1024).await?;
            assert!(!chunk.is_empty(), "an opened file never shrinks");
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    })
}

#[cfg(unix)]
#[test]
fn links_below_the_top_level_never_escape_the_root() {
    use std::os::unix::fs::symlink;

    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mounted = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
    std::fs::create_dir(root.path().join("plain")).unwrap();
    std::fs::write(root.path().join("plain/index.html"), b"plain").unwrap();
    std::fs::write(mounted.path().join("index.html"), b"mounted").unwrap();
    // A nested directory link, a nested file link, a top-level file link
    // and a link inside a mounted folder all point outside.
    symlink(outside.path(), root.path().join("plain/escape")).unwrap();
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("plain/secret.txt"),
    )
    .unwrap();
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("leak.txt"),
    )
    .unwrap();
    symlink(outside.path(), mounted.path().join("escape")).unwrap();
    // A top-level directory link is a separately configured mount (1.x).
    symlink(mounted.path(), root.path().join("mounted")).unwrap();
    let Minted { lease, .. } = &lease(&rt, root.path());

    assert_eq!(
        read_all(&rt, &open(&rt, lease, "plain/index.html").unwrap()).unwrap(),
        b"plain"
    );
    assert_eq!(
        read_all(&rt, &open(&rt, lease, "mounted/index.html").unwrap()).unwrap(),
        b"mounted"
    );
    for escape in [
        "plain/escape/secret.txt",
        "plain/secret.txt",
        "leak.txt",
        "mounted/escape/secret.txt",
        "plain/../leak.txt",
        "mounted/../plain/index.html",
    ] {
        assert!(
            open(&rt, lease, escape).is_err(),
            "{escape} escaped the root"
        );
    }
}

#[test]
fn a_publish_racing_a_read_never_mixes_an_open_file() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("overlay/data.bin");
    let first = vec![b'a'; 512 * 1024];
    let second = vec![b'b'; 256 * 1024];
    let publish = |bytes: &[u8]| {
        vosu_net::publish::publish_file(
            &target,
            bytes,
            ".vosu-test-",
            vosu_net::publish::Publish::Replace,
        )
        .unwrap()
    };
    publish(&first);
    let Minted {
        _logs,
        _host,
        lease,
        retirement,
    } = lease(&rt, root.path());
    let opened = open(&rt, &lease, "overlay/data.bin").unwrap();
    assert_eq!(opened.metadata.size, first.len() as u64);
    assert_eq!(read_all(&rt, &opened).unwrap(), first);

    // The overlay host's writer replaces the file while the already opened
    // object is read. That object yields its own bytes or fails closed once
    // its identity changes; it never yields the new or mixed bytes.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for round in 0..32 {
                publish(if round % 2 == 0 { &second } else { &first });
            }
        });
        for _ in 0..8 {
            match read_all(&rt, &opened) {
                Ok(bytes) => assert_eq!(bytes, first),
                Err(error) => assert!(matches!(error, FileError::TargetGone), "{error:?}"),
            }
        }
    });

    // A new open sees one complete publish.
    let fresh = open(&rt, &lease, "overlay/data.bin").unwrap();
    let latest = read_all(&rt, &fresh).unwrap();
    assert!(latest == first || latest == second);
    assert_eq!(latest.len() as u64, fresh.metadata.size);
    // Retiring the generation (a root change) ends reads of open files too.
    drop(retirement);
    assert!(matches!(
        rt.block_on(fresh.read(0, 1)),
        Err(FileError::TargetGone)
    ));
}
