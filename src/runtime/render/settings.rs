// Included by render.rs rather than declared as a module, so the settings
// markup shares render.rs's private helpers without widening their visibility.

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
    let externally_managed = vosu_core::desktop::externally_managed();
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
            interval(
                "API_V1_SUBSCRIPTION_DEBOUNCE",
                "API v1 subscription debounce",
                "Wait at least this long between API v1 subscription updates, independently of selected V2 fields.",
                config.api_v1_subscription_debounce,
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
            r#"<div class="settings-item settings-item--static"><span class="settings-item__copy"><span class="settings-item__name">Source</span></span><span class="settings-item__control settings-item__value"><a href="https://github.com/18-lab-dev/vosu" target="_blank" rel="noopener noreferrer">github.com/18-lab-dev/vosu</a></span></div>"#,
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
