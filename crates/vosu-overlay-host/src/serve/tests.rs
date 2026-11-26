//! Overlay static serving through a lease. The test lease opens under a
//! temporary root with `std::fs` and keeps each opened handle, as core's
//! lease does; confinement itself is core's and is tested there.
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use vosu_proptest::bounded;
use vosu_surface::files::{
    DirectoryPage, DirectoryReader, FileError, FileMetadata, FileReader, OpenedFile, Operation,
    RelativeChild, RootLease, RootOperations,
};
use vosu_surface::routes::{ResponseBody, SurfaceRequest, SurfaceResponse};

use super::{OverlayRootView, OverlayStatic};
use crate::ReloadableCounterService;

pub(crate) struct TestRoot(pub PathBuf);
struct Handle(Mutex<std::fs::File>);
impl FileReader for Handle {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        let result = (|| {
            let mut file = self.0.lock().unwrap();
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| FileError::Io)?;
            let mut bytes = Vec::new();
            (&mut *file)
                .take(u64::from(len))
                .read_to_end(&mut bytes)
                .map_err(|_| FileError::Io)?;
            Ok(bytes)
        })();
        Box::pin(async move { result })
    }
}
struct NoEntries;
impl DirectoryReader for NoEntries {
    fn next_page(&mut self) -> Operation<'_, DirectoryPage> {
        Box::pin(async {
            Ok(DirectoryPage {
                entries: Vec::new(),
                truncated: false,
            })
        })
    }
}
impl RootOperations for TestRoot {
    fn open(&self, path: &RelativeChild) -> Operation<'_, OpenedFile> {
        let path = self.0.join(path.as_str());
        Box::pin(async move {
            // Mirror core's lease on every platform: a directory is
            // InvalidPath. `File::open` cannot open a directory on Windows,
            // so classify by metadata before opening.
            let metadata = std::fs::metadata(&path).map_err(|_| FileError::NotFound)?;
            if !metadata.is_file() {
                return Err(FileError::InvalidPath);
            }
            let file = std::fs::File::open(&path).map_err(|_| FileError::NotFound)?;
            Ok(OpenedFile::new(
                FileMetadata {
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                    identity: Arc::from(&b"test"[..]),
                },
                Handle(Mutex::new(file)),
            ))
        })
    }
    fn directory(&self, path: &RelativeChild) -> Operation<'_, Box<dyn DirectoryReader>> {
        let directory = self.0.join(path.as_str()).is_dir();
        Box::pin(async move {
            if directory {
                Ok(Box::new(NoEntries) as Box<dyn DirectoryReader>)
            } else {
                Err(FileError::NotFound)
            }
        })
    }
}

pub(crate) fn request(method: &str, target: &str, headers: &[(&str, &str)]) -> SurfaceRequest {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    SurfaceRequest {
        method: method.into(),
        path: path.into(),
        query: url::form_urlencoded::parse(query.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect(),
        headers: headers
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect(),
        body: Arc::from([]),
        context: Arc::new(vosu_surface::state::CapturedSelection {
            session: None,
            focus_revision: 0,
            roster: vosu_surface::state::IdentifiedRoster {
                revision: 0,
                sessions: Arc::from([]),
            },
            revision: 0,
            namespace_revisions: Arc::from([]),
            values: Arc::from([]),
        }),
        management: None,
    }
}

pub(crate) async fn body(response: SurfaceResponse) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let bytes = match response.body {
        ResponseBody::Empty => Vec::new(),
        ResponseBody::Bytes(bytes) => bytes.to_vec(),
        ResponseBody::File {
            file,
            offset,
            length,
        } => file.read(offset, length as u32).await.unwrap(),
    };
    (response.status, response.headers, bytes)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value.as_str()))
}

fn fixture() -> (
    tempfile::TempDir,
    Arc<ReloadableCounterService>,
    OverlayStatic,
) {
    let root = tempfile::tempdir().unwrap();
    let statics = root.path().join("static");
    std::fs::create_dir_all(statics.join("nested counter")).unwrap();
    let counters = Arc::new(ReloadableCounterService::production(
        statics.clone(),
        root.path().join("config/settings"),
        root.path().join("cache"),
    ));
    let serving = OverlayStatic::new(Arc::clone(&counters));
    serving.set_view(view(1, &statics));
    (root, counters, serving)
}

