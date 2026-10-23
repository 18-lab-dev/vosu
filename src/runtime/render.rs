use super::preview_scheme::PreviewScheme;
use super::*;

pub(super) fn local_overlay(counter: &crate::counters::Counter) -> LocalOverlay {
    LocalOverlay {
        name: counter.name.clone(),
        author: counter.author.clone(),
        version: counter.version.clone(),
    }
}

/// Per-page chrome rendered on the server, so the first paint already shows
/// the right heading, current tab and search hint.
struct PageChrome {
    /// `data-page` of the current navigation tab.
    key: &'static str,
    title: &'static str,
    /// Search placeholder and label; `None` renders no search field.
    search: Option<&'static str>,
}

fn page_chrome(page: DynamicPage) -> PageChrome {
    let (key, title, search) = match page {
        DynamicPage::Dashboard => ("installed", "Overlays", Some("Search overlays")),
        DynamicPage::Available => (
            "available",
            "Available overlays",
            Some("Search available overlays"),
        ),
        DynamicPage::Tools => ("tools", "Tools", Some("Search tools")),
        DynamicPage::Settings => ("settings", "Settings", Some("Search settings")),
        DynamicPage::OverlaySettings => ("installed", "Overlay settings", None),
        DynamicPage::ToolSettings => ("tools", "Tool settings", None),
    };
    PageChrome { key, title, search }
}

fn search_form(placeholder: &str) -> String {
    format!(
        r#"<form class="search-form" role="search"><div class="search-control"><input id="counter-search" class="search-bar" type="search" autocomplete="off" placeholder="{placeholder}" aria-label="{placeholder}" aria-controls="counter-results"><kbd aria-hidden="true">/</kbd></div></form>"#
    )
}

/// Shown when the overlays folder holds no overlays at all; the client
/// renders the same markup after deleting the last card.
pub(super) const INSTALLED_EMPTY_STATE: &str = r#"<div class="empty-state"><h2>No overlays yet</h2><p>Install one, or copy an overlay folder into the overlays folder.</p><div class="counter-actions"><a class="button button-primary" href="/available">Browse available overlays</a><button class="button" type="button" data-action="open-overlays-root">Open overlays folder</button></div></div>"#;

/// The overlays folder itself could not be listed. Distinct from the empty
/// state so a read failure never looks like the overlays are gone.
pub(super) fn render_overlays_unreadable(root: &std::path::Path, error: &str) -> String {
    format!(
        r#"<div class="empty-state" role="alert"><h2>Couldn't read the overlays folder</h2><p><code>{}</code></p><p>{}</p><div class="counter-actions"><a class="button button-primary" href="/">Retry</a><button class="button" type="button" data-action="open-overlays-root">Open overlays folder</button></div></div>"#,
        escape_html(&root.display().to_string()),
        escape_html(error),
    )
}

/// Overlay folders whose files failed to parse, as error cards beside the
/// readable overlays: the folder, the parse error, Open folder and Delete.
pub(super) fn render_broken_overlays(
    broken: &[crate::counters::BrokenOverlay],
    names: &std::collections::BTreeMap<String, crate::counters::OverlayName>,
    query: Option<&str>,
) -> String {
    broken
        .iter()
        .filter(|overlay| {
            query.is_none_or(|query| matches_catalog_query(&overlay.name, "", &[], query))
        })
        .map(|overlay| {
            let title = names
                .get(&overlay.folder_name)
                .and_then(|name| name.nickname.as_deref())
                .unwrap_or(&overlay.name);
            let path = overlay_path(&overlay.folder_name);
            let id = escape_html(&format!("broken-{}", path.trim_matches('/')));
            // The folder is named once: as the title, or under a different
            // title (nickname or nested folder).
            let folder_line = if title == overlay.folder_name {
                String::new()
            } else {
                format!(
                    r#"<p class="counter-card__meta"><span>{}</span></p>"#,
                    escape_html(&overlay.folder_name)
                )
            };
            format!(
                concat!(
                    r#"<article class="counter-card is-broken" data-overlay-id="{folder}" data-counter-name="{name}" data-counter-title="{title}" data-counter-nickname="" data-counter-alias="" data-counter-author="" data-counter-version="" data-counter-compatible="" aria-labelledby="{id}-title">"#,
                    r#"<div class="counter-card__broken" role="alert" id="{id}-error"><strong>Couldn't read this overlay</strong><code>{error}</code></div>"#,
                    r#"<div class="counter-card__body"><div class="counter-card__identity"><h2 id="{id}-title" title="{title}">{title}</h2>{folder_line}</div>"#,
                    r#"<div class="counter-actions"><button class="button" type="button" data-action="open-counter" data-folder="{folder}">Open folder</button>"#,
                    r#"<button class="button button-danger" type="button" data-action="delete-counter" data-folder="{folder}">Delete</button></div></div></article>"#,
                ),
                folder = escape_html(&overlay.folder_name),
                name = escape_html(&overlay.name),
                title = escape_html(title),
                id = id,
                error = escape_html(&overlay.error),
                folder_line = folder_line,
            )
        })
        .collect()
}

