//! The Settings form shell: controls, labels and bounds only. The form starts
//! `inert`; the client fills every value from core's `GET /api/settings`
//! (`SettingsView`) and saves through `/api/settingsSave`. This crate never
//! reads configuration.

/// Upper bound of every interval setting (core's `ConfigDocument::MAX_INTERVAL_MS`).
const MAX_INTERVAL_MS: u32 = 60_000;

/// Boolean settings are switches; the row's description is linked rather
/// than wrapped into the label, so the accessible name stays the title.
fn switch_control(id: &str) -> String {
    format!(
        r#"<input class="settings-control v-toggle" id="setting-{id}" data-id="{id}" type="checkbox" role="switch" aria-describedby="setting-{id}-description">"#
    )
}

fn text_control(id: &str) -> String {
    format!(
        r#"<input class="settings-control settings-control--mono" id="setting-{id}" data-id="{id}" type="text" value="" spellcheck="false" aria-describedby="setting-{id}-description">"#
    )
}

/// Numeric control whose bounds mirror the server-side save validation, so the
/// browser rejects out-of-range values before they are sent. A unit sits
/// inside the field's right edge, so every field in a column keeps one width.
fn number_control(id: &str, min: u32, max: u32, step: &str, unit: Option<&str>) -> String {
    let input = format!(
        r#"<input class="settings-control" id="setting-{id}" data-id="{id}" type="number" value="" min="{min}" max="{max}" step="{step}" required aria-describedby="setting-{id}-description">"#
    );
    match unit {
        Some(unit) => format!(
            r#"<span class="settings-unit-field">{input}<span class="settings-unit" aria-hidden="true">{unit}</span></span>"#
        ),
        None => input,
    }
}

/// `id` is the control's `data-id`: the title labels `setting-{id}` and the
/// description is its `aria-describedby` target.
fn item(id: &str, name: &str, description: &str, control: &str) -> String {
    format!(
        r#"<div class="settings-item"><span class="settings-item__copy"><label class="settings-item__name" for="setting-{id}">{}</label><span class="settings-item__description" id="setting-{id}-description">{}</span></span><span class="settings-item__control">{control}</span></div>"#,
        crate::page::escape_html(name),
        crate::page::escape_html(description),
    )
}

fn switch(id: &str, name: &str, description: &str) -> String {
    item(id, name, description, &switch_control(id))
}

fn interval(id: &str, name: &str, description: &str, min: u32) -> String {
    item(
        id,
        name,
        description,
        &number_control(id, min, MAX_INTERVAL_MS, "any", Some("ms")),
    )
}

fn group(header: &str, items: &[String]) -> String {
    format!(
        r#"<fieldset class="settings-group"><legend>{header}</legend>{}</fieldset>"#,
        items.join("\n"),
    )
}

