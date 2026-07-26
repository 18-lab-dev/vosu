use super::*;

pub(super) fn format_number_px(value: f64) -> String {
    format!("{}px", format_number(value))
}

pub(super) fn local_overlay(counter: &crate::counters::Counter) -> LocalOverlay {
    LocalOverlay {
        name: counter.name.clone(),
        author: counter.author.clone(),
        version: counter.version.clone(),
    }
}

pub(super) fn render_homepage(
    template: EmbeddedResponse,
    local_count: Option<usize>,
    available_count: Option<usize>,
    search: bool,
    list: &str,
    update_version: Option<&str>,
) -> EmbeddedResponse {
    let search = if search {
        r#"<form class="search-form" role="search"><label for="counter-search">Search overlays</label><div class="search-control"><input id="counter-search" class="search-bar" type="search" autocomplete="off" placeholder="Name, author, or compatible client" aria-controls="counter-results"><kbd aria-hidden="true">/</kbd></div></form>"#
    } else {
        ""
    };
    let body = String::from_utf8_lossy(&template.body)
        .replace(
            "{{LOCAL_AMOUNT}}",
            &local_count.map_or_else(String::new, |count| format!("({count})")),
        )
        .replace(
            "{{AVAILABLE_AMOUNT}}",
            &available_count.map_or_else(String::new, |count| format!("({count})")),
        )
        .replace("{{SEARCH}}", search)
        .replace("{{LIST}}", list)
        .replace(
            "{{UPDATE_HIDDEN}}",
            if update_version.is_some() {
                ""
            } else {
                "hidden"
            },
        )
        .replace("{{CURRENT_VERSION}}", &current_version().to_string())
        .replace("{{NEW_VERSION}}", update_version.unwrap_or_default());
    EmbeddedResponse {
        body: Bytes::from(body),
        ..template
    }
}

pub(super) fn normalize_catalog_query(query: &str) -> String {
    let mut value = query.to_owned();
    if let Some((index, character)) = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_alphanumeric())
    {
        value.replace_range(index..index + character.len_utf8(), "");
    }
    value.to_ascii_lowercase()
}

pub(super) fn query_tokens(query: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    for token in query.split(' ') {
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
    let name = name.to_ascii_lowercase();
    let author = author.to_ascii_lowercase();
    query_tokens(query).into_iter().any(|token| {
        name.contains(token)
            || author.contains(token)
            || (token != "vosu"
                && compatible
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains(token)))
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
            let assets = counter
                .assets
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|asset| asset.get("url").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>();
            render_counter_card(
                &counter.name,
                &counter.author,
                &counter.version,
                &counter.folder_name,
                false,
                counter.downloaded,
                counter.updatable,
                counter.download_link.as_deref(),
                &counter.authorlinks,
                !counter.settings.is_empty(),
                false,
                &assets,
                &counter.resolution,
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
                .map(|asset| asset.url.as_str())
                .collect::<Vec<_>>();
            let version = counter
                .version
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| counter.version.to_string());
            render_counter_card(
                &counter.name,
                &counter.author,
                &version,
                counter.folder_name.as_deref().unwrap_or(&counter.name),
                true,
                counter.downloaded,
                counter.updatable,
                counter.download_link.as_deref(),
                &counter.authorlinks,
                counter
                    .settings
                    .as_ref()
                    .is_some_and(|value| !value.is_empty()),
                counter.has_settings,
                &assets,
                &counter.resolution,
                authority,
            )
        })
        .collect()
}