/// Third-party disclosure for the Available page. It sits outside the results
/// section so catalog searches, which replace only the results, keep it.
pub(super) const CATALOG_NOTICE: &str = concat!(
    r#"<details class="catalog-notice" open><summary>Community overlays · third-party code</summary>"#,
    r#"<p>Third-party JavaScript from <a href="https://tosu.app" target="_blank" rel="noopener noreferrer">tosu.app</a> (downloads via osuck.link), not reviewed by vosu. Install only overlays you trust.</p>"#,
    "</details>",
);

pub(super) fn render_homepage(
    template: EmbeddedResponse,
    page: DynamicPage,
    local_count: Option<usize>,
    available_count: Option<usize>,
    notice: &str,
    list: &str,
    update_version: Option<&str>,
) -> EmbeddedResponse {
    let chrome = page_chrome(page);
    let local_count = local_count
        .map(|count| count.to_string())
        .unwrap_or_default();
    let available_count = available_count
        .map(|count| count.to_string())
        .unwrap_or_default();
    let current_version = current_version().to_string();
    let search = chrome.search.map(search_form).unwrap_or_default();
    // Settings saves as you go: its save state shares the title row and is
    // empty at rest (homepage.js fills it after a save or while edits wait).
    let status = if page == DynamicPage::Settings {
        r#"<p class="settings-status" hidden></p>"#
    } else {
        ""
    };
    let current = |key: &str| {
        if chrome.key == key {
            r#" aria-current="page""#
        } else {
            ""
        }
    };
    let body = substitute_placeholders(&String::from_utf8_lossy(&template.body), |key| {
        Some(match key {
            "LOCAL_AMOUNT" => &local_count,
            "AVAILABLE_AMOUNT" => &available_count,
            "PAGE_KEY" => chrome.key,
            "PAGE_TITLE" => chrome.title,
            "CURRENT_INSTALLED" => current("installed"),
            "CURRENT_AVAILABLE" => current("available"),
            "CURRENT_TOOLS" => current("tools"),
            "CURRENT_SETTINGS" => current("settings"),
            "PAGE_STATUS" => status,
            "SEARCH" => &search,
            "NOTICE" => notice,
            "LIST" => list,
            "UPDATE_HIDDEN" if update_version.is_some() => "",
            "UPDATE_HIDDEN" => "hidden",
            "CURRENT_VERSION" => &current_version,
            "NEW_VERSION" => update_version.unwrap_or_default(),
            _ => return None,
        })
    });
    EmbeddedResponse {
        body: Bytes::from(body),
        ..template
    }
}