/// `version` is already escaped.
pub(crate) fn form(version: &str) -> String {
    let general = group(
        "General",
        &[
            switch(
                "ENABLE_AUTOUPDATE",
                "Automatic updates",
                "Install new vosu releases automatically during startup. vosu checks for them at every start either way, so it can show the update notice.",
            ),
            switch(
                "OPEN_DASHBOARD_ON_STARTUP",
                "Open dashboard at startup",
                "Open this local dashboard in the default browser after vosu starts.",
            ),
            #[cfg(target_os = "linux")]
            switch(
                "START_AT_LOGIN",
                "Start at login",
                "Run vosu as a systemd user service when you log in.",
            ),
        ],
    );
    let data = group(
        "Game data",
        &[
            switch(
                "CALCULATE_PP",
                "Calculate performance points",
                "Compute live performance points. Disable this when overlays do not need PP data.",
            ),
            switch(
                "READ_MANIA_SCROLL_SPEED",
                "Read mania scroll speed",
                "Expose the current mania scroll speed from game memory.",
            ),
            interval(
                "POLL_RATE",
                "Common poll interval",
                "Milliseconds between reads of general game data.",
                100,
            ),
            interval(
                "PRECISE_DATA_POLL_RATE",
                "Precise poll interval",
                "Milliseconds between reads of precise key and hit-error data.",
                1,
            ),
            interval(
                "API_V1_SUBSCRIPTION_DEBOUNCE",
                "API v1 subscription debounce",
                "Wait at least this long between API v1 subscription updates.",
                1,
            ),
        ],
    );
    let server = group(
        "Server",
        &[
            item(
                "SERVER_IP",
                "Listener address",
                "IP address where the vosu HTTP server listens.",
                &text_control("SERVER_IP"),
            ),
            item(
                "SERVER_PORT",
                "Listener port",
                "Port for the vosu HTTP server, from 1 through 65535. Ports below 1024 usually require administrator rights.",
                &number_control("SERVER_PORT", 1, 65_535, "1", None),
            ),
            item(
                "DASHBOARD_IP",
                "Dashboard address",
                "Trusted dashboard listener. Loopback-only by default, independently of the overlay listener.",
                &text_control("DASHBOARD_IP"),
            ),
            item(
                "DASHBOARD_PORT",
                "Dashboard port",
                "Separate port for trusted management pages. Overlays keep their original port and browser storage.",
                &number_control("DASHBOARD_PORT", 1, 65_535, "1", None),
            ),
            item(
                "ALLOWED_IPS",
                "Allowed IP addresses",
                "One IP address or CIDR range per line. Prefer CIDR, such as 100.64.0.0/10 for Tailscale, over legacy wildcard octets that may include public addresses (100.*.*.* is broader than Tailscale). The literal legacy hostname/raw-header token absolute comes from tosu defaults; it is not a wildcard or a peer-policy bypass.",
                r#"<textarea class="settings-control allowlist-editor" id="setting-ALLOWED_IPS" data-id="ALLOWED_IPS" wrap="off" rows="5" spellcheck="false" aria-describedby="setting-ALLOWED_IPS-description allowlist-warning"></textarea><span id="allowlist-warning" class="allowlist-warning" role="status"></span>"#,
            ),
        ],
    );
    let advanced = group(
        "Advanced",
        &[
            item(
                "MAX_PACKAGE_MIB",
                "Maximum package size",
                "Compressed and expanded catalog package limit. Raising it permits larger downloads and archive contents.",
                &number_control("MAX_PACKAGE_MIB", 1, 4096, "1", Some("MiB")),
            ),
            switch(
                "DEBUG_LOG",
                "Debug logging",
                "Write verbose diagnostics. Enable this only while troubleshooting.",
            ),
            switch(
                "SHOW_MP_COMMANDS",
                "Show multiplayer commands",
                "Expose Bancho multiplayer commands in tournament chat data.",
            ),
            item(
                "STATIC_FOLDER_PATH",
                "Overlays directory",
                "Directory vosu scans for local overlays. Changes need Apply.",
                &text_control("STATIC_FOLDER_PATH"),
            ),
        ],
    );
    // The affiliation notice and version live here instead of a page footer.
    let about = format!(
        concat!(
            r#"<fieldset class="settings-group settings-group--about"><legend>About</legend>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Version</span></span><span class="settings-item__control settings-item__value">vosu {version}</span></div>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Source</span></span><span class="settings-item__control settings-item__value"><a href="https://github.com/18-lab-dev/vosu" target="_blank" rel="noopener noreferrer">github.com/18-lab-dev/vosu</a></span></div>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Affiliation</span></span><span class="settings-item__control settings-item__value">Not affiliated with ppy or the tosu project.</span></div>"#,
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Made by</span></span><span class="settings-item__control settings-item__value">18-LAB</span></div>"#,
            "</fieldset>",
        ),
        version = version,
    );
    format!(
        r#"<form class="settings" id="runtime-settings" aria-busy="true" inert>{general}{data}{server}{advanced}{about}<p class="settings-empty" hidden>No settings match your search.</p></form>"#
    )
}
