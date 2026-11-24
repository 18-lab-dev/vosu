//! Explicit one-shot barriers, never sleeps, control detach during provider work.
use super::*;
use vosu_plugin_api::{CborBytes, ResourceResolved, StreamChunk, VOSU_STREAM_CHUNK_MAX_BYTES};

async fn held(o: &Arc<Mutex<Observed>>) {
    bounded(async {
        loop {
            if o.lock().unwrap().held.is_some() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[test]
fn resources_tree_detach_during_list_and_lookup_returns_410_and_rejects_late_reply() {
    for (id, pid, lookup) in [
        ("test.r5.detach.list", 975301, false),
        ("test.r5.detach.lookup", 975302, true),
    ] {
        let (h, o) = setup(id, pid, None);
        h.runtime.block_on(bounded(async {
            let state = state(&h, id, SessionTarget::Active).await;
            let server = Server::new(&h, false).await;
            let tree = endpoint(&server, &state["resources"]["tree"]);
            if lookup {
                let index: Value = server
                    .client
                    .get(&tree)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                assert_eq!(index["entries"][0]["path"], "fixture.osu");
                o.lock().unwrap().hold_lookup = true;
            } else {
                o.lock().unwrap().hold_list = true;
            }
            let url = if lookup {
                format!("{tree}alias.osu")
            } else {
                tree.clone()
            };
            let request = tokio::spawn({
                let client = server.client.clone();
                async move { client.get(url).send().await.unwrap() }
            });
            held(&o).await;
            h.host.process_exited(pid);
            let response = bounded(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::GONE);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"]["code"],
                "unpublished_tree"
            );
            let (host, correlation) = {
                let mut o = o.lock().unwrap();
                (o.host.unwrap(), o.held.take().unwrap().1)
            };
            let reply = if lookup {
                ResourceResolved::TreePath {
                    root: "fixture".into(),
                    path: "fixture.osu".into(),
                    entry_path: "fixture.osu".into(),
                }
            } else {
                ResourceResolved::Tree {
                    entries: vec![],
                    truncated: false,
                    revision: "late-975301".into(),
                }
            };
            assert_ne!(
                host.send(&Message::ResourceResolved(reply), correlation),
                vosu_status::VOSU_OK
            );
            assert_eq!(o.lock().unwrap().lists, 1 + usize::from(lookup));
            assert_eq!(o.lock().unwrap().lookups, usize::from(lookup));
            assert_eq!(o.lock().unwrap().resolve, 0);
            assert_eq!(o.lock().unwrap().read, 0);
            assert_eq!(o.lock().unwrap().close, 0);
            assert_eq!(
                server.client.get(&tree).send().await.unwrap().status(),
                StatusCode::GONE
            );
            server.shutdown().await;
        }));
    }
}

#[test]
fn resources_midstream_detach_truncates_body_and_closes_exactly_once() {
    let id = "test.r5.detach.stream";
    let (h, o) = setup(id, 975303, None);
    h.runtime.block_on(bounded(async {
        let state = state(&h, id, SessionTarget::Active).await;
        {
            let mut o = o.lock().unwrap();
            o.large_stream = true;
            o.hold_second_read = true;
        }
        let server = Server::new(&h, false).await;
        let url = endpoint(&server, &state["resources"]["stream"]);
        let mut response = server.client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.content_length(),
            Some(u64::from(VOSU_STREAM_CHUNK_MAX_BYTES) * 3)
        );
        let mut first = Vec::new();
        while first.len() < VOSU_STREAM_CHUNK_MAX_BYTES as usize {
            first.extend_from_slice(&bounded(response.chunk()).await.unwrap().unwrap());
        }
        assert_eq!(first, vec![0x7d; VOSU_STREAM_CHUNK_MAX_BYTES as usize]);
        held(&o).await;
        h.host.process_exited(975303);
        assert!(
            bounded(response.chunk()).await.is_err(),
            "a detached body must not manufacture the remaining bytes"
        );
        count(&o, 1).await;
        let (host, correlation) = {
            let mut o = o.lock().unwrap();
            (o.host.unwrap(), o.held.take().unwrap().1)
        };
        assert_ne!(
            host.send(
                &Message::StreamChunk(StreamChunk::Data {
                    data: CborBytes(vec![0x42; 17])
                }),
                correlation
            ),
            vosu_status::VOSU_OK
        );
        assert_eq!(o.lock().unwrap().resolve, 1);
        assert_eq!(o.lock().unwrap().read, 2);
        assert_eq!(o.lock().unwrap().close, 1);
        assert_eq!(
            server.client.get(&url).send().await.unwrap().status(),
            StatusCode::GONE
        );
        server.shutdown().await;
    }));
}

#[test]
fn resources_current_provider_failure_stays_502_not_retired_410() {
    let id = "test.r5.current.failure";
    let (h, o) = setup(id, 975304, None);
    h.runtime.block_on(bounded(async {
        let state = state(&h, id, SessionTarget::Active).await;
        o.lock().unwrap().hold = true;
        let server = Server::new(&h, false).await;
        let url = endpoint(&server, &state["resources"]["file"]);
        let request = tokio::spawn({
            let client = server.client.clone();
            async move { client.get(url).send().await.unwrap() }
        });
        held(&o).await;
        let (host, correlation) = {
            let mut o = o.lock().unwrap();
            (o.host.unwrap(), o.held.take().unwrap().1)
        };
        assert_eq!(
            host.send(
                &Message::ResourceResolved(ResourceResolved::Error {
                    error: vosu_plugin_api::ErrorInfo {
                        code: "provider_io_failed".into(),
                        message: "private-provider-error-975304".into()
                    }
                }),
                correlation
            ),
            vosu_status::VOSU_OK
        );
        let response = bounded(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let error = response.text().await.unwrap();
        assert!(error.contains("provider_failed"));
        assert!(!error.contains("private-provider-error-975304"));
        let token = state["resources"]["file"]["url"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap();
        assert!(
            h.host
                .published_resource(token, vosu_core::resources::ReferenceClass::Resource)
                .is_ok()
        );
        assert_eq!(o.lock().unwrap().resolve, 1);
        assert_eq!(o.lock().unwrap().read, 0);
        assert_eq!(o.lock().unwrap().close, 0);
        server.shutdown().await;
    }));
}