fn view(generation: u64, root: &Path) -> OverlayRootView {
    OverlayRootView {
        generation,
        root: RootLease::new(TestRoot(root.to_owned())),
    }
}

#[tokio::test]
async fn full_html_appends_counter_path_and_previews_mark_their_sockets() {
    let (root, _, serving) = fixture();
    let page = root.path().join("static/nested counter/index.html");
    std::fs::write(&page, "<html><head><title>x</title></head></html>").unwrap();

    let (status, headers, html) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/",
        &[]
    ))))
    .await;
    let html = String::from_utf8(html).unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        header(&headers, "content-length"),
        Some(html.len().to_string().as_str())
    );
    assert!(html.starts_with("<html><head><title>x</title></head></html>"));
    assert!(html.ends_with(r#"<script>window.COUNTER_PATH="nested counter";</script>"#));
    assert!(!html.contains("vosu-preview"), "only previews are marked");

    let (_, _, preview) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/?vosu-preview=1",
        &[]
    ))))
    .await;
    let preview = String::from_utf8(preview).unwrap();
    assert!(preview.starts_with("<html><head><script>(()=>{const W=window.WebSocket"));
    assert!(preview.ends_with(r#"<script>window.COUNTER_PATH="nested counter";</script>"#));

    // A ranged HTML read is the original bytes only, with no identity script.
    let (status, headers, part) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/index.html",
        &[("range", "bytes=0-5")]
    ))))
    .await;
    assert_eq!(status, 206);
    assert_eq!(part, b"<html>");
    assert_eq!(header(&headers, "content-range"), Some("bytes 0-5/42"));
}

