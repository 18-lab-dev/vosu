//! `OverlayPresenter::search` over a real overlays folder, moved with the
//! installed list from the root runtime's page provider.

use std::{path::Path, sync::Arc};

use tokio_util::sync::CancellationToken;
use vosu_proptest::bounded;

use crate::{ReloadableCounterService, catalog::CatalogService, presentation::OverlayPresenter};

/// A presenter whose catalog fetch is already cancelled, so the community
/// catalog is unreachable without touching the network.
fn offline_presenter(root: &Path) -> (Arc<ReloadableCounterService>, OverlayPresenter) {
    let counters = Arc::new(ReloadableCounterService::production(
        root.join("static"),
        root.join("settings"),
        root.join("cache"),
    ));
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    let presenter = OverlayPresenter::new(
        Arc::clone(&counters),
        CatalogService::production("test"),
        shutdown,
    );
    (counters, presenter)
}

#[tokio::test]
async fn installed_search_ignores_an_unreachable_catalog() {
    let root = tempfile::tempdir().unwrap();
    for name in ["first", "second"] {
        let counter = root.path().join("static").join(name);
        std::fs::create_dir_all(&counter).unwrap();
        std::fs::write(counter.join("index.html"), "").unwrap();
    }
    let (counters, presenter) = offline_presenter(root.path());

    let installed = bounded!(presenter.search("", false)).unwrap();
    let installed = String::from_utf8(installed.to_vec()).unwrap();
    assert_eq!(
        installed
            .matches(r#"<article class="counter-card""#)
            .count(),
        2
    );
    assert!(bounded!(presenter.search("", true)).is_err());

    bounded!(counters.shutdown());
}

#[tokio::test]
async fn one_unreadable_overlay_is_an_error_card_not_an_empty_list() {
    let root = tempfile::tempdir().unwrap();
    let good = root.path().join("static/good");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(good.join("index.html"), "").unwrap();
    let bad = root.path().join("static/bad &one");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("metadata.txt"), vec![b'x'; 64 * 1024 + 1]).unwrap();
    let (counters, presenter) = offline_presenter(root.path());

    let installed = bounded!(presenter.search("", false)).unwrap();
    let installed = String::from_utf8(installed.to_vec()).unwrap();
    // The readable overlay is listed; the broken folder is an error card
    // naming the file, with the actions that fix it. The page counts cards.
    assert!(installed.contains(r#"data-overlay-id="good""#));
    assert!(
        installed
            .contains(r#"<article class="counter-card is-broken" data-overlay-id="bad &amp;one""#)
    );
    assert!(installed.contains("<code>metadata.txt: "));
    assert!(installed.contains(r#"data-action="delete-counter" data-folder="bad &amp;one""#));
    assert!(!installed.contains("No overlays yet"));

    bounded!(counters.shutdown());
}
