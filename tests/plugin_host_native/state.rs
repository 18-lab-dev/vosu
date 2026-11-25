//! M5 I0: a real `vosu plugin-host` child's state through core's existing
//! services (`docs/v2/plugin-abi.md` §6.2, §6.6, §6.7, §6.9, §6.12, §7.5;
//! `docs/v2/state-schema.md` privacy and lanes): publication committed
//! atomically and projected through the production API v1 router, demand
//! from API and plugin subscribers, settings, session lifecycle, a
//! cross-plugin `STATE_SUBSCRIBE` between two children, and core's
//! `internal` projection and surface refusal for an external process
//! reader.

use std::time::Duration;

use serde_json::{Value, json};
use vosu_core::plugin::{self, PluginInstanceId};
use vosu_plugin_api::{
    HostServices as _, PublishBatchWriter, PublishValueRef, Selector, vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::publisher::{self, PublisherEvent};
use vosu_surface::state::{SessionTarget, SurfaceError};

use super::support::api::{Api, fence, frame, select};
use super::support::services::{BOUND, Services, Spec, op};

const INVALID: u32 = vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32;

/// Every `demand` receipt of instance `index` so far.
fn demands(run: &Services, index: usize) -> Vec<(String, String, String)> {
    run.plugin(index)
        .fixture
        .receipts("demand")
        .iter()
        .map(|receipt| {
            (
                receipt.get("regular").to_owned(),
                receipt.get("precise").to_owned(),
                receipt.get("selectors").to_owned(),
            )
        })
        .collect()
}

/// Publication from a real child: the attach batch, an API WebSocket
/// subscription's demand (one-shot reads create none), a committed batch
/// seen by HTTP and the subscriber, a batch core refuses that commits
/// nothing (no frame, HTTP unchanged), demand released on close, and the
/// session gone after detach.
#[test]
fn state_process_publication_commits_atomically_and_projects_through_the_api() {
    let run = Services::new(&[Spec {
        name: "state.publish",
        ..Spec::default()
    }]);
    let ns = run.plugin(0).plugin_id.clone();
    let target = 0x7E10_0001;
    let attach = run.attach(0, target);
    assert_eq!(attach.pid, run.child(0), "the child published");
    assert_ne!(attach.pid, std::process::id(), "the plugin ran in core");
    let session = attach.get("session").to_owned();
    let api = Api::new(run.host(), Duration::from_secs(5));
    let path = format!("/sessions/{session}/state/{ns}");
    let document = api.json(&path);
    assert_eq!(document["marker"], "attached", "{document}");
    assert_eq!(document["count"], 0, "{document}");
    assert_eq!(
        api.json(&format!("/sessions/{session}"))["plugin"]["id"],
        ns
    );
    assert!(
        demands(&run, 0)
            .iter()
            .all(|(regular, precise, selectors)| regular == "false"
                && precise == "false"
                && selectors == "-"),
        "one-shot reads created demand: {:?}",
        demands(&run, 0)
    );

    let marker = format!("/{ns}/marker");
    let count = format!("/{ns}/count");
    let mut socket = api.runtime.block_on(async {
        let mut socket = api.ws(&format!("/sessions/{session}/subscribe")).await;
        let initial = select(&mut socket, &[&marker, &count]).await;
        assert_eq!(initial[0], 0, "{initial}");
        assert_eq!(initial[2], json!([[0, "attached"], [1, 0]]), "{initial}");
        socket
    });
    let demanded = run.wait(0, "demand", |receipt| receipt.get("selectors") != "-");
    assert_eq!(
        (demanded.get("regular"), demanded.get("precise")),
        ("true", "true"),
        "the marker's regular lane and the count's precise lane: {demanded:?}"
    );
    let selectors: Vec<&str> = demanded.get("selectors").split(',').collect();
    assert!(
        selectors.contains(&format!("{ns}|/marker").as_str())
            && selectors.contains(&format!("{ns}|/count").as_str()),
        "{demanded:?}"
    );

    assert_eq!(
        run.drive(0, op("publish", json!({"text": "second", "count": 2})))
            .status(),
        0
    );
    let update = api.runtime.block_on(frame(&mut socket));
    assert_eq!(update[0], 1, "{update}");
    assert_eq!(update[2], json!([[0, "second"], [1, 2]]), "{update}");
    let document = api.json(&path);
    assert_eq!(
        (&document["marker"], &document["count"]),
        (&json!("second"), &json!(2))
    );

    assert_eq!(
        run.drive(0, op("publish_invalid", json!({"text": "third"})))
            .status(),
        INVALID,
        "core refused the batch descending through a string"
    );
    api.runtime.block_on(fence(&mut socket));
    let document = api.json(&path);
    assert_eq!(
        (&document["marker"], &document["count"]),
        (&json!("second"), &json!(2)),
        "a refused batch committed nothing: {document}"
    );

    api.runtime.block_on(async {
        socket.close(None).await.unwrap();
    });
    drop(socket);
    let released = run.wait(0, "demand", |receipt| {
        receipt.get("selectors") == "-" && receipt.get("regular") == "false"
    });
    assert_eq!(released.get("precise"), "false");

    run.host.process_exited(target);
    let detach = run.wait(0, "detach", |_| true);
    assert_eq!(detach.get("handle"), attach.get("handle"));
    assert_eq!(detach.get("reason"), "Exited");
    let (status, body) = api.get(&path);
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("unknown_session"), "{body}");
}

