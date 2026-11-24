use super::*;
use crate::plugin::host::tests::{test_host, test_process};
use crate::plugin::host_services::PluginHostHandler;
use crate::plugin::registry::{BundledPlugin, load_bundled};
use vosu_plugin_testkit::session::{self, OnAttach, SessionEvent, SessionPluginConfig};

async fn perform(
    host: &PluginHost,
    instance: PluginInstanceId,
    message: Message,
    deadline: Duration,
) -> Result<ResourceReply, ResourceRequestError> {
    let Message::ResourceResolve(request) = message else {
        panic!("expected resolve request");
    };
    let owner = host.resource_owner(instance, request.handle)?;
    host.resource_requests(deadline)
        .request(owner, ResourceRequest::Resolve(request))
        .await
}

fn attach(host: &Arc<PluginHost>, instance: PluginInstanceId, plugin: &str, pid: u32) -> u64 {
    let executable = format!("resource-{pid}.exe");
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin}'\nname='roots'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nroot_slots=['songs']\n[match]\nprocess_names=['{executable}']\n[[provides]]\nid='game:fixture'\nversion='*'\n"
    );
    let events = session::register_with_config(
        plugin,
        SessionPluginConfig {
            on_attach: OnAttach::Ignore,
            ..Default::default()
        },
    );
    let loaded = load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: session::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    host.create_instance(instance, &loaded[0], dir.path().to_owned(), None)
        .unwrap();
    host.process_appeared(test_process(pid, &executable));
    match events.recv_timeout(Duration::from_secs(3)).unwrap() {
        SessionEvent::Attached(a) => a.handle,
        other => panic!("expected attachment, got {other:?}"),
    }
}

fn declare(
    host: &PluginHost,
    instance: PluginInstanceId,
    handle: Option<u64>,
    name: &str,
    path: &Path,
) -> vosu_status {
    host.handle(
        instance,
        Message::RootDeclare(RootDeclare {
            handle,
            name: name.into(),
            path: path.to_str().unwrap().into(),
        }),
        0,
    )
}

#[test]
fn resource_requests_roots_are_scoped_retained_and_revoked() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let first = PluginInstanceId(8201);
    let second = PluginInstanceId(8202);
    let handle = attach(&host, first, "testkit.resources.roots.first", 98201);
    let other = attach(&host, second, "testkit.resources.roots.second", 98202);
    let wide = tempfile::tempdir().unwrap();
    let scoped = tempfile::tempdir().unwrap();
    std::fs::write(wide.path().join("chart.osu"), b"wide-content").unwrap();
    std::fs::write(scoped.path().join("chart.osu"), b"session-specific").unwrap();
    assert_eq!(
        declare(&host, first, None, "undeclared", wide.path()),
        vosu_status::VOSU_ERR_PERMISSION_DENIED
    );
    assert_eq!(
        declare(&host, first, Some(other), "songs", wide.path()),
        vosu_status::VOSU_ERR_PERMISSION_DENIED
    );
    assert_eq!(
        declare(&host, first, None, "songs", Path::new("relative")),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        declare(&host, first, None, "songs", wide.path()),
        vosu_status::VOSU_OK
    );
    let old = host.resource_root(first, handle, "songs").unwrap();
    let stale_owner = host.resource_owner(first, handle).unwrap();
    let relative = SafeRelativePath::parse("chart.osu").unwrap();
    assert_eq!(old.open_child(&relative).unwrap().metadata.len(), 12);
    assert!(host.resource_root(second, other, "songs").is_err());
    assert!(host.resource_root(first, other, "songs").is_err());
    assert_eq!(
        declare(&host, first, Some(handle), "songs", scoped.path()),
        vosu_status::VOSU_OK
    );
    assert!(!old.is_current());
    assert!(old.open_child(&relative).is_err());
    let current = host.resource_root(first, handle, "songs").unwrap();
    assert_eq!(current.open_child(&relative).unwrap().metadata.len(), 16);
    assert_eq!(
        runtime.block_on(host.resource_requests(Duration::from_secs(2)).request(
            stale_owner,
            ResourceRequest::Read(vosu_plugin_api::StreamRead {
                stream: 1,
                offset: 0,
                len: 1
            })
        )),
        Err(ResourceRequestError::Cancelled)
    );
    host.process_appeared(test_process(98207, "resource-98201.exe"));
    let same_plugin_handle = host
        .sessions
        .lock()
        .unwrap()
        .handle_for_pid(98207)
        .unwrap()
        .0;
    let same_plugin = host
        .resource_root(first, same_plugin_handle, "songs")
        .unwrap();
    assert_eq!(
        same_plugin.open_child(&relative).unwrap().metadata.len(),
        12
    );
    host.process_exited(98201);
    assert!(!current.is_current());
    assert!(
        same_plugin.is_current(),
        "detaching one session preserves another session's root"
    );
    assert!(host.resource_root(first, handle, "songs").is_err());
    assert_eq!(
        declare(&host, first, Some(handle), "songs", scoped.path()),
        vosu_status::VOSU_ERR_TARGET_GONE
    );
    host.destroy_all(Duration::from_secs(2));
    assert!(
        !same_plugin.is_current(),
        "instance shutdown revokes plugin-wide roots"
    );
    assert!(host.resource_state.lock().unwrap().owners.is_empty());
}

