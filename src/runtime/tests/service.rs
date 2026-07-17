#[test]
fn non_focused_instance_generation_invalidates_manager_revision() {
    let mut revisions = SemanticRevision::default();
    let initial = ManagerSemantics {
        focused_pid: Some(7),
        game_focused: true,
        enable_key_overlay: true,
        instances: BTreeMap::from([(7, 10), (9, 20)]),
    };
    assert_eq!(revisions.observe(initial.clone()), 1);
    assert_eq!(revisions.observe(initial.clone()), 1);

    let mut non_focused_changed = initial.clone();
    non_focused_changed.instances.insert(9, 21);
    assert_eq!(revisions.observe(non_focused_changed), 2);

    let mut settings_changed = initial;
    settings_changed.enable_key_overlay = false;
    assert_eq!(revisions.observe(settings_changed), 3);
}
#[test]
fn production_page_adapter_substitutes_template_and_renders_html_contracts() {
    let page = render_homepage(
        PageService.dashboard_template(),
        Some(2),
        None,
        true,
        r#"<div class="result-item">counter</div>"#,
        None,
    );
    let html = String::from_utf8(page.body.to_vec()).unwrap();
    for placeholder in [
        "{{LOCAL_AMOUNT}}",
        "{{AVAILABLE_AMOUNT}}",
        "{{SEARCH}}",
        "{{LIST}}",
    ] {
        assert!(!html.contains(placeholder));
    }
    assert!(html.contains("Installed (2)"));
    assert!(html.contains("search-bar"));
    assert!(html.contains(r#"class="result-item""#));

    let updated = render_homepage(
        PageService.dashboard_template(),
        Some(0),
        Some(0),
        true,
        "",
        Some("99.0.0"),
    );
    let updated = String::from_utf8(updated.body.to_vec()).unwrap();
    assert!(updated.contains("update-available"));
    assert!(!updated.contains("hidden update-available"));
    assert!(!updated.contains("{OLD}"));
    assert!(updated.contains("99.0.0"));

    let settings = render_runtime_settings(&Config::default());
    for binding in [
        "ENABLE_AUTOUPDATE",
        "OPEN_DASHBOARD_ON_STARTUP",
        "CALCULATE_PP",
        "READ_MANIA_SCROLL_SPEED",
        "ENABLE_KEY_OVERLAY",
        "POLL_RATE",
        "PRECISE_DATA_POLL_RATE",
        "V2_SUBSCRIPTION_DEBOUNCE",
        "SERVER_IP",
        "SERVER_PORT",
        "ALLOWED_IPS",
        "ENABLE_INGAME_OVERLAY",
        "INGAME_OVERLAY_KEYBIND",
        "INGAME_OVERLAY_MAX_FPS",
        "DEBUG_LOG",
        "SHOW_MP_COMMANDS",
        "STATIC_FOLDER_PATH",
    ] {
        assert!(
            settings.contains(&format!(r#"data-id="{binding}""#)),
            "{binding}"
        );
    }
    assert!(settings.contains("V2 Subscription Debounce"));
    assert!(settings.contains(
        "The minimum interval between selected-field WebSocket frames, in milliseconds."
    ));
    assert!(settings.contains(
        r#"data-id="V2_SUBSCRIPTION_DEBOUNCE"><button title="[Click] decrements by 1&#10;[Shift + Click] by 10" class="decr">-</button><input type="number" value="0" min="0" max="">"#
    ));
}

#[test]
fn settings_save_maps_fractional_v2_subscription_debounce() {
    let mut config = Config::default();
    apply_config_values(
        &mut config,
        serde_json::json!({"V2_SUBSCRIPTION_DEBOUNCE": 0.125})
            .as_object()
            .unwrap(),
    );

    assert_eq!(config.v2_subscription_debounce, 0.125);
}

#[test]
fn counter_cards_use_absolute_obs_url_and_source_resolution_rules() {
    let fixed = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        "folder",
        false,
        false,
        false,
        None,
        &[],
        false,
        false,
        &[],
        &[serde_json::json!(640), serde_json::json!(480)],
        "localhost:24050",
    );
    assert!(fixed.contains(r#"src="http://localhost:24050/folder/""#));
    assert!(fixed.contains(r#"width="640px" height="480px""#));
    assert!(fixed.contains(r#"nfv="640" class="copyable">640</span> x"#));
    assert!(fixed.contains(r#"nfv="480" class="copyable">480</span>"#));

    let any = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        "folder",
        false,
        false,
        false,
        None,
        &[],
        false,
        false,
        &[],
        &[serde_json::json!(-1), serde_json::json!(-2)],
        "127.0.0.1:24050",
    );
    assert!(any.contains(r#"width="500px" height="300px""#));
    assert_eq!(
        any.matches(r#"nfv="ANY" class="copyable">ANY</span>"#)
            .count(),
        2
    );

    let flexible = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        "folder",
        false,
        false,
        false,
        None,
        &[],
        false,
        false,
        &[],
        &[serde_json::json!(-2), serde_json::json!(-1)],
        "127.0.0.1:24050",
    );
    assert!(flexible.contains(r#"width="100%" height="500px""#));
}

#[test]
fn production_catalog_query_matches_source_token_rules() {
    assert_eq!(normalize_catalog_query("!HD SC"), "hd sc");
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

#[test]
fn report_adapter_reads_real_instance_client_version() {
    let mut settings = crate::model::states::SettingsObject::new();
    settings.insert("client".into(), serde_json::json!({"version":"2026.714.0"}));
    assert_eq!(report_instance_version(&settings), "2026.714.0");
    settings.insert("client".into(), serde_json::json!({"version":42}));
    assert_eq!(report_instance_version(&settings), "");
}

#[test]
fn production_adapter_keeps_current_files_relative_to_songs() {
    assert_eq!(
        current_file_relative(Some("123 Artist - Title"), Some("map.osu")),
        Some(PathBuf::from("123 Artist - Title").join("map.osu"))
    );
    assert_eq!(
        current_file_relative(Some("../outside"), Some("map.osu")),
        Some(PathBuf::from("../outside").join("map.osu")),
        "the provider must not turn a request-derived path into a trust root"
    );
}

#[test]
fn production_adapter_builds_only_trusted_stable_skin_roots() {
    assert_eq!(
        stable_skin_root("/games/osu", "Default"),
        Some(PathBuf::from("/games/osu/Skins/Default"))
    );
    assert_eq!(stable_skin_root("/games/osu", ""), None);
    assert_eq!(stable_skin_root("/games/osu", "../outside"), None);
    assert_eq!(stable_skin_root("/games/osu", "nested/skin"), None);
    let absolute = format!("{}outside", std::path::MAIN_SEPARATOR);
    assert_eq!(stable_skin_root("/games/osu", &absolute), None);
}
#[test]
fn fractional_server_port_is_rejected() {
    let config = Config {
        server_port: 24_050.5,
        ..Config::default()
    };
    let error = listen_target(&config).expect_err("fractional ports must not be truncated");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn authorization_policy_reloads_without_listener_rebuild() {
    let authorizer = ReloadableAuthorizer::new("127.0.0.1,localhost", "localhost");
    let peer = "127.0.0.1".parse().unwrap();
    assert!(
        authorizer
            .authorize(
                peer,
                RequestHeaders {
                    origin: Some("http://localhost:24050"),
                    referer: None,
                },
            )
            .allowed()
    );
    authorizer.reload("127.0.0.1,example.test", "example.test");
    assert!(
        !authorizer
            .authorize(
                peer,
                RequestHeaders {
                    origin: Some("http://localhost:24050"),
                    referer: None,
                },
            )
            .allowed()
    );
    assert!(
        authorizer
            .authorize(
                peer,
                RequestHeaders {
                    origin: Some("https://example.test"),
                    referer: None,
                },
            )
            .allowed()
    );
}

#[test]
fn startup_owner_conflict_remains_typed_for_exit_75_mapping() {
    let error = io::Error::new(io::ErrorKind::ResourceBusy, StartupOwnerConflict);
    assert!(is_startup_owner_conflict(&error));
    assert!(!is_startup_owner_conflict(&io::Error::other("ordinary")));
}

