//! Card HTML contracts, moved with the cards from the root renderer. Card
//! URLs are origin-relative (A7.1): the dashboard prefixes `overlayOrigin`.

use crate::cards::{
    CounterCard, local_overlay, matches_catalog_query, normalize_catalog_query,
    render_counter_card, render_local_counters,
};
use crate::presentation::annotate_installed_updates;
use crate::preview_scheme;

#[test]
fn counter_cards_preview_at_native_resolution_from_encoded_relative_paths() {
    let fixed = render_counter_card(&CounterCard {
        name: "Counter",
        folder: "accuracy by sxlvia",
        resolution: &[serde_json::json!(640), serde_json::json!(480)],
        ..CounterCard::default()
    });
    // Preview frames carry the marker that keeps them out of the connected count.
    assert!(fixed.contains(r#"src="/accuracy%20by%20sxlvia/?vosu-preview=1""#));
    assert!(fixed.contains(r#"data-preview-width="640" data-preview-height="480""#));
    assert!(fixed.contains(r#"width="640" height="480" scrolling="no""#));
    assert!(fixed.contains(r#"data-copy-value="/accuracy%20by%20sxlvia/""#));
    assert!(fixed.contains(r#"<code>/accuracy by sxlvia/</code>"#));
    assert!(fixed.contains(r#"data-copy-value="640 × 480""#));
    // No listener origin is ever embedded in a card.
    assert!(!fixed.contains("http://"));

    let nested = render_counter_card(&CounterCard {
        folder: "parent/50% #1?",
        ..CounterCard::default()
    });
    assert!(nested.contains(r#"src="/parent/50%25%20%231%3F/?vosu-preview=1""#));

    let any = render_counter_card(&CounterCard {
        folder: "folder",
        resolution: &[serde_json::json!(-1), serde_json::json!(-2)],
        ..CounterCard::default()
    });
    assert!(any.contains(r#"width="500" height="300""#));
    assert!(any.contains(r#"data-copy-value="ANY × ANY""#));

    let flexible = render_counter_card(&CounterCard {
        folder: "folder",
        resolution: &[serde_json::json!(-2), serde_json::json!(-1)],
        ..CounterCard::default()
    });
    assert!(flexible.contains(r#"class="preview-stage is-fluid""#));
    assert!(flexible.contains(r#"width="100%" height="500""#));
}

#[test]
fn installed_updates_use_catalog_metadata_and_actual_folder_target() {
    let mut local: Vec<crate::Counter> = serde_json::from_value(serde_json::json!([{
        "folderName": "custom-folder", "name": "OffsetSuggester", "author": "Pinossaur",
        "version": "0.1", "resolution": [350, 230], "authorlinks": [], "settings": []
    }]))
    .unwrap();
    let remote = crate::catalog::decode_response(
        200,
        br#"[{"name":"OffsetSuggester","author":"Pinossaur","version":"1.0","downloadLink":"https://example.test/counter.zip"}]"#,
    )
    .unwrap();
    let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
    let remote = crate::catalog::merge_local(remote, &installed);
    annotate_installed_updates(&mut local, &remote);
    let html = render_local_counters(&local, &[], &Default::default(), None);
    // A real Update button, not a badge-styled one, and no badge repeating it.
    assert!(!html.contains("badge-warning"));
    assert!(
        html.contains(r#"class="button button-update" type="button" data-action="update-counter""#)
    );
    assert!(html.contains(r#"aria-label="Update OffsetSuggester""#));
    assert!(html.contains(r#"data-install-name="custom-folder""#));
    assert!(html.contains(r#"data-download-url="https://example.test/counter.zip""#));
}

#[test]
fn card_meta_omits_empty_or_unknown_author_segments() {
    for external in [false, true] {
        for author in ["", "  ", "unknown", "Unknown"] {
            let html = render_counter_card(&CounterCard {
                name: "Native small",
                version: "1.0.0",
                author,
                external,
                resolution: &[serde_json::json!(180), serde_json::json!(80)],
                ..CounterCard::default()
            });
            let meta = html
                .split(r#"<p class="counter-card__meta">"#)
                .nth(1)
                .unwrap()
                .split("</p>")
                .next()
                .unwrap();
            assert!(!meta.contains("<span></span>"), "{meta}");
            assert!(!meta.contains("unknown"), "{meta}");
            assert!(!meta.contains("by "), "{meta}");
            if !external {
                assert!(
                    meta.starts_with(
                        r#"<span>v1.0</span><span class="counter-card__size">180 × 80</span>"#
                    ),
                    "{meta}"
                );
            }
        }
    }
}

#[test]
fn installed_counter_cards_emit_escaped_identity_pin_and_management_menu() {
    let card = render_counter_card(&CounterCard {
        name: "Counter",
        author: "Author",
        version: "1.0",
        folder: r#"folder<&""#,
        settings: true,
        ..CounterCard::default()
    });

    assert!(
        card.starts_with(
            r#"<article class="counter-card" data-overlay-id="folder&lt;&amp;&quot;""#
        )
    );
    assert!(card.contains(r#"title="Counter">Counter</h2>"#));
    assert!(card.contains(r#"aria-labelledby="card-title-"#));
    assert!(card.contains(r#"data-action="toggle-pin" aria-pressed="false""#));
    let header_actions = &card[..card.find(r#"<div class="card-menu""#).unwrap()];
    assert!(header_actions.contains(r#"data-action="counter-settings""#));
    assert!(!header_actions.contains(r#"data-action="delete-counter""#));
    let menu = &card[card.find(r#"<div class="card-menu""#).unwrap()..];
    assert!(menu.starts_with(
        r#"<div class="card-menu" id="card-menu-folder%3C&amp;%22" popover role="menu""#
    ));
    // Enlarge and Copy resolution are menu items, not extra Tab stops.
    assert!(menu.contains(r#"data-action="expand-preview">Enlarge preview</button>"#));
    assert!(menu.contains(r#"data-action="copy-metadata" data-copy-label="Resolution""#));
    assert!(card.contains(
        r#"class="preview-expand" type="button" data-action="expand-preview" tabindex="-1""#
    ));
    let delete = menu.find(r#"data-action="delete-counter""#).unwrap();
    for earlier in ["open-counter", "pin-earlier", "pin-later"] {
        assert!(menu.find(&format!(r#"data-action="{earlier}""#)).unwrap() < delete);
    }
    assert!(card.contains(r#"popovertarget="card-menu-folder%3C&amp;%22""#));
}

#[test]
fn duplicate_installed_titles_show_their_folder() {
    let counter = |folder: &str, name: &str| -> crate::Counter {
        serde_json::from_value(serde_json::json!({
            "folderName": folder,
            "name": name,
            "author": "sxlvia",
            "version": "1.0.0",
            "resolution": [],
            "authorlinks": [],
            "settings": []
        }))
        .unwrap()
    };
    let html = render_local_counters(
        &[
            counter("accuracy", "accuracy"),
            counter("accuracy by sxlvia", "Accuracy"),
            counter("other", "other"),
        ],
        &[preview_scheme::PreviewScheme::Dark],
        &Default::default(),
        None,
    );
    assert_eq!(html.matches(r#"class="counter-card__folder""#).count(), 2);
    assert!(
        html.contains(r#"<span class="visually-hidden">Folder: </span>accuracy by sxlvia</p>"#)
    );
    assert!(!html.contains(r#"</span>other</p>"#));
    // Only the overlay that declares a dark scheme gets a dark preview frame;
    // the others keep the light default.
    assert_eq!(html.matches(r#"data-color-scheme="dark""#).count(), 1);
    assert!(html.contains(r#"<iframe src="/accuracy/?vosu-preview=1" data-color-scheme="dark""#));
}

#[test]
fn nicknames_and_aliases_rename_cards_but_keep_the_folder_frame() {
    let counter: crate::Counter = serde_json::from_value(serde_json::json!({
        "folderName": "accuracy by sxlvia", "name": "accuracy", "author": "sxlvia", "version": "1.0",
        "resolution": [], "authorlinks": [], "settings": []
    }))
    .unwrap();
    let names = std::collections::BTreeMap::from([(
        "accuracy by sxlvia".to_owned(),
        crate::OverlayName {
            nickname: Some("Acc <main>".into()),
            alias: Some("acc".into()),
        },
    )]);
    let html = render_local_counters(std::slice::from_ref(&counter), &[], &names, None);
    assert!(html.contains(r#"title="Acc &lt;main&gt;">Acc &lt;main&gt;</h2>"#));
    assert!(html.contains(r#"data-counter-name="accuracy" data-counter-title="Acc &lt;main&gt;""#));
    assert!(html.contains(r#"data-counter-alias="acc""#));
    // The frame loads the real folder; the copied browser-source URL is the alias.
    assert!(html.contains(r#"<iframe src="/accuracy%20by%20sxlvia/?vosu-preview=1""#));
    assert!(html.contains(r#"data-copy-value="/acc/""#));
    assert!(html.contains("<code>/acc/</code>"));
    assert!(html.contains(r#"data-action="rename-counter""#));
    // The nickname is searchable alongside the original name.
    for query in ["acc main", "accuracy"] {
        assert!(
            !render_local_counters(std::slice::from_ref(&counter), &[], &names, Some(query))
                .is_empty(),
            "{query}"
        );
    }
}

#[test]
fn catalog_counter_cards_do_not_emit_identity_or_pin_control() {
    let card = render_counter_card(&CounterCard {
        name: "Counter",
        author: "Author",
        folder: "catalog-folder",
        external: true,
        download_link: Some("https://example.com/counter.zip"),
        author_links: &["https://github.com/Author".to_owned()],
        ..CounterCard::default()
    });

    assert!(!card.contains("data-overlay-id"));
    assert!(!card.contains(r#"data-action="toggle-pin""#));
    assert!(!card.contains("card-menu"));
    assert!(card.contains(r#">GitHub</a>"#));
    assert!(!card.contains("<img"));
}

#[test]
fn counter_card_links_require_absolute_http_or_https_urls() {
    let author_links = [
        "javascript:alert(1)".to_owned(),
        "https://authors.example/profile".to_owned(),
        "data:text/plain,unsafe".to_owned(),
        "/same-origin/profile".to_owned(),
        "http://authors.example/profile".to_owned(),
    ];
    let assets = [
        "/same-origin/preview.png",
        "https://cdn.example/preview.png",
        "javascript:alert(1)",
        "data:text/plain,unsafe",
        "http://cdn.example/preview.png",
    ];
    let card = render_counter_card(&CounterCard {
        name: "Counter",
        author: "Author",
        folder: "catalog-folder",
        external: true,
        author_links: &author_links,
        assets: &assets,
        ..CounterCard::default()
    });

    assert!(card.contains(r#"href="https://authors.example/profile""#));
    assert!(card.contains(r#"href="http://authors.example/profile""#));
    assert!(card.contains(r#"src="https://cdn.example/preview.png""#));
    assert!(card.contains(r#"src="http://cdn.example/preview.png""#));
    assert_eq!(card.matches(r#"class="author-link""#).count(), 2);
    assert_eq!(card.matches(r#"class="gallery__slide""#).count(), 2);
    // Only the first preview is shown; the gallery browses the rest.
    assert_eq!(card.matches(r#" hidden>"#).count(), 1);
    assert!(card.contains(r#"<span class="gallery__count" aria-live="polite">1 / 2</span>"#));
    for rejected in [
        "javascript:alert(1)",
        "data:text/plain,unsafe",
        "/same-origin/profile",
        "/same-origin/preview.png",
    ] {
        assert!(!card.contains(rejected), "{rejected} must be omitted");
    }
}

#[test]
fn production_catalog_query_matches_source_token_rules() {
    assert_eq!(normalize_catalog_query("!HD SC"), "hd sc");
    assert_eq!(normalize_catalog_query("HD SC"), "hd sc");
    assert_eq!(normalize_catalog_query("  my  overlay! "), "my overlay");
    // Every word must match somewhere, as on the Tools and Settings pages.
    assert!(matches_catalog_query(
        "HD Counter",
        "Author",
        &["streamcompanion".into()],
        &normalize_catalog_query("HD SC")
    ));
    assert!(!matches_catalog_query(
        "Counter",
        "Author",
        &["streamcompanion".into()],
        &normalize_catalog_query("HD SC")
    ));
    assert!(matches_catalog_query(
        "My-Overlay",
        "Author",
        &[],
        &normalize_catalog_query("my overlay")
    ));
    assert!(!matches_catalog_query(
        "Counter",
        "Author",
        &[],
        &normalize_catalog_query("my overlay")
    ));
    assert!(matches_catalog_query(
        "Counter",
        "Author",
        &["streamcompanion".into()],
        "sc"
    ));
    assert!(!matches_catalog_query(
        "Counter",
        "Author",
        &["vosu".into()],
        "vosu"
    ));
    assert!(matches_catalog_query("vosu counter", "Author", &[], "vosu"));
    assert!(!matches_catalog_query(
        "Counter",
        "Author",
        &["vosu".into()],
        "missing vosu"
    ));
}
