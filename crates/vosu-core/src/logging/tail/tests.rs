use super::*;
use std::fs::OpenOptions;
use tempfile::tempdir;
use tokio::sync::mpsc;

#[test]
fn snapshot_spans_archives_and_orders_collision_suffixes_numerically() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("2026-11-24 10-00-00.log"), b"old\n").unwrap();
    fs::write(directory.path().join("2026-11-24 10-00-00-2.log"), b"two\n").unwrap();
    fs::write(
        directory.path().join("2026-11-24 10-00-00-10.log"),
        b"ten\n",
    )
    .unwrap();
    fs::write(directory.path().join("latest.log"), b"current\npartial").unwrap();
    fs::write(directory.path().join("unrelated.log"), b"ignored\n").unwrap();
    let mut state = Tail::new(directory.path().to_owned()).unwrap();
    let mut output = Vec::new();
    state.snapshot(4, &mut output).unwrap();
    assert_eq!(output, b"two\nten\ncurrent\npartial");
    output.clear();
    state.snapshot(0, &mut output).unwrap();
    assert!(output.is_empty());
}

#[test]
fn snapshot_handles_empty_files_crlf_and_chunk_boundaries() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("latest.log");
    fs::write(&path, []).unwrap();
    let mut state = Tail::new(directory.path().to_owned()).unwrap();
    let mut output = Vec::new();
    state.snapshot(10, &mut output).unwrap();
    assert!(output.is_empty());
    let mut contents = vec![b'x'; CHUNK * 2];
    contents.extend_from_slice(b"\r\nlast\r\n");
    fs::write(&path, contents).unwrap();
    state.snapshot(1, &mut output).unwrap();
    assert_eq!(output, b"last\r\n");
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    updates: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(updates) = &self.updates {
            let _ = updates.send(self.bytes.clone());
        }
        Ok(())
    }
}

async fn expect_output(updates: &mut mpsc::UnboundedReceiver<Vec<u8>>, expected: &[u8]) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let actual = updates.recv().await.expect("follower closed unexpectedly");
            if actual == expected {
                return;
            }
            assert!(
                expected.starts_with(&actual),
                "unexpected output: {actual:?}"
            );
        }
    })
    .await
    .expect("follower did not produce expected output");
}

fn follow(
    directory: &Path,
    lines: usize,
) -> (
    CancellationToken,
    mpsc::UnboundedReceiver<Vec<u8>>,
    tokio::task::JoinHandle<io::Result<()>>,
) {
    let cancellation = CancellationToken::new();
    let (updates, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(
        directory.to_owned(),
        lines,
        true,
        cancellation.clone(),
        Capture {
            bytes: Vec::new(),
            updates: Some(updates),
        },
    ));
    (cancellation, receiver, task)
}

async fn stop(cancellation: CancellationToken, task: tokio::task::JoinHandle<io::Result<()>>) {
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("follower did not stop")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn follow_appends_and_cancels_without_replaying_snapshot() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("latest.log");
    fs::write(&path, b"skip\nkeep\n").unwrap();
    let (cancellation, mut updates, task) = follow(directory.path(), 1);
    expect_output(&mut updates, b"keep\n").await;
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"next\n")
        .unwrap();
    expect_output(&mut updates, b"keep\nnext\n").await;
    stop(cancellation, task).await;
}

#[tokio::test]
async fn follow_restarts_on_truncation_and_same_length_rewrite() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("latest.log");
    fs::write(&path, b"initial contents\n").unwrap();
    let (cancellation, mut updates, task) = follow(directory.path(), 0);
    expect_output(&mut updates, b"").await;
    fs::write(&path, b"short\n").unwrap();
    expect_output(&mut updates, b"short\n").await;
    fs::write(&path, b"other\n").unwrap();
    expect_output(&mut updates, b"short\nother\n").await;
    stop(cancellation, task).await;
}

#[tokio::test]
async fn follow_drains_rotated_handle_and_multiple_new_daemon_files() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("latest.log");
    fs::write(&path, b"before\n").unwrap();
    let (cancellation, mut updates, task) = follow(directory.path(), 1);
    expect_output(&mut updates, b"before\n").await;
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"late\n")
        .unwrap();
    fs::rename(&path, directory.path().join("2026-11-24 10-00-00.log")).unwrap();
    fs::write(
        directory.path().join("2026-11-24 10-00-01.log"),
        b"middle\n",
    )
    .unwrap();
    fs::write(&path, b"after\n").unwrap();
    expect_output(&mut updates, b"before\nlate\nmiddle\nafter\n").await;
    stop(cancellation, task).await;
}

#[tokio::test]
async fn follow_waits_for_directory_creation_and_snapshot_does_not_create_it() {
    let directory = tempdir().unwrap();
    let missing = directory.path().join("missing");
    run(
        missing.clone(),
        10,
        false,
        CancellationToken::new(),
        Capture::default(),
    )
    .await
    .unwrap();
    assert!(!missing.exists());
    let (cancellation, mut updates, task) = follow(&missing, 10);
    expect_output(&mut updates, b"").await;
    fs::create_dir(&missing).unwrap();
    fs::write(missing.join("latest.log"), b"started\n").unwrap();
    expect_output(&mut updates, b"started\n").await;
    stop(cancellation, task).await;
}