#[test]
fn resource_requests_root_update_and_detach_cancel_pending() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let instance = PluginInstanceId(8203);
    let handle = attach(&host, instance, "testkit.resources.roots.cancel", 98203);
    let dir = tempfile::tempdir().unwrap();
    let message = Message::ResourceResolve(vosu_plugin_api::ResourceResolve {
        handle,
        kind: "file".into(),
        key: vosu_plugin_api::CborValue::Integer(17.into()),
    });
    let (id, mut receiver) = host
        .resource_state
        .lock()
        .unwrap()
        .requests
        .reserve(instance, 0, &message)
        .unwrap();
    assert_eq!(
        declare(&host, instance, None, "songs", dir.path()),
        vosu_status::VOSU_OK
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    assert_eq!(
        host.handle(
            instance,
            Message::ResourceResolved(vosu_plugin_api::ResourceResolved::Stream {
                stream: 23,
                size: 31
            }),
            id
        ),
        vosu_status::VOSU_ERR_CANCELLED
    );
    let (_, mut receiver) = host
        .resource_state
        .lock()
        .unwrap()
        .requests
        .reserve(instance, 1, &message)
        .unwrap();
    let wide = host.resource_root(instance, handle, "songs").unwrap();
    host.process_exited(98203);
    assert!(
        !wide.is_current(),
        "plugin-wide roots borrowed by a detached session are revoked"
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    host.destroy_all(Duration::from_secs(2));
}

#[test]
fn resource_requests_dropped_future_releases_pending_slot() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let instance = PluginInstanceId(8204);
    let handle = attach(&host, instance, "testkit.resources.roots.drop", 98204);
    runtime.block_on(async {
        let message = Message::ResourceResolve(vosu_plugin_api::ResourceResolve {
            handle,
            kind: "file".into(),
            key: vosu_plugin_api::CborValue::Null,
        });
        let future = perform(&host, instance, message, Duration::from_millis(100));
        tokio::pin!(future);
        // Poll through registration and dispatch once, without sleeping for ordering.
        std::future::poll_fn(|cx| {
            let _ = Future::poll(future.as_mut(), cx);
            std::task::Poll::Ready(())
        })
        .await;
    });
    assert_eq!(host.resource_state.lock().unwrap().requests.len(), 0);
    host.destroy_all(Duration::from_secs(2));
}