/// A second child subscribes to the first child's session through
/// `vosu.sessions` and `STATE_SUBSCRIBE`: lifecycle, the initial value
/// and a later commit reach it, its subscription is demand on the owner,
/// settings arrive as the complete effective object, and detach reaches
/// both the owner and the observer.
#[test]
fn state_process_subscriber_follows_lifecycle_settings_and_another_childs_commits() {
    let owner = "test.i0.state.owner";
    let run = Services::new(&[
        Spec {
            name: "state.owner",
            ..Spec::default()
        },
        Spec {
            name: "state.reader",
            observer: true,
            requires: vec![format!("[[requires]]\nid = 'field:{owner}'\nversion = '*'")],
            ..Spec::default()
        },
    ]);
    assert_ne!(run.child(0), run.child(1), "one child per instance");
    let init = run.plugin(1).fixture.wait("init", BOUND).unwrap();
    assert_eq!(
        init.get("settings"),
        "{LABEL:initial-label,RATE:150}",
        "init carries the effective defaults"
    );
    assert_eq!(run.drive(1, op("sessions", json!({}))).status(), 0);

    let target = 0x7E10_0002;
    let attach = run.attach(0, target);
    let observed = run.wait(1, "attach", |receipt| {
        receipt.get("session") == attach.get("session")
    });
    assert_eq!(observed.get("observed"), "1");
    let handle: u64 = observed.get("handle").parse().unwrap();
    assert_eq!(
        run.drive(
            1,
            op(
                "subscribe",
                json!({"session": handle, "selectors": [format!("{owner}|/marker")]})
            )
        )
        .status(),
        0
    );
    let initial = run.wait(1, "update", |receipt| {
        receipt.get("entries") == format!("{owner}|/marker|attached")
    });
    assert_eq!(initial.get("session"), handle.to_string());
    let demanded = run.wait(0, "demand", |receipt| {
        receipt
            .get("selectors")
            .split(',')
            .any(|selector| selector == format!("{owner}|/marker"))
    });
    assert_eq!(
        demanded.get("regular"),
        "true",
        "a plugin subscriber is demand"
    );

    assert_eq!(
        run.drive(0, op("publish", json!({"text": "from-owner", "count": 1})))
            .status(),
        0
    );
    run.wait(1, "update", |receipt| {
        receipt.get("entries") == format!("{owner}|/marker|from-owner")
    });

    run.apply_settings(1, json!({"LABEL": "changed label"}));
    let settings = run.wait(1, "settings", |_| true);
    assert_eq!(
        settings.get("values"),
        "{LABEL:changed%20label,RATE:150}",
        "the complete effective object, not a diff"
    );
    assert!(
        run.plugin(0).fixture.receipts("settings").is_empty(),
        "settings reached another plugin"
    );

    run.host.process_exited(target);
    assert_eq!(
        run.wait(0, "detach", |_| true).get("handle"),
        attach.get("handle")
    );
    assert_eq!(
        run.wait(1, "detach", |_| true).get("handle"),
        handle.to_string(),
        "the observer saw the session end"
    );
}

/// Loads the in-process `vosu.osu` publisher (the one namespace with an
/// `x-visibility` subtree) as instance 100.
fn osu_publisher(run: &Services) -> std::sync::mpsc::Receiver<PublisherEvent> {
    let id = "vosu.osu";
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='0.1.0'\n\
         host='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n\
         [[provides]]\nid='field:{id}'\nversion='1.3.0'\nschema='state.json'\n\
         [[provides]]\nid='game:fixture'\nversion='*'\n[match]\nprocess_names=['{id}']\n"
    );
    publisher::register_manifest(id, &manifest);
    let events = publisher::register(id);
    let loaded = plugin::load_bundled(
        vosu::application_version(),
        &run.host().validation_snapshot(),
        &[plugin::BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            namespace_schemas: &[include_str!("../../docs/v2/schemas/vosu.osu.schema.json")],
            action_schemas: &[],
            vtable: publisher::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    {
        let _enter = run.host.enter();
        run.host()
            .create_instance(
                PluginInstanceId::new(100),
                &loaded[0],
                run.root.path().join("osu-data"),
                None,
            )
            .unwrap();
    }
    events
}

/// Offers `target` to the publisher and returns its session handle.
fn attach_osu(
    run: &Services,
    events: &std::sync::mpsc::Receiver<PublisherEvent>,
    target: u32,
) -> u64 {
    run.host.offer_target(target, "vosu.osu", vec![0; 8]);
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("the publisher attached");
        if let PublisherEvent::Attached { handle } = event {
            return handle;
        }
    }
}