/// Replaces `{{KEY}}` placeholders in a single pass. Substituted values are
/// never rescanned, so placeholder-shaped text in overlay metadata or catalog
/// content is emitted verbatim instead of being rewritten by a later key.
fn substitute_placeholders<'a>(template: &str, value: impl Fn(&str) -> Option<&'a str>) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after
            .find("}}")
            .and_then(|end| Some((end, value(&after[..end])?)))
        {
            Some((end, replacement)) => {
                output.push_str(replacement);
                rest = &after[end + 2..];
            }
            None => {
                output.push_str("{{");
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

/// Lowercases and removes every character that is neither alphanumeric nor
/// whitespace, so `"HD SC"` keeps its word boundary.
fn normalize_catalog_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn normalize_catalog_query(query: &str) -> String {
    normalize_catalog_text(query)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn query_tokens(query: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    for token in query.split_whitespace() {
        let token = if token == "sc" {
            "streamcompanion"
        } else {
            token
        };
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

pub(super) fn matches_catalog_query(
    name: &str,
    author: &str,
    compatible: &[String],
    query: &str,
) -> bool {
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return true;
    }
    let name = normalize_catalog_text(name);
    let author = normalize_catalog_text(author);
    let compatible = compatible
        .iter()
        .map(|value| normalize_catalog_text(value))
        .collect::<Vec<_>>();
    // Every word must match somewhere, like Tools and Settings search.
    tokens.into_iter().all(|token| {
        name.contains(token)
            || author.contains(token)
            || (token != "vosu" && compatible.iter().any(|value| value.contains(token)))
    })
}

/// `schemes[i]` is the declared preview colour scheme of `counters[i]`; a
/// missing entry keeps the light default. `names` holds dashboard nicknames
/// and URL aliases keyed by folder.
pub(super) fn render_local_counters(
    counters: &[crate::counters::Counter],
    schemes: &[PreviewScheme],
    names: &std::collections::BTreeMap<String, crate::counters::OverlayName>,
    query: Option<&str>,
    authority: &str,
) -> String {
    counters
        .iter()
        .enumerate()
        .filter(|(_, counter)| {
            query.is_none_or(|query| {
                let compatible = counter.compatiblewith.as_deref().unwrap_or_default();
                // Name and nickname are one field, as on the client.
                let nickname = names
                    .get(&counter.folder_name)
                    .and_then(|name| name.nickname.as_deref())
                    .unwrap_or_default();
                matches_catalog_query(
                    &format!("{} {nickname}", counter.name),
                    &counter.author,
                    compatible,
                    query,
                )
            })
        })
        .map(|(index, counter)| {
            let name = names.get(&counter.folder_name);
            let duplicate_name = counters
                .iter()
                .filter(|other| other.name.eq_ignore_ascii_case(&counter.name))
                .nth(1)
                .is_some();
            render_counter_card(
                &CounterCard {
                    name: &counter.name,
                    author: &counter.author,
                    version: &counter.version,
                    folder: &counter.folder_name,
                    downloaded: counter.downloaded,
                    updatable: counter.updatable,
                    download_link: counter.download_link.as_deref(),
                    author_links: &counter.authorlinks,
                    settings: !counter.settings.is_empty(),
                    resolution: &counter.resolution,
                    compatible: counter.compatiblewith.as_deref().unwrap_or_default(),
                    show_folder: duplicate_name,
                    preview_scheme: schemes.get(index).copied().unwrap_or_default(),
                    nickname: name.and_then(|name| name.nickname.as_deref()),
                    alias: name.and_then(|name| name.alias.as_deref()),
                    ..CounterCard::default()
                },
                authority,
            )
        })
        .collect()
}

pub(super) fn render_catalog_counters(
    counters: &[CatalogOverlay],
    query: Option<&str>,
    authority: &str,
) -> String {
    counters
        .iter()
        .filter(|counter| {
            query.is_none_or(|query| {
                matches_catalog_query(
                    &counter.name,
                    &counter.author,
                    counter.compatiblewith.as_deref().unwrap_or_default(),
                    query,
                )
            })
        })
        .map(|counter| {
            let assets = counter
                .assets
                .as_deref()
                .unwrap_or_default()
                .iter()
                // Catalog previews render as <img>; other media kinds have no
                // inline representation here.
                .filter(|asset| asset.kind == "image")
                .map(|asset| asset.url.as_str())
                .collect::<Vec<_>>();
            let version = counter
                .version
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| counter.version.to_string());
            render_counter_card(
                &CounterCard {
                    name: &counter.name,
                    author: &counter.author,
                    version: &version,
                    folder: counter.folder_name.as_deref().unwrap_or(&counter.name),
                    external: true,
                    downloaded: counter.downloaded,
                    updatable: counter.updatable,
                    download_link: counter.download_link.as_deref(),
                    author_links: &counter.authorlinks,
                    settings: counter
                        .settings
                        .as_ref()
                        .is_some_and(|value| !value.is_empty()),
                    assets: &assets,
                    resolution: &counter.resolution,
                    compatible: counter.compatiblewith.as_deref().unwrap_or_default(),
                    show_folder: false,
                    usecase: counter.usecase.as_ref(),
                    // Catalog cards show images, not live frames.
                    preview_scheme: PreviewScheme::Light,
                    nickname: None,
                    alias: None,
                },
                authority,
            )
        })
        .collect()
}

fn parse_absolute_http_url(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// Server path of a local overlay with every folder segment percent-encoded.
pub(super) fn overlay_path(folder: &str) -> String {
    let mut url = url::Url::parse("http://localhost/").expect("static base URL is valid");
    url.path_segments_mut()
        .expect("HTTP URLs have path segments")
        .pop_if_empty()
        .extend(folder.split('/'))
        .push("");
    url.path().to_owned()
}

/// Borrowed catalog or local fields for one card; borrowing avoids an
/// allocation-only intermediate model at this internal rendering boundary.
#[derive(Default)]
pub(super) struct CounterCard<'a> {
    pub(super) name: &'a str,
    pub(super) author: &'a str,
    pub(super) version: &'a str,
    pub(super) folder: &'a str,
    /// Catalog cards have no local identity, live preview, or management menu.
    pub(super) external: bool,
    pub(super) downloaded: bool,
    pub(super) updatable: bool,
    pub(super) download_link: Option<&'a str>,
    pub(super) author_links: &'a [String],
    pub(super) settings: bool,
    pub(super) assets: &'a [&'a str],
    pub(super) resolution: &'a [serde_json::Value],
    pub(super) compatible: &'a [String],
    /// Shows the folder under the title when another installed card shares its name.
    pub(super) show_folder: bool,
    /// Catalog use cases such as `ingame` or `obs-overlay`.
    pub(super) usecase: Option<&'a serde_json::Value>,
    /// Colour scheme the installed overlay declares; its preview frame matches it.
    pub(super) preview_scheme: PreviewScheme,
    /// Dashboard display name replacing `name` on installed cards.
    pub(super) nickname: Option<&'a str>,
    /// Short URL path (`/acc/`) that redirects to the folder.
    pub(super) alias: Option<&'a str>,
}

const PIN_ICON: &str = r#"<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M9 3h6l-1 6 3 3v2h-4v6l-1 1-1-1v-6H7v-2l3-3z"/></svg>"#;
const SETTINGS_ICON: &str = r#"<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12"/><circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/></svg>"#;

pub(super) fn render_counter_card(card: &CounterCard<'_>, authority: &str) -> String {
    let status = if card.updatable {
        " is-updatable"
    } else if card.downloaded {
        " is-installed"
    } else {
        ""
    };
    let identity = format!(
        r#" data-counter-name="{}" data-counter-title="{}" data-counter-nickname="{}" data-counter-alias="{}" data-counter-author="{}" data-counter-version="{}" data-counter-compatible="{}""#,
        escape_html(card.name),
        escape_html(card.nickname.unwrap_or(card.name)),
        escape_html(card.nickname.unwrap_or_default()),
        escape_html(card.alias.unwrap_or_default()),
        escape_html(card.author),
        escape_html(card.version),
        escape_html(&card.compatible.join(",")),
    );
    if card.external {
        render_catalog_card(card, status, &identity)
    } else {
        render_installed_card(card, status, &identity, authority)
    }
}

fn render_author_links(name: &str, author_links: &[String]) -> String {
    author_links
        .iter()
        .filter_map(|link| parse_absolute_http_url(link))
        .enumerate()
        .map(|(index, link)| {
            let link = link.as_str();
            let label = if link.contains("github.com") {
                "GitHub"
            } else if link.contains("twitter.com") || link.contains("x.com") {
                "X / Twitter"
            } else if link.contains("discord.gg") || link.contains("discord.com") {
                "Discord"
            } else {
                "Website"
            };
            format!(
                r#"<a class="author-link" href="{}" target="_blank" rel="noopener noreferrer" aria-label="{} author link {}">{label}</a>"#,
                escape_html(link),
                escape_html(name),
                index + 1,
            )
        })
        .collect()
}

/// Formats a `[width, height]` resolution, where `-1`/`-2` mean any size.
fn resolution_text(resolution: &[serde_json::Value]) -> String {
    let size = |value: Option<f64>| match value {
        Some(value) if value != -1.0 && value != -2.0 => format_number(value),
        _ => "ANY".to_owned(),
    };
    format!(
        "{} × {}",
        size(resolution.first().and_then(json_number)),
        size(resolution.get(1).and_then(json_number)),
    )
}

fn usecase_labels(usecase: Option<&serde_json::Value>) -> Vec<String> {
    let values = match usecase {
        Some(serde_json::Value::Array(values)) => {
            values.iter().filter_map(|v| v.as_str()).collect()
        }
        Some(serde_json::Value::String(value)) => vec![value.as_str()],
        _ => Vec::new(),
    };
    values
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter(|value| !value.trim().is_empty())
        .map(|value| match value.trim().to_ascii_lowercase().as_str() {
            "ingame" | "in-game" | "in game" => "In-game".to_owned(),
            "obs-overlay" | "obs-overlays" | "obs" => "OBS".to_owned(),
            _ => value.trim().to_owned(),
        })
        .collect()
}

fn display_version(version: &str) -> String {
    let raw = version.trim().trim_start_matches('v');
    let mut parts: Vec<&str> = raw.split('.').collect();
    if parts.len() == 3 && parts[2] == "0" {
        parts.pop();
    }
    parts.join(".")
}

/// Catalog entries are listing rows led by one preview image at a time; the
/// gallery arrows browse the remaining images. Hidden images stay lazy, so
/// only the shown preview of each visible row is fetched.
fn render_catalog_card(card: &CounterCard<'_>, status: &str, identity: &str) -> String {
    let name = escape_html(card.name);
    let install_name = escape_html(&format!("{} by {}", card.name, card.author));
    let download = escape_html(card.download_link.unwrap_or_default());
    let update = if card.updatable && card.download_link.is_some() {
        format!(
            r#"<button class="button button-update" type="button" data-action="update-counter" data-download-url="{download}" data-install-name="{install_name}" aria-label="Update {name}">Update</button>"#
        )
    } else {
        String::new()
    };
    let primary = if !card.downloaded && card.download_link.is_some() {
        format!(
            r#"<button class="button button-primary" type="button" data-action="download-counter" data-download-url="{download}" data-install-name="{install_name}" aria-label="Install {name}">Install</button>"#
        )
    } else if card.downloaded {
        format!(
            r#"<button class="button" type="button" data-action="open-counter" data-folder="{install_name}" aria-label="Open {name} folder">Open folder</button>"#
        )
    } else {
        r#"<button class="button" type="button" disabled>Unavailable</button>"#.to_owned()
    };

    let images = card
        .assets
        .iter()
        .filter_map(|url| parse_absolute_http_url(url))
        .collect::<Vec<_>>();
    let count = images.len();
    let slides = images
        .iter()
        .enumerate()
        .map(|(index, url)| {
            format!(
                r#"<img class="gallery__slide" src="{}" alt="{name} preview {} of {count}" loading="lazy" decoding="async" referrerpolicy="no-referrer"{}>"#,
                escape_html(url.as_str()),
                index + 1,
                if index == 0 { "" } else { " hidden" },
            )
        })
        .collect::<String>();
    let enlarge = format!(
        r#"<button class="preview-expand" type="button" data-action="expand-image" aria-label="Enlarge {name} preview" title="Enlarge preview"><span>Enlarge</span></button>"#
    );
    let gallery = match count {
        0 => r#"<div class="gallery gallery--empty"><span>No preview provided</span></div>"#
            .to_owned(),
        1 => format!(r#"<div class="gallery">{slides}{enlarge}</div>"#),
        _ => format!(
            concat!(
                r#"<div class="gallery" data-gallery-index="0">{slides}{enlarge}"#,
                r#"<div class="gallery__controls"><button class="gallery__nav gallery__nav--prev" type="button" data-action="gallery-prev" aria-label="Previous {name} preview">‹</button>"#,
                r#"<button class="gallery__nav gallery__nav--next" type="button" data-action="gallery-next" aria-label="Next {name} preview">›</button>"#,
                r#"<span class="gallery__count" aria-live="polite">1 / {count}</span></div></div>"#,
            ),
            slides = slides,
            enlarge = enlarge,
            name = name,
            count = count,
        ),
    };

    let mut facts = format!(
        r#"<span><span class="fact-label">Size</span> {}</span>"#,
        escape_html(&resolution_text(card.resolution)),
    );
    let usecases = usecase_labels(card.usecase);
    if !usecases.is_empty() {
        facts.push_str(&format!(
            r#"<span><span class="fact-label">For</span> {}</span>"#,
            escape_html(&usecases.join(", ")),
        ));
    }
    if !card.compatible.is_empty() {
        facts.push_str(&format!(
            r#"<span><span class="fact-label">Works with</span> {}</span>"#,
            escape_html(&card.compatible.join(", ")),
        ));
    }
    // One Available list holds catalog-source and community overlays; the
    // badge names where each comes from.
    let mut tags = String::from(r#"<span class="badge" data-source="community">Community</span>"#);
    if card.updatable && card.download_link.is_some() {
        tags.push_str(r#"<span class="badge badge-warning">Update available</span>"#);
    } else if card.downloaded {
        tags.push_str(r#"<span class="badge">Installed</span>"#);
    }
    format!(
        concat!(
            r#"<article class="counter-card catalog-card{status}"{identity} aria-label="{name}">{gallery}"#,
            r#"<div class="catalog-card__main"><div class="counter-card__title-row"><h2 title="{name}">{name}</h2><span class="counter-card__version">v{version}</span></div>"#,
            r#"<p class="counter-card__meta">{author}{links}</p>"#,
            r#"<p class="catalog-card__facts">{facts}</p>"#,
            r#"<div class="catalog-card__footer"><p class="catalog-card__tags">{tags}</p><div class="counter-actions">{update}{primary}</div></div>"#,
            "</div></article>",
        ),
        status = status,
        identity = identity,
        gallery = gallery,
        name = name,
        version = escape_html(&display_version(card.version)),
        author = if card.author.trim().eq_ignore_ascii_case("unknown")
            || card.author.trim().is_empty()
        {
            String::new()
        } else {
            format!("<span>by {}</span>", escape_html(card.author))
        },
        links = render_author_links(card.name, card.author_links),
        facts = facts,
        tags = tags,
        update = update,
        primary = primary,
    )
}

/// Installed cards share one fixed-size preview frame regardless of overlay
/// resolution, so a long list scans as an even grid; the URL bar is the
/// card's primary target.
fn render_installed_card(
    card: &CounterCard<'_>,
    status: &str,
    identity: &str,
    authority: &str,
) -> String {
    // The nickname names the overlay everywhere on the dashboard; the folder
    // remains its identity and its frame's address.
    let name = escape_html(card.nickname.unwrap_or(card.name));
    let folder = escape_html(card.folder);
    let path = overlay_path(card.folder);
    // Preview frames mark themselves so their own sockets are not counted as
    // connected browser sources.
    let preview_url = escape_html(&format!(
        "http://{authority}{path}?{}",
        crate::server::pages::OVERLAY_PREVIEW_QUERY
    ));
    // Browser sources may use the alias, which redirects to the folder.
    let shown_path = card.alias.map_or_else(
        || format!("/{}/", card.folder),
        |alias| format!("/{alias}/"),
    );
    let copy_url = escape_html(&card.alias.map_or_else(
        || format!("http://{authority}{path}"),
        |alias| format!("http://{authority}/{alias}/"),
    ));
    let settings = if card.settings {
        format!(
            r#"<button class="icon-button" type="button" data-action="counter-settings" data-folder="{folder}" aria-label="Settings for {name}" title="Settings">{SETTINGS_ICON}</button>"#
        )
    } else {
        String::new()
    };
    // Encoded folder paths are unique, whitespace-free element IDs.
    let element = path.trim_matches('/').to_owned();
    let menu = escape_html(&format!("card-menu-{element}"));
    let title_id = escape_html(&format!("card-title-{element}"));
    // One stop each for pin, settings and the menu; the resolution copy,
    // enlarged preview and open-in-browser actions live in the menu so a
    // long grid stays quick to tab through.
    let actions = format!(
        concat!(
            r#"<div class="counter-actions">"#,
            r#"<button class="pin-toggle" type="button" data-action="toggle-pin" aria-pressed="false" aria-label="Pin {name}" title="Pin">{icon}</button>"#,
            "{settings}",
            r#"<button class="icon-button card-menu-toggle" type="button" popovertarget="{menu}" aria-label="More actions for {name}" title="More actions">⋯</button>"#,
            r#"<div class="card-menu" id="{menu}" popover role="menu" aria-label="Actions for {name}">"#,
            r#"<button type="button" role="menuitem" data-action="expand-preview">Enlarge preview</button>"#,
            r#"<button type="button" role="menuitem" data-action="open-url" data-url="{url}">Open in browser</button>"#,
            r#"<button type="button" role="menuitem" data-action="copy-metadata" data-copy-label="Resolution" data-copy-value="{resolution}">Copy resolution</button>"#,
            r#"<button type="button" role="menuitem" data-action="open-counter" data-folder="{folder}">Open folder</button>"#,
            r#"<button type="button" role="menuitem" data-action="rename-counter" data-folder="{folder}">Nickname and URL…</button>"#,
            r#"<button class="card-menu__pin" type="button" role="menuitem" data-action="pin-earlier">Move pin earlier</button>"#,
            r#"<button class="card-menu__pin" type="button" role="menuitem" data-action="pin-later">Move pin later</button>"#,
            r#"<hr>"#,
            r#"<button class="card-menu__danger" type="button" role="menuitem" data-action="delete-counter" data-folder="{folder}">Delete</button>"#,
            "</div></div>",
        ),
        name = name,
        icon = PIN_ICON,
        settings = settings,
        menu = menu,
        folder = folder,
        url = copy_url,
        resolution = escape_html(&resolution_text(card.resolution)),
    );

    let x = card
        .resolution
        .first()
        .and_then(json_number)
        .unwrap_or(-1.0);
    let y = card.resolution.get(1).and_then(json_number).unwrap_or(-1.0);
    // `None` is a fluid width that fills the frame instead of being scaled.
    let width = if x == -1.0 {
        Some(500.0)
    } else if x == -2.0 || x <= 10.0 {
        None
    } else {
        Some(x)
    };
    let height = if y == -1.0 {
        500.0
    } else if x <= 10.0 || y <= 10.0 {
        300.0
    } else {
        y
    };
    let resolution = escape_html(&resolution_text(card.resolution));
    let height = format_number(height);
    let (stage, width) = match width {
        Some(width) => {
            let width = format_number(width);
            (
                format!(
                    r#"class="preview-stage" data-preview-width="{width}" data-preview-height="{height}""#
                ),
                width,
            )
        }
        None => (
            r#"class="preview-stage is-fluid""#.to_owned(),
            "100%".to_owned(),
        ),
    };
    let folder_line = if card.nickname.is_some() {
        format!(
            r#"<p class="counter-card__folder" title="Folder: {folder}"><span class="visually-hidden">Overlay: </span>{}</p>"#,
            escape_html(card.name)
        )
    } else if card.show_folder {
        format!(
            r#"<p class="counter-card__folder"><span class="visually-hidden">Folder: </span>{folder}</p>"#
        )
    } else {
        String::new()
    };
    let update = if card.updatable && card.download_link.is_some() {
        format!(
            r#"<button class="button button-update" type="button" data-action="update-counter" data-download-url="{}" data-install-name="{folder}" aria-label="Update {name}">Update</button>"#,
            escape_html(card.download_link.unwrap_or_default())
        )
    } else {
        String::new()
    };
    format!(
        concat!(
            r#"<article class="counter-card{status}" data-overlay-id="{folder}"{identity} aria-labelledby="{title_id}">"#,
            r#"<div class="counter-card__preview"><div {stage}><iframe src="{src}"{scheme} loading="lazy" width="{width}" height="{height}" scrolling="no" tabindex="-1" title="{name} overlay preview"></iframe></div><button class="preview-expand" type="button" data-action="expand-preview" tabindex="-1" aria-label="Enlarge {name} preview"><span aria-hidden="true">Enlarge</span></button></div>"#,
            r#"<div class="counter-card__body"><div class="counter-card__identity"><h2 id="{title_id}" title="{name}">{name}</h2>"#,
            r#"<p class="counter-card__meta">{version}{author}<span class="counter-card__size">{resolution}</span></p>"#,
            r#"{folder_line}<div class="counter-card__chips">{update}<span class="badge connected-chip" hidden></span></div></div>{actions}</div>"#,
            r#"<button class="counter-card__url" type="button" data-action="copy-metadata" data-copy-label="URL" data-copy-value="{url}" title="{url}" aria-label="Copy URL {url}"><code>{shown}</code><span class="counter-card__url-action" aria-hidden="true">Copy URL</span></button>"#,
            "</article>",
        ),
        title_id = title_id,
        status = status,
        folder = folder,
        identity = identity,
        stage = stage,
        src = &preview_url,
        scheme = match card.preview_scheme {
            PreviewScheme::Light => "",
            PreviewScheme::Dark => r#" data-color-scheme="dark""#,
        },
        width = width,
        height = height,
        name = name,
        version = if card.version.trim().is_empty() {
            String::new()
        } else {
            format!(
                "<span>v{}</span>",
                escape_html(&display_version(card.version))
            )
        },
        author = if card.author.trim().is_empty()
            || card.author.trim().eq_ignore_ascii_case("unknown")
        {
            String::new()
        } else {
            format!("<span>{}</span>", escape_html(card.author.trim()))
        },
        resolution = resolution,
        folder_line = folder_line,
        actions = actions,
        update = update,
        url = copy_url,
        shown = escape_html(&shown_path),
    )
}

pub(super) fn empty_counter_search() -> String {
    r#"<div class="empty-state"><h2>No matching overlays</h2><p>Try fewer words, an author, or a client such as tosu.</p></div>"#.to_owned()
}

/// Boolean settings are switches; the row's description is linked rather
/// than wrapped into the label, so the accessible name stays the title.
pub(super) fn settings_switch(id: &str, checked: bool) -> String {
    let id = escape_html(id);
    format!(
        r#"<input class="settings-control v-toggle" id="setting-{id}" data-id="{id}" type="checkbox" role="switch" aria-describedby="setting-{id}-description"{}>"#,
        if checked { " checked" } else { "" },
    )
}

pub(super) fn settings_text(id: &str, value: &str, class: &str) -> String {
    let id = escape_html(id);
    format!(
        r#"<input class="settings-control{class}" id="setting-{id}" data-id="{id}" type="text" value="{}" spellcheck="false" aria-describedby="setting-{id}-description">"#,
        escape_html(value),
    )
}

/// Numeric control whose bounds mirror the server-side save validation, so the
/// browser rejects out-of-range values before they are sent. A unit sits
/// inside the field's right edge, so every field in a column keeps one width.
pub(super) fn settings_number(
    id: &str,
    value: f64,
    min: f64,
    max: f64,
    step: &str,
    unit: Option<&str>,
) -> String {
    let id = escape_html(id);
    let input = format!(
        r#"<input class="settings-control" id="setting-{id}" data-id="{id}" type="number" value="{}" min="{}" max="{}" step="{step}" required aria-describedby="setting-{id}-description">"#,
        format_number(value),
        format_number(min),
        format_number(max),
    );
    match unit {
        Some(unit) => format!(
            r#"<span class="settings-unit-field">{input}<span class="settings-unit" aria-hidden="true">{}</span></span>"#,
            escape_html(unit)
        ),
        None => input,
    }
}

/// `id` is the control's `data-id`: the title labels `setting-{id}` and the
/// description is its `aria-describedby` target.
pub(super) fn settings_item(id: &str, name: &str, description: &str, inputs: &str) -> String {
    let id = escape_html(id);
    format!(
        r#"<div class="settings-item"><span class="settings-item__copy"><label class="settings-item__name" for="setting-{id}">{}</label><span class="settings-item__description" id="setting-{id}-description">{}</span></span><span class="settings-item__control">{inputs}</span></div>"#,
        escape_html(name),
        escape_html(description),
    )
}

pub(super) fn settings_group(header: &str, items: &[String]) -> String {
    format!(
        r#"<fieldset class="settings-group"><legend>{}</legend>{}</fieldset>"#,
        escape_html(header),
        items.join("\n"),
    )
}

pub(super) fn render_runtime_settings(config: &Config) -> String {
    let externally_managed = crate::desktop::externally_managed();
    let mut update_switch = settings_switch("ENABLE_AUTOUPDATE", config.enable_auto_update);
    if externally_managed {
        update_switch.insert_str(update_switch.len() - 1, " disabled");
    }
    let switch = |id: &str, name: &str, description: &str, checked: bool| {
        settings_item(id, name, description, &settings_switch(id, checked))
    };
    let general_items = [
        settings_item(
            "ENABLE_AUTOUPDATE",
            "Automatic updates",
            if externally_managed {
                "Updates are managed by your Linux package manager; automatic updates are disabled."
            } else {
                "Install new vosu releases automatically during startup. vosu checks for them at every start either way, so it can show the update notice."
            },
            &update_switch,
        ),
        switch(
            "OPEN_DASHBOARD_ON_STARTUP",
            "Open dashboard at startup",
            "Open this local dashboard in the default browser after vosu starts.",
            config.open_dashboard_on_startup,
        ),
        #[cfg(target_os = "linux")]
        switch(
            "START_AT_LOGIN",
            "Start at login",
            "Run vosu as a systemd user service when you log in.",
            config.start_at_login,
        ),
    ];
    let general = settings_group("General", &general_items);
    let interval = |id: &str, name: &str, description: &str, value: f64, min: f64| {
        settings_item(
            id,
            name,
            description,
            &settings_number(
                id,
                value,
                min,
                ConfigDocument::MAX_INTERVAL_MS,
                "any",
                Some("ms"),
            ),
        )
    };
    let data = settings_group(
        "Game data",
        &[
            switch(
                "CALCULATE_PP",
                "Calculate performance points",
                "Compute live performance points. Disable this when overlays do not need PP data.",
                config.calculate_pp,
            ),
            switch(
                "READ_MANIA_SCROLL_SPEED",
                "Read mania scroll speed",
                "Expose the current mania scroll speed from game memory.",
                config.read_mania_scroll_speed,
            ),
            switch(
                "ENABLE_KEY_OVERLAY",
                "Read key overlay data",
                "Expose live K1, K2, M1, and M2 input state to overlays.",
                config.enable_key_overlay,
            ),
            switch(
                "ENABLE_MANIA_KEY_HIT_ERRORS",
                "Per-key mania hit errors",
                "Expose opt-in press and release timing arrays for known mania columns.",
                config.enable_mania_key_hit_errors,
            ),
            switch(
                "ENABLE_HIT_OFFSETS",
                "Standard hit offsets",
                "Expose cursor offsets and timing errors for standard circle and slider-head hits.",
                config.enable_hit_offsets,
            ),
            interval(
                "POLL_RATE",
                "Common poll interval",
                "Milliseconds between reads of general game data.",
                config.poll_rate,
                100.0,
            ),
            interval(
                "PRECISE_DATA_POLL_RATE",
                "Precise poll interval",
                "Milliseconds between reads of precise key and hit-error data.",
                config.precise_data_poll_rate,
                1.0,
            ),
            interval(
                "V2_SUBSCRIPTION_DEBOUNCE",
                "V2 subscription debounce",
                "Wait at least this long between updates sent to overlays that request selected V2 fields.",
                config.v2_subscription_debounce,
                1.0,
            ),
        ],
    );
    let server = settings_group(
        "Server",
        &[
            settings_item(
                "SERVER_IP",
                "Listener address",
                "IP address where the vosu HTTP server listens.",
                &settings_text("SERVER_IP", &config.server_ip, " settings-control--mono"),
            ),
            settings_item(
                "SERVER_PORT",
                "Listener port",
                "Port for the vosu HTTP server, from 1 through 65535. Ports below 1024 usually require administrator rights.",
                &settings_number("SERVER_PORT", config.server_port, 1.0, 65_535.0, "1", None),
            ),
            settings_item(
                "DASHBOARD_IP",
                "Dashboard address",
                "Trusted dashboard listener. Loopback-only by default, independently of the overlay listener.",
                &settings_text(
                    "DASHBOARD_IP",
                    &config.dashboard_ip,
                    " settings-control--mono",
                ),
            ),
            settings_item(
                "DASHBOARD_PORT",
                "Dashboard port",
                "Separate port for trusted management pages. Overlays keep their original port and browser storage.",
                &settings_number(
                    "DASHBOARD_PORT",
                    config.dashboard_port,
                    1.0,
                    65_535.0,
                    "1",
                    None,
                ),
            ),
            settings_item(
                "ALLOWED_IPS",
                "Allowed IP addresses",
                "One IP address or CIDR range per line. Prefer CIDR, such as 100.64.0.0/10 for Tailscale, over legacy wildcard octets that may include public addresses (100.*.*.* is broader than Tailscale). The literal legacy hostname/raw-header token absolute comes from tosu defaults; it is not a wildcard or a peer-policy bypass.",
                &format!(
                    r#"<textarea class="settings-control allowlist-editor" id="setting-ALLOWED_IPS" data-id="ALLOWED_IPS" wrap="off" rows="5" spellcheck="false" aria-describedby="setting-ALLOWED_IPS-description allowlist-warning">{}</textarea><span id="allowlist-warning" class="allowlist-warning" role="status"></span>"#,
                    escape_html(
                        &config
                            .allowed_ips
                            .split(',')
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                ),
            ),
        ],
    );
    let advanced = settings_group(
        "Advanced",
        &[
            switch(
                "DEBUG_LOG",
                "Debug logging",
                "Write verbose diagnostics. Enable this only while troubleshooting.",
                config.debug_log,
            ),
            switch(
                "SHOW_MP_COMMANDS",
                "Show multiplayer commands",
                "Expose Bancho multiplayer commands in tournament chat data.",
                config.show_mp_commands,
            ),
            settings_item(
                "STATIC_FOLDER_PATH",
                "Overlays directory",
                "Directory vosu scans for local overlays. Changes need Apply.",
                &settings_text(
                    "STATIC_FOLDER_PATH",
                    &config.static_folder_path,
                    " settings-control--mono",
                ),
            ),
        ],
    );
    // The affiliation notice and version live here instead of a page footer.
    let about = format!(
        concat!(
            r#"<fieldset class="settings-group settings-group--about"><legend>About</legend>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Version</span></span><span class="settings-item__control settings-item__value">vosu {version}</span></div>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Source</span></span><span class="settings-item__control settings-item__value"><a href="https://github.com/shiruviaa/vosu" target="_blank" rel="noopener noreferrer">github.com/shiruviaa/vosu</a></span></div>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Affiliation</span></span><span class="settings-item__control settings-item__value">Not affiliated with ppy or the tosu project.</span></div>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Made by</span></span><span class="settings-item__control settings-item__value">18-LAB</span></div>"#,
            "</fieldset>",
        ),
        version = escape_html(&current_version().to_string()),
    );
    format!(
        r#"<form class="settings" id="runtime-settings">{general}{data}{server}{advanced}{about}<p class="settings-empty" hidden>No settings match your search.</p></form>"#
    )
}

pub(super) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