#[test]
fn resource_requests_deadline_releases_pending_slot() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let instance = PluginInstanceId(8205);
    let handle = attach(&host, instance, "testkit.resources.roots.timeout", 98205);
    let message = Message::ResourceResolve(vosu_plugin_api::ResourceResolve {
        handle,
        kind: "file".into(),
        key: vosu_plugin_api::CborValue::Null,
    });
    let result = runtime.block_on(tokio::time::timeout(
        Duration::from_secs(3),
        perform(&host, instance, message, Duration::from_millis(10)),
    ));
    assert_eq!(result.unwrap(), Err(ResourceRequestError::TimedOut));
    assert_eq!(host.resource_state.lock().unwrap().requests.len(), 0);
    host.destroy_all(Duration::from_secs(2));
}

mod reentrant {
    use vosu_plugin_api::{
        HostServices, Init, Message, Plugin, PluginError, ResourceResolved, RootDeclare,
        VtableHostServices, vosu_status,
    };
    pub struct Reentrant(VtableHostServices);
    impl Plugin for Reentrant {
        fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let status = host.send(
                &Message::RootDeclare(RootDeclare {
                    handle: None,
                    name: "songs".into(),
                    path: init.data_dir.to_str().unwrap().into(),
                }),
                0,
            );
            if status != vosu_status::VOSU_OK {
                return Err(status.into());
            }
            Ok(Self(host))
        }
        fn on_message(&mut self, message: Message, id: u64) -> Result<(), PluginError> {
            if let Message::ResourceResolve(request) = message {
                if request.kind == "reject" {
                    return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
                }
                let reply = if request.kind == "wrong-shape" {
                    ResourceResolved::Tree {
                        entries: vec![],
                        truncated: false,
                        revision: "index-73".into(),
                    }
                } else {
                    ResourceResolved::Path {
                        root: "songs".into(),
                        path: "reentrant.osu".into(),
                    }
                };
                let status = self.0.send(&Message::ResourceResolved(reply), id);
                if status != vosu_status::VOSU_OK {
                    return Err(status.into());
                }
            }
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled Reentrant);
}

#[test]
fn resource_requests_real_abi_reentrancy_and_dispatch_failure() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let manifest = "kind='plugin'\nmanifest_version=1\nid='testkit.resources.reentrant'\nname='reentrant'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nroot_slots=['songs']\n[match]\nprocess_names=['reentrant-98206.exe']\n[[provides]]\nid='game:fixture'\nversion='*'\n";
    let loaded = load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: reentrant::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let data = tempfile::tempdir().unwrap();
    let instance = PluginInstanceId(8206);
    host.create_instance(instance, &loaded[0], data.path().to_owned(), None)
        .unwrap();
    host.process_appeared(test_process(98206, "reentrant-98206.exe"));
    let handle = host
        .sessions
        .lock()
        .unwrap()
        .handle_for_pid(98206)
        .unwrap()
        .0;
    // Root declaration re-entered the host during plugin create.
    assert!(
        host.resource_root(instance, handle, "songs")
            .unwrap()
            .is_current()
    );
    let request = |kind: &str| {
        Message::ResourceResolve(vosu_plugin_api::ResourceResolve {
            handle,
            kind: kind.into(),
            key: vosu_plugin_api::CborValue::Integer(59.into()),
        })
    };
    let reply = runtime
        .block_on(perform(
            &host,
            instance,
            request("file"),
            Duration::from_secs(2),
        ))
        .unwrap();
    assert_eq!(
        reply,
        ResourceReply::Resolved(vosu_plugin_api::ResourceResolved::Path {
            root: "songs".into(),
            path: "reentrant.osu".into()
        })
    );
    assert_eq!(
        runtime.block_on(perform(
            &host,
            instance,
            request("reject"),
            Duration::from_secs(2)
        )),
        Err(ResourceRequestError::DispatchFailed)
    );
    assert_eq!(
        runtime.block_on(perform(
            &host,
            instance,
            request("wrong-shape"),
            Duration::from_secs(2)
        )),
        Err(ResourceRequestError::InvalidReply)
    );
    assert_eq!(host.resource_state.lock().unwrap().requests.len(), 0);
    host.destroy_all(Duration::from_secs(2));
}