#[tokio::test]
async fn pinned_profiles_qualify_counter_path_and_unknown_ones_fall_back_to_active() {
    let (root, counters, serving) = fixture();
    std::fs::write(root.path().join("static/nested counter/index.html"), "x").unwrap();
    let store = counters.settings_store();
    store.manage_profile("create", "stream", None).unwrap();
    let (_, _, pinned) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/?vosuProfile=stream",
        &[]
    ))))
    .await;
    assert!(String::from_utf8(pinned).unwrap().ends_with(
        r#"<script>window.COUNTER_PATH="/@vosu-profile/stream/nested counter";window.VOSU_PROFILE="stream";</script>"#
    ));
    let active = store.resolve_profile(None).unwrap();
    let (_, _, fallback) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/?vosuProfile=missing",
        &[]
    ))))
    .await;
    assert!(
        String::from_utf8(fallback)
            .unwrap()
            .contains(&format!(r#"window.VOSU_PROFILE="{active}""#))
    );
}

#[tokio::test]
async fn counter_path_script_escapes_html_terminators_and_line_separators() {
    let script = super::page::counter_path_script(
        "evil%3C%2Fscript%3E%26%E2%80%A8%E2%80%A9/index.html",
        None,
    )
    .unwrap();
    assert!(script.contains(r#"window.COUNTER_PATH="evil\u003C/script\u003E\u0026\u2028\u2029""#));
    assert_eq!(script.matches("</script>").count(), 1);
    // `<header>` is not mistaken for `<head>`; with neither, the marker leads.
    let marked = super::page::with_preview_socket_marker(b"<body><header>x</header>");
    assert!(marked.starts_with(b"<script>(()=>{const W=window.WebSocket"));
    assert!(marked.ends_with(b"</script><body><header>x</header>"));
}

#[tokio::test]
async fn aliases_redirect_until_a_real_folder_takes_the_name() {
    let (root, counters, serving) = fixture();
    std::fs::create_dir_all(root.path().join("static/accuracy by sxlvia")).unwrap();
    counters
        .set_name("accuracy by sxlvia", Some("Acc"), Some("acc"))
        .unwrap();
    for (target, location) in [
        ("/acc", "/accuracy%20by%20sxlvia/"),
        (
            "/ACC/assets/a.css?x=1",
            "/accuracy%20by%20sxlvia/assets/a.css?x=1",
        ),
    ] {
        let (status, headers, _) =
            body(bounded!(serving.serve(&request("GET", target, &[])))).await;
        assert_eq!(status, 302, "{target}");
        assert_eq!(header(&headers, "location"), Some(location), "{target}");
        assert_eq!(header(&headers, "cache-control"), Some("no-store"));
    }
    std::fs::create_dir_all(root.path().join("static/acc")).unwrap();
    let (status, _, _) = body(bounded!(serving.serve(&request("GET", "/acc/", &[])))).await;
    assert_ne!(status, 302);
}

#[tokio::test]
async fn malformed_and_double_encoding_never_broaden_a_match() {
    let (root, _, serving) = fixture();
    std::fs::write(root.path().join("secret.txt"), "outside").unwrap();
    std::fs::write(root.path().join("static/nested counter/a.txt"), "inside").unwrap();
    std::fs::create_dir_all(root.path().join("static/tools")).unwrap();
    std::fs::write(root.path().join("static/tools/index.html"), "reserved").unwrap();
    for target in [
        "/%2e%2e/secret.txt",
        "/%252e%252e/secret.txt",
        "/nested%2520counter/a.txt",
        "/nested%20counter/%zz",
        "/nested%20counter/a.txt%",
        "/nested%20counter%2fa.txt%00",
        "/tools/index.html",
        "/%74ools/index.html",
        "/TOOLS/",
    ] {
        let (status, headers, bytes) =
            body(bounded!(serving.serve(&request("GET", target, &[])))).await;
        assert_eq!(status, 404, "{target}");
        assert_eq!(
            header(&headers, "content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(bytes, b"Not Found", "{target}");
    }
    // One decode reaches the real name, and only it.
    let (status, _, bytes) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/a.txt",
        &[]
    ))))
    .await;
    assert_eq!((status, bytes.as_slice()), (200, &b"inside"[..]));
}

#[tokio::test]
async fn directories_redirect_to_their_slash_and_ranges_follow_core_rules() {
    let (root, _, serving) = fixture();
    std::fs::write(
        root.path().join("static/nested counter/clip.mp3"),
        b"0123456789",
    )
    .unwrap();
    let (status, headers, _) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter?l=x",
        &[]
    ))))
    .await;
    assert_eq!(
        (status, header(&headers, "location")),
        (301, Some("/nested%20counter/?l=x"))
    );

    let (status, headers, bytes) = body(bounded!(serving.serve(&request(
        "HEAD",
        "/nested%20counter/clip.mp3",
        &[]
    ))))
    .await;
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "accept-ranges"), Some("bytes"));
    assert_eq!(header(&headers, "content-length"), Some("10"));
    assert_eq!(bytes, b"0123456789");
    for (range, expected) in [
        ("bytes=2-4", &b"234"[..]),
        ("bytes=-3", b"789"),
        ("bytes=8-", b"89"),
    ] {
        let (status, _, bytes) = body(bounded!(serving.serve(&request(
            "GET",
            "/nested%20counter/clip.mp3",
            &[("range", range)]
        ))))
        .await;
        assert_eq!((status, bytes.as_slice()), (206, expected), "{range}");
    }
    for range in ["bytes=0-10", "bytes=0-1,3-4", "items=0-1", "bytes=5-2"] {
        let (status, headers, _) = body(bounded!(serving.serve(&request(
            "GET",
            "/nested%20counter/clip.mp3",
            &[("range", range)]
        ))))
        .await;
        assert_eq!(
            (status, header(&headers, "content-range")),
            (416, Some("bytes */10")),
            "{range}"
        );
    }
    let (status, _, _) = body(bounded!(serving.serve(&request(
        "POST",
        "/nested%20counter/clip.mp3",
        &[]
    ))))
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn nothing_is_served_without_a_view_and_older_generations_never_win() {
    let (root, _, serving) = fixture();
    std::fs::write(root.path().join("static/nested counter/a.txt"), "first").unwrap();
    let second = root.path().join("second");
    std::fs::create_dir_all(second.join("nested counter")).unwrap();
    std::fs::write(second.join("nested counter/a.txt"), "second").unwrap();

    serving.set_view(view(3, &second));
    serving.set_view(view(2, &root.path().join("static")));
    assert_eq!(serving.generation(), Some(3));
    let (_, _, bytes) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/a.txt",
        &[]
    ))))
    .await;
    assert_eq!(bytes, b"second");

    serving.clear_view();
    let (status, _, _) = body(bounded!(serving.serve(&request(
        "GET",
        "/nested%20counter/a.txt",
        &[]
    ))))
    .await;
    assert_eq!(status, 404);
}