fn parse_absolute_http_url(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

// The renderer intentionally accepts the catalog fields directly to avoid an
// allocation-only intermediate model at this internal rendering boundary.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_counter_card(
    name: &str,
    author: &str,
    version: &str,
    folder: &str,
    external: bool,
    downloaded: bool,
    updatable: bool,
    download_link: Option<&str>,
    author_links: &[String],
    settings: bool,
    external_settings: bool,
    assets: &[&str],
    resolution: &[serde_json::Value],
    authority: &str,
) -> String {
    let status = if updatable {
        " is-updatable"
    } else if downloaded {
        " is-installed"
    } else {
        ""
    };
    let settings_badge = if external && external_settings {
        r#"<span class="badge badge-cyan">Settings available</span>"#
    } else {
        ""
    };
    let author_links = author_links
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
        .collect::<String>();
    let author_links = if author_links.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="author-links">{author_links}</span>"#)
    };
    let overlay_identity = if external {
        String::new()
    } else {
        format!(r#" data-overlay-id="{}""#, escape_html(folder))
    };
    let install_name = format!("{name} by {author}");
    let actions = if external {
        let update = if updatable && download_link.is_some() {
            format!(
                r#"<button class="button button-cyan" type="button" data-action="update-counter" data-download-url="{}" data-install-name="{}">Update</button>"#,
                escape_html(download_link.unwrap_or_default()),
                escape_html(&install_name),
            )
        } else {
            String::new()
        };
        let primary = if !downloaded && download_link.is_some() {
            format!(
                r#"<button class="button button-primary" type="button" data-action="download-counter" data-download-url="{}" data-install-name="{}">Install</button>"#,
                escape_html(download_link.unwrap_or_default()),
                escape_html(&install_name),
            )
        } else if downloaded {
            format!(
                r#"<button class="button" type="button" data-action="open-counter" data-folder="{}">Open folder</button>"#,
                escape_html(&install_name),
            )
        } else {
            r#"<button class="button" type="button" disabled>Unavailable</button>"#.to_owned()
        };
        format!(r#"<div class="counter-actions">{update}{primary}</div>"#)
    } else {
        let settings_action = if settings {
            format!(
                r#"<button class="button" type="button" data-action="counter-settings" data-folder="{}">Settings</button>"#,
                escape_html(folder),
            )
        } else {
            String::new()
        };
        format!(
            r#"<div class="counter-actions"><button class="button" type="button" data-action="toggle-pin" aria-pressed="false">Pin</button><button class="pin-order" type="button" data-action="pin-earlier" aria-label="Move pinned overlay earlier" title="Move earlier">↑</button><button class="pin-order" type="button" data-action="pin-later" aria-label="Move pinned overlay later" title="Move later">↓</button><button class="button builder-action" type="button" data-action="settings-builder" data-folder="{}">Edit schema</button>{settings_action}<button class="button" type="button" data-action="open-counter" data-folder="{}">Open folder</button><button class="button button-danger" type="button" data-action="delete-counter" data-folder="{}">Delete</button></div>"#,
            escape_html(folder),
            escape_html(folder),
            escape_html(folder),
        )
    };

    let x = resolution.first().and_then(json_number).unwrap_or(-1.0);
    let y = resolution.get(1).and_then(json_number).unwrap_or(-1.0);
    let width = if x == -1.0 {
        "500px".to_owned()
    } else if x == -2.0 || x <= 10.0 {
        "100%".to_owned()
    } else {
        format_number_px(x)
    };
    let height = if y == -1.0 {
        "500px".to_owned()
    } else if x <= 10.0 {
        "300px".to_owned()
    } else {
        format_number_px(y)
    };
    let x_text = if x == -1.0 || x == -2.0 {
        "ANY".to_owned()
    } else {
        format_number(x)
    };
    let y_text = if y == -1.0 || y == -2.0 {
        "ANY".to_owned()
    } else {
        format_number(y)
    };
    let absolute_url = format!("http://{authority}/{folder}/");
    let preview = if external {
        let links = assets
            .iter()
            .filter_map(|url| parse_absolute_http_url(url))
            .enumerate()
            .map(|(index, url)| {
                format!(
                    r#"<li><a href="{}" target="_blank" rel="noopener noreferrer">Preview {}</a></li>"#,
                    escape_html(url.as_str()),
                    index + 1,
                )
            })
            .collect::<String>();
        if links.is_empty() {
            r#"<div class="preview-placeholder"><strong>Catalog overlay</strong><span>Install this overlay to preview it from your local Vosu server.</span></div>"#.to_owned()
        } else {
            format!(
                r#"<div class="preview-links"><strong>Catalog preview links</strong><span>Media opens only when selected.</span><ul>{links}</ul></div>"#
            )
        }
    } else {
        format!(
            r#"<iframe src="{}" loading="lazy" width="{width}" height="{height}" title="{} overlay preview"></iframe>"#,
            escape_html(&absolute_url),
            escape_html(name),
        )
    };
    let metadata = if external {
        String::new()
    } else {
        let resolution_text = format!("{x_text} × {y_text}");
        format!(
            r#"<footer class="counter-card__metadata"><button class="metadata-copy" type="button" data-action="copy-metadata" data-copy-label="URL" data-copy-value="{}"><span>URL</span><code>/{}/</code></button><button class="metadata-copy" type="button" data-action="copy-metadata" data-copy-label="Resolution" data-copy-value="{}"><span>Resolution</span><code>{}</code></button></footer>"#,
            escape_html(&absolute_url),
            escape_html(folder),
            escape_html(&resolution_text),
            escape_html(&resolution_text),
        )
    };
    format!(
        r#"<article class="counter-card{status}"{overlay_identity} data-counter-name="{}" data-counter-author="{}" data-counter-version="{}"><header class="counter-card__header"><div class="counter-card__identity"><div class="counter-card__title-row"><h2>{}</h2><span class="counter-card__version">v{}</span>{settings_badge}</div><p class="counter-card__author"><span>by {}</span>{author_links}</p></div>{actions}</header><div class="counter-card__preview">{preview}</div>{metadata}</article>"#,
        escape_html(name),
        escape_html(author),
        escape_html(version),
        escape_html(name),
        escape_html(version),
        escape_html(author),
    )
}

pub(super) fn empty_counter_search() -> String {
    r#"<div class="empty-state"><h2>No matching overlays</h2><p>Try a shorter name, a different author, or another compatible client.</p></div>"#.to_owned()
}

pub(super) fn local_overlay_instructions() -> String {
    r#"<section class="local-guide" aria-labelledby="local-guide-title"><header><h2 id="local-guide-title">Serve an overlay from this computer</h2><p>Vosu discovers local overlays beneath its configured overlays directory.</p></header><ol><li><h3>Open the overlays directory</h3><p>Create one folder for the overlay you want to serve.</p><button class="button button-primary" type="button" data-action="open-root">Open overlays folder</button></li><li><h3>Add the overlay files</h3><p>Place the overlay entry point and its supporting files in that folder. The usual entry point is <code>index.html</code>.</p></li><li><h3>Describe the overlay</h3><p>Add <code>metadata.txt</code> to provide its display name, author, version, links, and preferred resolution. The first-party quickstart repository includes an example.</p><a class="button" href="https://github.com/shiruviaa/vosu-overlay-quickstart" target="_blank" rel="noopener noreferrer">Open overlay quickstart</a></li><li><h3>Return to Installed</h3><p>Reload the Installed page. Vosu will discover the folder and expose its local URL, settings, and management actions.</p><a class="button" href="/">View installed overlays</a></li></ol></section>"#.to_owned()
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

pub(super) fn settings_number(id: &str, value: f64, min: u32) -> String {
    format!(
        r#"<input class="settings-control" id="setting-{}" data-id="{}" type="number" value="{value}" min="{min}" step="any">"#,
        escape_html(id),
        escape_html(id),
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
    let general = settings_group(
        "General",
        &[
            settings_item(
                "Automatic updates",
                "Check for a new Vosu release during startup and install it automatically.",
                &settings_switch("ENABLE_AUTOUPDATE", config.enable_auto_update),
            ),
            settings_item(
                "Open dashboard at startup",
                "Open this local dashboard in the default browser after Vosu starts.",
                &settings_switch(
                    "OPEN_DASHBOARD_ON_STARTUP",
                    config.open_dashboard_on_startup,
                ),
            ),
        ],
    );
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
                &settings_number("POLL_RATE", config.poll_rate, 100),
            ),
            settings_item(
                "Precise poll interval",
                "Milliseconds between reads of precise key and hit-error data.",
                &settings_number("PRECISE_DATA_POLL_RATE", config.precise_data_poll_rate, 0),
            ),
            settings_item(
                "V2 subscription debounce",
                "Minimum milliseconds between selected-field WebSocket frames.",
                &settings_number(
                    "V2_SUBSCRIPTION_DEBOUNCE",
                    config.v2_subscription_debounce,
                    0,
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
                "Port for the Vosu HTTP server. Use a value from 1024 through 65535.",
                &settings_text("SERVER_PORT", &config.server_port.to_string(), ""),
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
        r#"<form class="settings" id="runtime-settings"><header class="settings-header"><h2>Runtime configuration</h2><p>Review your changes, then save them together. Changing the listener address or port reconnects this page.</p></header><div class="settings-grid">{general}{data}{server}{advanced}</div><div class="settings-actions"><span class="settings-status" role="status">No unsaved changes</span><button class="button button-primary" type="submit">Save settings</button></div></form>"#
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
