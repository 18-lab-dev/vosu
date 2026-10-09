use super::*;

pub(super) fn local_overlay(counter: &crate::counters::Counter) -> LocalOverlay {
    LocalOverlay {
        name: counter.name.clone(),
        author: counter.author.clone(),
        version: counter.version.clone(),
    }
}

const SEARCH_FORM: &str = r#"<form class="search-form" role="search"><label class="visually-hidden" for="counter-search">Search overlays</label><div class="search-control"><input id="counter-search" class="search-bar" type="search" autocomplete="off" placeholder="Search name, author, or client" aria-controls="counter-results"><kbd aria-hidden="true">/</kbd></div></form>"#;

pub(super) fn render_homepage(
    template: EmbeddedResponse,
    local_count: Option<usize>,
    available_count: Option<usize>,
    search: bool,
    list: &str,
    update_version: Option<&str>,
) -> EmbeddedResponse {
    let local_count = local_count
        .map(|count| count.to_string())
        .unwrap_or_default();
    let available_count = available_count
        .map(|count| count.to_string())
        .unwrap_or_default();
    let current_version = current_version().to_string();
    let body = substitute_placeholders(&String::from_utf8_lossy(&template.body), |key| {
        Some(match key {
            "LOCAL_AMOUNT" => &local_count,
            "AVAILABLE_AMOUNT" => &available_count,
            "SEARCH" if search => SEARCH_FORM,
            "SEARCH" => "",
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
    tokens.into_iter().any(|token| {
        name.contains(token)
            || author.contains(token)
            || (token != "vosu" && compatible.iter().any(|value| value.contains(token)))
    })
}

pub(super) fn render_local_counters(
    counters: &[crate::counters::Counter],
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
                    external_settings: counter.has_settings,
                    assets: &assets,
                    resolution: &counter.resolution,
                    compatible: counter.compatiblewith.as_deref().unwrap_or_default(),
                    show_folder: false,
                    usecase: counter.usecase.as_ref(),
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
    pub(super) external_settings: bool,
    pub(super) assets: &'a [&'a str],
    pub(super) resolution: &'a [serde_json::Value],
    pub(super) compatible: &'a [String],
    /// Shows the folder under the title when another installed card shares its name.
    pub(super) show_folder: bool,
    /// Catalog use cases such as `ingame` or `obs-overlay`.
    pub(super) usecase: Option<&'a serde_json::Value>,
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
        r#" data-counter-name="{}" data-counter-author="{}" data-counter-version="{}" data-counter-compatible="{}""#,
        escape_html(card.name),
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
        .filter(|value| !value.trim().is_empty())
        .map(|value| match value {
            "ingame" => "In-game".to_owned(),
            "obs-overlay" => "OBS".to_owned(),
            other => other.to_owned(),
        })
        .collect()
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
            r#"<button class="button button-cyan" type="button" data-action="update-counter" data-download-url="{download}" data-install-name="{install_name}">Update</button>"#
        )
    } else {
        String::new()
    };
    let primary = if !card.downloaded && card.download_link.is_some() {
        format!(
            r#"<button class="button button-primary" type="button" data-action="download-counter" data-download-url="{download}" data-install-name="{install_name}">Install</button>"#
        )
    } else if card.downloaded {
        format!(
            r#"<button class="button" type="button" data-action="open-counter" data-folder="{install_name}">Open folder</button>"#
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
                r#"<button class="gallery__nav gallery__nav--prev" type="button" data-action="gallery-prev" aria-label="Previous {name} preview">‹</button>"#,
                r#"<button class="gallery__nav gallery__nav--next" type="button" data-action="gallery-next" aria-label="Next {name} preview">›</button>"#,
                r#"<span class="gallery__count" aria-live="polite">1 / {count}</span></div>"#,
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
    let mut tags = String::new();
    if card.downloaded {
        tags.push_str(r#"<span class="badge">Installed</span>"#);
    }
    if card.updatable {
        tags.push_str(r#"<span class="badge badge-cyan">Update available</span>"#);
    }
    if card.external_settings {
        tags.push_str(r#"<span class="badge badge-cyan">Has settings</span>"#);
    }
    format!(
        concat!(
            r#"<article class="counter-card catalog-card{status}"{identity}>{gallery}"#,
            r#"<div class="catalog-card__main"><div class="counter-card__title-row"><h2 title="{name}">{name}</h2><span class="counter-card__version">v{version}</span></div>"#,
            r#"<p class="counter-card__meta"><span>by {author}</span>{links}</p>"#,
            r#"<p class="catalog-card__facts">{facts}</p>"#,
            r#"<div class="catalog-card__footer"><p class="catalog-card__tags">{tags}</p><div class="counter-actions">{update}{primary}</div></div>"#,
            "</div></article>",
        ),
        status = status,
        identity = identity,
        gallery = gallery,
        name = name,
        version = escape_html(card.version),
        author = escape_html(card.author),
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
    let name = escape_html(card.name);
    let folder = escape_html(card.folder);
    let path = overlay_path(card.folder);
    let absolute_url = escape_html(&format!("http://{authority}{path}"));
    let settings = if card.settings {
        format!(
            r#"<button class="icon-button" type="button" data-action="counter-settings" data-folder="{folder}" aria-label="Settings for {name}" title="Settings">{SETTINGS_ICON}</button>"#
        )
    } else {
        String::new()
    };
    // Encoded folder paths are unique, whitespace-free element IDs.
    let menu = escape_html(&format!("card-menu-{}", path.trim_matches('/')));
    let actions = format!(
        concat!(
            r#"<div class="counter-actions">"#,
            r#"<button class="pin-toggle" type="button" data-action="toggle-pin" aria-pressed="false" aria-label="Pin {name}" title="Pin">{icon}</button>"#,
            "{settings}",
            r#"<button class="icon-button card-menu-toggle" type="button" popovertarget="{menu}" aria-label="More actions for {name}" title="More actions">⋯</button>"#,
            r#"<div class="card-menu" id="{menu}" popover>"#,
            r#"<button type="button" data-action="open-counter" data-folder="{folder}">Open folder</button>"#,
            r#"<button class="card-menu__pin" type="button" data-action="pin-earlier">Move pin earlier</button>"#,
            r#"<button class="card-menu__pin" type="button" data-action="pin-later">Move pin later</button>"#,
            r#"<hr>"#,
            r#"<button class="card-menu__danger" type="button" data-action="delete-counter" data-folder="{folder}">Delete</button>"#,
            "</div></div>",
        ),
        name = name,
        icon = PIN_ICON,
        settings = settings,
        menu = menu,
        folder = folder,
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
    let folder_line = if card.show_folder {
        format!(
            r#"<p class="counter-card__folder"><span class="visually-hidden">Folder: </span>{folder}</p>"#
        )
    } else {
        String::new()
    };
    format!(
        concat!(
            r#"<article class="counter-card{status}" data-overlay-id="{folder}"{identity}>"#,
            r#"<div class="counter-card__preview"><div {stage}><iframe src="{src}" loading="lazy" width="{width}" height="{height}" scrolling="no" tabindex="-1" title="{name} overlay preview"></iframe></div><button class="preview-expand" type="button" data-action="expand-preview" aria-label="Enlarge {name} preview" title="Enlarge preview"><span>Enlarge</span></button></div>"#,
            r#"<div class="counter-card__body"><div class="counter-card__identity"><h2 title="{name}">{name}</h2>"#,
            r#"<p class="counter-card__meta"><span>v{version}</span><span>{author}</span><button class="meta-copy" type="button" data-action="copy-metadata" data-copy-label="Resolution" data-copy-value="{resolution}" title="Copy resolution">{resolution}</button></p>"#,
            "{folder_line}</div>{actions}</div>",
            r#"<button class="counter-card__url" type="button" data-action="copy-metadata" data-copy-label="URL" data-copy-value="{url}" title="{url} (click to copy, Shift-click to open)"><code>/{folder}/</code><span class="counter-card__url-action">Copy URL</span></button>"#,
            "</article>",
        ),
        status = status,
        folder = folder,
        identity = identity,
        stage = stage,
        src = escape_html(&path),
        width = width,
        height = height,
        name = name,
        version = escape_html(card.version),
        author = escape_html(card.author),
        resolution = resolution,
        folder_line = folder_line,
        actions = actions,
        url = absolute_url,
    )
}

pub(super) fn empty_counter_search() -> String {
    r#"<div class="empty-state"><h2>No matching overlays</h2><p>Try a shorter name, a different author, or another compatible client.</p></div>"#.to_owned()
}

pub(super) fn settings_switch(id: &str, checked: bool) -> String {
    format!(
        r#"<input class="settings-control" id="setting-{}" data-id="{}" type="checkbox" {}>"#,
        escape_html(id),
        escape_html(id),
        if checked { "checked" } else { "" },
    )
}

pub(super) fn settings_text(id: &str, value: &str, _class: &str) -> String {
    format!(
        r#"<input class="settings-control" id="setting-{}" data-id="{}" type="text" value="{}">"#,
        escape_html(id),
        escape_html(id),
        escape_html(value),
    )
}

/// Numeric control whose bounds mirror the server-side save validation, so the
/// browser rejects out-of-range values before they are sent.
pub(super) fn settings_number(id: &str, value: f64, min: f64, max: f64, step: &str) -> String {
    format!(
        r#"<input class="settings-control" id="setting-{}" data-id="{}" type="number" value="{}" min="{}" max="{}" step="{step}" required>"#,
        escape_html(id),
        escape_html(id),
        format_number(value),
        format_number(min),
        format_number(max),
    )
}

pub(super) fn settings_item(name: &str, description: &str, inputs: &str) -> String {
    format!(
        r#"<label class="settings-item"><span class="settings-item__copy"><span class="settings-item__name">{}</span><span class="settings-item__description">{}</span></span><span class="settings-item__control">{inputs}</span></label>"#,
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
    let general_items = [
        settings_item(
            "Automatic updates",
            if externally_managed {
                "Updates are managed by your Linux package manager; automatic updates are disabled."
            } else {
                "Check for a new Vosu release during startup and install it automatically."
            },
            &update_switch,
        ),
        settings_item(
            "Open dashboard at startup",
            "Open this local dashboard in the default browser after Vosu starts.",
            &settings_switch(
                "OPEN_DASHBOARD_ON_STARTUP",
                config.open_dashboard_on_startup,
            ),
        ),
        #[cfg(target_os = "linux")]
        settings_item(
            "Start at login",
            "Run vosu as a systemd user service when you log in.",
            &settings_switch("START_AT_LOGIN", config.start_at_login),
        ),
    ];
    let general = settings_group("General", &general_items);
    let data = settings_group(
        "Game data",
        &[
            settings_item(
                "Calculate performance points",
                "Compute live performance points. Disable this when overlays do not need PP data.",
                &settings_switch("CALCULATE_PP", config.calculate_pp),
            ),
            settings_item(
                "Read mania scroll speed",
                "Expose the current mania scroll speed from game memory.",
                &settings_switch("READ_MANIA_SCROLL_SPEED", config.read_mania_scroll_speed),
            ),
            settings_item(
                "Read key overlay data",
                "Expose live K1, K2, M1, and M2 input state to overlays.",
                &settings_switch("ENABLE_KEY_OVERLAY", config.enable_key_overlay),
            ),
            settings_item(
                "Common poll interval",
                "Milliseconds between reads of general game data.",
                &settings_number(
                    "POLL_RATE",
                    config.poll_rate,
                    100.0,
                    ConfigDocument::MAX_INTERVAL_MS,
                    "any",
                ),
            ),
            settings_item(
                "Precise poll interval",
                "Milliseconds between reads of precise key and hit-error data.",
                &settings_number(
                    "PRECISE_DATA_POLL_RATE",
                    config.precise_data_poll_rate,
                    1.0,
                    ConfigDocument::MAX_INTERVAL_MS,
                    "any",
                ),
            ),
            settings_item(
                "V2 subscription debounce",
                "Minimum milliseconds between selected-field WebSocket frames.",
                &settings_number(
                    "V2_SUBSCRIPTION_DEBOUNCE",
                    config.v2_subscription_debounce,
                    0.0,
                    ConfigDocument::MAX_INTERVAL_MS,
                    "any",
                ),
            ),
        ],
    );
    let server = settings_group(
        "Server",
        &[
            settings_item(
                "Listener address",
                "IP address where the Vosu HTTP server listens.",
                &settings_text("SERVER_IP", &config.server_ip, ""),
            ),
            settings_item(
                "Listener port",
                "Port for the Vosu HTTP server, from 1 through 65535. Ports below 1024 usually require administrator rights.",
                &settings_number("SERVER_PORT", config.server_port, 1.0, 65_535.0, "1"),
            ),
            settings_item(
                "Allowed IP addresses",
                "Comma-separated clients permitted to connect. Wildcard octets such as 192.168.*.* are supported.",
                &format!(
                    r#"<textarea class="settings-control" id="setting-ALLOWED_IPS" data-id="ALLOWED_IPS">{}</textarea>"#,
                    escape_html(&config.allowed_ips),
                ),
            ),
        ],
    );
    let advanced = settings_group(
        "Advanced",
        &[
            settings_item(
                "Debug logging",
                "Write verbose diagnostics. Enable this only while troubleshooting.",
                &settings_switch("DEBUG_LOG", config.debug_log),
            ),
            settings_item(
                "Show multiplayer commands",
                "Expose Bancho multiplayer commands in tournament chat data.",
                &settings_switch("SHOW_MP_COMMANDS", config.show_mp_commands),
            ),
            settings_item(
                "Overlays directory",
                "Directory Vosu scans for local overlays.",
                &settings_text("STATIC_FOLDER_PATH", &config.static_folder_path, ""),
            ),
        ],
    );
    format!(
        r#"<form class="settings" id="runtime-settings"><header class="settings-header"><div><h2>Runtime configuration</h2><p>Changes save when a field loses focus or a checkbox changes. Changing the listener address or port reconnects this page.</p></div><span class="settings-status" role="status" aria-live="polite">All changes saved</span></header>{general}{data}{server}{advanced}<p class="settings-empty" hidden>No settings match your search.</p></form>"#
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
