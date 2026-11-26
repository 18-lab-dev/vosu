//! The bundled `vosu.overlayhost` answering `overlays.query` over the real
//! ABI relay: core's grant gate, its schemas, and a live overlay root.
use super::{job_fixture as fixture, support::Harness};
use serde_json::{Value, json};
use std::sync::Arc;
use vosu_core::{
    identity::PluginInstanceId,
    plugin::{self, PluginHostHandler},
};
use vosu_overlay_host::ReloadableCounterService;
use vosu_plugin_api::{ActionCall, ActionResult, ActionStatus, CborValue, Message, vosu_status};

const ACTION: &str = "vosu.overlayhost.overlays.query";

fn call(h: &Harness, caller: u64, invocation: u64, args: Value) {
    assert_eq!(
        h.host.handle(
            PluginInstanceId::new(caller),
            Message::ActionCall(ActionCall {
                invocation,
                action: ACTION.into(),
                session: None,
                args: CborValue::serialized(&args).unwrap(),
            }),
            0,
        ),
        vosu_status::VOSU_OK
    );
}

fn result(events: &std::sync::mpsc::Receiver<Message>, invocation: u64) -> ActionResult {
    match fixture::next(
        events,
        |m| matches!(m, Message::ActionResult(r) if r.invocation == invocation),
    ) {
        Message::ActionResult(result) => result,
        _ => unreachable!(),
    }
}

#[test]
fn only_a_declaring_plugin_lists_installed_overlays() {
    let h = Harness::new();
    let overlays = h.root.path().join("static");
    std::fs::create_dir_all(overlays.join("hud")).unwrap();
    std::fs::write(overlays.join("hud/index.html"), "hud").unwrap();
    let counters = Arc::new(ReloadableCounterService::production(
        overlays.clone(),
        h.root.path().join("config/settings"),
        h.root.path().join("cache"),
    ));
    let bundled = vosu::plugins::BUNDLED
        .iter()
        .find(|entry| entry.manifest == vosu_overlay_host::MANIFEST)
        .unwrap();
    let loaded = plugin::load_bundled(
        &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
        &h.host.validation_snapshot(),
        std::slice::from_ref(bundled),
    )
    .unwrap();
    h.host
        .create_instance(
            PluginInstanceId::new(783401),
            &loaded[0],
            h.root.path().into(),
            None,
        )
        .unwrap();
    let _binding = vosu_overlay_host::bind_overlay_query(h.root.path(), &counters);
    let (granted, _) = fixture::load(
        &h,
        783402,
        "fixture.overlayquery.granted",
        &format!("\n[[optional]]\nid='action:{ACTION}'\nversion='^1.0'\n"),
    );
    let (undeclared, _) = fixture::load(&h, 783403, "fixture.overlayquery.undeclared", "");

    call(&h, 783403, 1, json!({}));
    let refused = result(&undeclared, 1);
    assert_eq!(refused.status, ActionStatus::Error);
    assert_eq!(refused.code.as_deref(), Some("permission_denied"));
    assert_eq!(refused.value, None);

    call(&h, 783402, 2, json!({"limit": 51}));
    assert_eq!(
        result(&granted, 2).code.as_deref(),
        Some("invalid_arguments")
    );

    call(&h, 783402, 3, json!({"limit": 1}));
    let listed = result(&granted, 3);
    assert_eq!(listed.status, ActionStatus::Ok, "{listed:?}");
    let page: Value = listed.value.unwrap().deserialized().unwrap();
    assert_eq!(
        page,
        json!({
            "overlays": [{"id": "hud", "name": "hud", "entry": "/hud/", "version": null, "settings": {}}],
            "next": null,
        })
    );
    assert!(
        !serde_json::to_string(&page)
            .unwrap()
            .contains(&*h.root.path().to_string_lossy())
    );

    std::fs::remove_dir_all(overlays.join("hud")).unwrap();
    call(&h, 783402, 4, json!({}));
    let page: Value = result(&granted, 4).value.unwrap().deserialized().unwrap();
    assert_eq!(page, json!({"overlays": [], "next": null}));

    h.host.action_instance_lost(PluginInstanceId::new(783402));
    h.host.action_instance_lost(PluginInstanceId::new(783403));
}
