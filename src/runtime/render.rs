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
    let body = String::from_utf8_lossy(&template.body)
        .replace(
            "{{LOCAL_AMOUNT}}",
            &local_count.map_or_else(String::new, |count| format!(" ({count})")),
        )
        .replace(
            "{{AVAILABLE_AMOUNT}}",
            &available_count.map_or_else(String::new, |count| format!(" ({count})")),
        )
        .replace(
            "{{SEARCH}}",
            if search {
                r#"<input class="search-bar" type="text" placeholder="Type counter name to search..." />"#
            } else {
                ""
            },
        )
        .replace("{{LIST}}", list);
    let body = if let Some(new_version) = update_version {
        body.replace("{OLD}", &current_version().to_string())
            .replace("{NEW}", new_version)
            .replace("hidden update-available", "update-available")
    } else {
        body
    };
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
        " updatable"
    } else if downloaded {
        " downloaded"
    } else {
        ""
    };
    let customisable = if external && external_settings {
        r#"<div class="external-exists flexer"><i class="icon-settings"></i> customisable</div>"#
    } else {
        ""
    };
    let links = author_links
        .iter()
        .filter_map(|link| {
            let icon = if link.contains("github.com") {
                "/assets/images/github-000000.svg"
            } else if link.contains("twitter.com") {
                "/assets/images/twitter-1DA1F2.svg"
            } else if link.contains("discord.gg") || link.contains("discord.com") {
                "/assets/images/discord-5865f2.svg"
            } else {
                return None;
            };
            Some(format!(
                r#"<a href="{}" target="_blank"><img loading="lazy" src="{icon}" /></a>"#,
                escape_html(link)
            ))
        })
        .collect::<String>();
    let overlay_identity = if external {
        String::new()
    } else {
        format!(r#" data-overlay-id="{}""#, escape_html(folder))
    };
    let buttons = if external {
        let update = if updatable {
            format!(
                r#"<button class="button update-button flexer" l="{}" n="{}" a="{}"><span>Update</span></button>"#,
                escape_html(download_link.unwrap_or_default()),
                escape_html(name),
                escape_html(author)
            )
        } else {
            String::new()
        };
        let main = if download_link.is_some() && !downloaded {
            format!(
                r#"<button class="button dl-button flexer" l="{}" n="{}" a="{}"><span>Download</span></button>"#,
                escape_html(download_link.unwrap_or_default()),
                escape_html(name),
                escape_html(author)
            )
        } else {
            format!(
                r#"<button class="button open-button flexer" n="{} by {}"><span>Open Folder</span></button>"#,
                escape_html(name),
                escape_html(author)
            )
        };
        format!(r#"<div class="buttons-group flexer indent-left">{update}{main}</div>"#)
    } else {
        format!(
            r#"<div class="calu buttons-group flexer indent-left" n="{}" a="{}" v="{}"><button class="button pin-button flexer" type="button" aria-pressed="false" title="Pin overlay" aria-label="Pin overlay"><span>Pin</span></button><button class="button settings-builder-button flexer" n="{}"><i class="icon-builder"></i></button>{}<button class="button open-button flexer" n="{}"><span>Open Folder</span></button><button class="button delete-button flexer" n="{}"><span>Delete</span></button></div>"#,
            escape_html(name),
            escape_html(author),
            escape_html(version),
            escape_html(folder),
            if settings {
                format!(
                    r#"<button class="button settings-button flexer" n="{}"><span>Settings</span></button>"#,
                    escape_html(folder)
                )
            } else {
                String::new()
            },
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
    let gallery = if assets.is_empty() {
        format!(
            r#"<iframe src="{}" loading="lazy" width="{width}" height="{height}" n="{}" scrolling="no" frameborder="0"></iframe>"#,
            escape_html(&absolute_url),
            escape_html(folder)
        )
    } else {
        assets
            .iter()
            .map(|url| format!(r#"<img loading="lazy" src="{}" />"#, escape_html(url)))
            .collect()
    };
    let footer = if external {
        String::new()
    } else {
        format!(
            r#"<div class="ri-footer flexer"><div>URL: <span nf nft="url" nfv="{}" class="copyable">/{}/</span></div><div>Resolution: <span nf nft="width" nfv="{}" class="copyable">{}</span> x <span nf nft="height" nfv="{}" class="copyable">{}</span></div></div>"#,
            escape_html(&absolute_url),
            escape_html(folder),
            escape_html(&x_text),
            escape_html(&x_text),
            escape_html(&y_text),
            escape_html(&y_text)
        )
    };
    format!(
        r#"<div class="result-item{status}"{overlay_identity}><div class="ri-head flexer"><div><h4 class="flexer">{} v{}{customisable}</h4><div class="ri-links flexer"><span>by {}</span>{links}</div></div>{buttons}</div><hr><div class="ri-gallery flexer">{gallery}</div>{footer}</div>"#,
        escape_html(name),
        escape_html(version),
        escape_html(author),
    )
}

pub(super) fn empty_counter_search() -> String {
    r#"<div class="no-results">No counters<br />Change your search phrase</div>"#.to_owned()
}

pub(super) fn local_overlay_instructions() -> String {
    r#"<div class="settings">
        <h3>How to Add Your Own Counter <a>Locally</a></h3>
        <p>
          1. <b>Create a new folder</b>:<br>- First, create a <a>new folder</a> inside your <a class="button open-button small" n="static.exe">static folder</a>.<br><br>
          2. <b>Move your pp counter files</b>:<br>- Next, move <a>your pp counter</a> files into the newly created folder.<br><br>
          3. <b>Download and place metadata file</b>:<br>- Download the <a
             href="https://raw.githubusercontent.com/vosuapp/counters/master/quickstart/metadata.txt"
             target="_blank">metadata.txt</a> file and place it in the counter folder.<br><br>
          4. <b>Fill out the metadata file</b>:<br>- Finally, <a>open</a> the metadata.txt file and <a>fill out</a> the necessary information.
        </p>
      </div>"#.to_owned()
}

pub(super) fn settings_switch(id: &str, checked: bool) -> String {
    format!(
        r#"<label class="switch" data-id="{id}"><input type="checkbox" class="switch-thumb" {}><span class="switch-track"></span></label>"#,
        if checked { "checked" } else { "" },
    )
}

pub(super) fn settings_text(id: &str, value: &str, class: &str) -> String {
    format!(
        r#"<div class="text-input{class}" data-id="{id}"><input type="text" value="{}"></div>"#,
        escape_html(value),
    )
}

pub(super) fn settings_number(id: &str, value: f64, min: u32) -> String {
    format!(
        r#"<div class="number-input" data-id="{id}"><button title="[Click] decrements by 1&#10;[Shift + Click] by 10" class="decr">-</button><input type="number" value="{value}" min="{min}" max=""><button title="[Click] increments by 1&#10;[Shift + Click] by 10" class="incr">+</button></div>"#,
    )
}

pub(super) fn settings_item(name: &str, description: &str, inputs: &str) -> String {
    format!(
        r#"<div class="settings-item-v2"><div>{inputs}<p>{name}</p></div><p>{description}</p></div>"#,
    )
}

pub(super) fn settings_group(header: &str, items: &[String]) -> String {
    format!(
        r#"<div class="settings-item-group"><p>{header}</p><div>{}</div></div>"#,
        items.join("\n"),
    )
}

pub(super) fn render_runtime_settings(config: &Config) -> String {
    let general = settings_group(
        "General",
        &[
            settings_item(
                "Auto Update Client",
                "Check for updates on startup and automatically install newer versions if available.",
                &settings_switch("ENABLE_AUTOUPDATE", config.enable_auto_update),
            ),
            settings_item(
                "Open Dashboard on Startup",
                "Automatically open the vosu dashboard in your default browser on startup.",
                &settings_switch(
                    "OPEN_DASHBOARD_ON_STARTUP",
                    config.open_dashboard_on_startup,
                ),
            ),
        ],
    );
    let data = settings_group(
        "Data Fetching",
        &[
            settings_item(
                "RT PP Calculation",
                "Allow real-time calculation of performance points.<br>Useful to turn off for tournaments or users who do not care about pp.",
                &settings_switch("CALCULATE_PP", config.calculate_pp),
            ),
            settings_item(
                "Mania Scroll Speed",
                "Allow reading scrollSpeed from game memory.",
                &settings_switch("READ_MANIA_SCROLL_SPEED", config.read_mania_scroll_speed),
            ),
            settings_item(
                "Key Overlay Data",
                "Allow retrieval of live keypress data for the [K1/K2/M1/M2] keys.",
                &settings_switch("ENABLE_KEY_OVERLAY", config.enable_key_overlay),
            ),
            settings_item(
                "Common Poll Rate",
                "Interval of time, in milliseconds, between polling of general game data.",
                &settings_number("POLL_RATE", config.poll_rate, 100),
            ),
            settings_item(
                "Precise Poll Rate",
                "Interval of time, in milliseconds, between polling of precise game data. (KeyOverlayData, HitErrorData)",
                &settings_number("PRECISE_DATA_POLL_RATE", config.precise_data_poll_rate, 0),
            ),
            settings_item(
                "V2 Subscription Debounce",
                "The minimum interval between selected-field WebSocket frames, in milliseconds.",
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
                "Server IP",
                "The server IP address vosu will listen on.",
                &settings_text("SERVER_IP", &config.server_ip, ""),
            ),
            settings_item(
                "Server Port",
                "The server port vosu will listen on.<br>Must be between 1024 and 65535.",
                &settings_text("SERVER_PORT", &config.server_port.to_string(), ""),
            ),
            settings_item(
                "Allowed IPs",
                "A comma-separated list of IP addresses that are allowed to connect to the server.<br>Server IP is automatically added to the list. | Supports wildcard support (eg. 192.168.*.*)",
                &format!(
                    r#"<div class="textarea-input" data-id="ALLOWED_IPS"><textarea>{}</textarea></div>"#,
                    escape_html(&config.allowed_ips)
                ),
            ),
        ],
    );
    let advanced = settings_group(
        "Advanced",
        &[
            settings_item(
                "Debugging Mode",
                "Allow logging of additional verbose data to the console.<br>May cause performance issues: Please do not enable unless troubleshooting.",
                &settings_switch("DEBUG_LOG", config.debug_log),
            ),
            settings_item(
                "Show Multiplayer Commands",
                "Show bancho multiplayer commands (!mp) in the tournament chat manager.",
                &settings_switch("SHOW_MP_COMMANDS", config.show_mp_commands),
            ),
            settings_item(
                "Counters Directory",
                "Path to the folder containing the vosu counters. (named 'static' by default)",
                &settings_text("STATIC_FOLDER_PATH", &config.static_folder_path, ""),
            ),
        ],
    );
    format!(r#"<div class="settings">{general}{data}{server}{advanced}</div>"#)
}

pub(super) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