fn publish_osu(handle: u64, fields: &[(&str, PublishValueRef<'_>)]) {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    for (pointer, value) in fields {
        writer.push("vosu.osu", pointer, *value);
    }
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    assert_eq!(
        publisher::host_services("vosu.osu").send_flat(
            vosu_opcode::VOSU_OP_PUBLISH_BATCH,
            0,
            0,
            &bytes
        ),
        vosu_status::VOSU_OK
    );
}

/// Core's privacy projection for an external process reader: the child
/// granted `vosu.osu.internal.paths.stable` receives that subtree (the
/// authorized positive control) and never its `lazer` sibling; the API's
/// HTTP and WebSocket projections never carry either sentinel; and the
/// external plugin gets no bundled surface authority, which the bundled
/// publisher has.
#[test]
fn state_process_reader_gets_only_its_internal_grant_and_no_surface_authority() {
    const STABLE: &str = "/private/I0-stable-install-5113";
    const LAZER: &str = "/private/I0-lazer-install-5114";
    let mut run = Services::empty();
    let events = osu_publisher(&run);
    run.add(&Spec {
        name: "state.private",
        observer: true,
        requires: vec![
            "[[requires]]\nid = 'field:vosu.osu.internal.paths.stable'\nversion = '*'".to_owned(),
        ],
        ..Spec::default()
    });
    assert_eq!(run.drive(0, op("sessions", json!({}))).status(), 0);
    let target = 0x7E10_0003;
    let publisher = attach_osu(&run, &events, target);
    publish_osu(
        publisher,
        &[
            (
                "/internal/paths/stable/install",
                PublishValueRef::String(STABLE),
            ),
            (
                "/internal/paths/lazer/install",
                PublishValueRef::String(LAZER),
            ),
            ("/public", PublishValueRef::String("public-5115")),
        ],
    );
    let observed = run.wait(0, "attach", |_| true);
    let session = observed.get("session").to_owned();
    let handle: u64 = observed.get("handle").parse().unwrap();
    assert_eq!(
        run.drive(
            0,
            op(
                "subscribe",
                json!({"session": handle, "selectors": ["vosu.osu|/internal"]})
            )
        )
        .status(),
        0
    );
    let granted = run.wait(0, "update", |receipt| {
        receipt.get("entries").starts_with("vosu.osu|/internal|")
    });
    assert_eq!(
        granted.get("entries"),
        format!("vosu.osu|/internal|{{paths:{{stable:{{install:{STABLE}}}}}}}"),
        "exactly the granted subtree, never its sibling"
    );
    assert!(!granted.get("entries").contains(LAZER));

    let api = Api::new(run.host(), Duration::from_secs(5));
    for path in [
        format!("/sessions/{session}/state"),
        format!("/sessions/{session}/state/vosu.osu"),
        format!("/sessions/{session}/state?pointer=%2Fvosu.osu%2Finternal"),
        format!(
            "/sessions/{session}/state/vosu.osu?pointer=%2Finternal%2Fpaths%2Fstable%2Finstall"
        ),
    ] {
        let (status, body) = api.get(&path);
        assert_eq!(status, 200, "{path}: {body}");
        assert!(
            !body.contains(STABLE) && !body.contains(LAZER),
            "{path} leaked an internal path: {body}"
        );
    }
    assert!(
        api.get(&format!("/sessions/{session}/state/vosu.osu"))
            .1
            .contains("public-5115")
    );
    api.runtime.block_on(async {
        let mut socket = api.ws(&format!("/sessions/{session}/subscribe")).await;
        let initial = select(&mut socket, &["/vosu.osu"]).await;
        let text = initial.to_string();
        assert!(text.contains("public-5115") && !text.contains(STABLE) && !text.contains(LAZER));
    });

    let reader = &run.plugin(0).plugin_id;
    let surface = run.host().surface_host();
    let selector = Selector {
        ns: "vosu.osu".into(),
        pointer: "/public".into(),
    };
    assert_eq!(
        surface
            .read(reader, SessionTarget::Id(session.clone()), &selector)
            .unwrap_err(),
        SurfaceError::UnknownReader
    );
    assert_eq!(
        surface
            .subscribe(
                reader,
                SessionTarget::Id(session.clone()),
                vec![selector.clone()]
            )
            .err(),
        Some(SurfaceError::UnknownReader)
    );
    assert_eq!(
        run.host().surface_requester(run.instance(0)).err(),
        Some(SurfaceError::UnknownReader),
        "an external process plugin has no bundled surface requester"
    );
    let bundled: Value = serde_json::to_value(
        surface
            .read("vosu.osu", SessionTarget::Id(session), &selector)
            .expect("the bundled publisher's own surface read")
            .value
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        bundled,
        json!("public-5115"),
        "the bundled positive control"
    );
    assert!(
        run.host()
            .surface_requester(PluginInstanceId::new(100))
            .is_ok()
    );
}
