#![expect(
    clippy::disallowed_methods,
    reason = "Disposable resource fixtures exercise in-place disk mutation"
)]

use super::support::*;
use axum::http::{StatusCode, header};
use serde_json::Value;
use vosu_core::api::{ApiPlan, SessionTarget};
use vosu_plugin_testkit::fixture::{ApiFixturePlugin, STREAM_BYTES};

const FILE: &[u8] = include_bytes!("../fixtures/api-v1-plugin/fixture.osu");
fn vtable() -> vosu_plugin_api::vosu_plugin_vtable {
    vosu_plugin_internal::vtable_for::<ApiFixturePlugin>()
}
fn setup(id: &str, pid: u32) -> Harness {
    let mut h = Harness::new();
    std::fs::write(h.root.path().join("fixture.osu"), FILE).unwrap();
    let manifest = format!(
        "{}\nroot_slots=['fixture']\n[match]\nprocess_names=['{id}']\n[[provides]]\nid='field:{id}'\nversion='1.0.0'\nschema='namespace.json'\n[[provides]]\nid='resource:{id}.fixture-file'\nversion='1.0.0'\n[[provides]]\nid='resource:{id}.fixture-stream'\nversion='1.0.0'\n[[provides]]\nid='resource:{id}.fixture-tree'\nversion='1.0.0'\n[[resource_kinds]]\nkind='fixture-file'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-stream'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-tree'\ntree=true\nfamilies=['chart-text']\n[[provides]]\nid='game:fixture'\nversion='*'\n",
        manifest(id)
    );
    let schema =
        format!(r#"{{"title":"{id}","version":"1.0.0","type":"object","properties":{{}}}}"#);
    h.load(&manifest, &[&schema], &[], vtable);
    h.host.process_appeared(record(pid, id));
    h
}
async fn urls(h: &Harness, id: &str) -> Value {
    bounded(async {
        loop {
            let plan = ApiPlan::new(&[format!("/{id}")]).unwrap();
            if let Ok(update) = h
                .host
                .api_state_source()
                .read(&SessionTarget::Active, &plan)
                && let Some(Some(value)) = update.values.first()
            {
                let mut bytes = Vec::new();
                value.write_json(&mut bytes);
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                if value["resources"]["tree"]["url"].is_string() {
                    return value;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}
fn endpoint(server: &Server, url: &str) -> String {
    format!("{}{}", server.base.trim_end_matches("/api/v1"), url)
}
fn secured(response: &reqwest::Response) {
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        "sandbox"
    );
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
}

#[test]
fn resources_file_stream_ranges_head_and_validators() {
    let h = setup("test.r4.ranges", 974013);
    h.runtime.block_on(bounded(async {
        let state = urls(&h, "test.r4.ranges").await;
        let server = Server::new(&h, false).await;
        for (kind, bytes) in [("file", FILE), ("stream", STREAM_BYTES)] {
            let url = endpoint(&server, state["resources"][kind]["url"].as_str().unwrap());
            let response = server.client.get(&url).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            secured(&response);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/plain; charset=utf-8"
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=31536000, immutable"
            );
            let etag = response.headers()[header::ETAG]
                .to_str()
                .unwrap()
                .to_owned();
            assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
            for (range, expected) in [
                ("bytes=2-7", &bytes[2..8]),
                ("bytes=-5", &bytes[bytes.len() - 5..]),
                ("bytes=3-", &bytes[3..]),
            ] {
                let response = server
                    .client
                    .get(&url)
                    .header(header::RANGE, range)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                secured(&response);
                assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
            }
            for range in [
                "bytes=0-1,3-4",
                "bytes=-0",
                "bytes=18446744073709551616-",
                "bytes=999999-",
                "bytes=+1-2",
            ] {
                let response = server
                    .client
                    .get(&url)
                    .header(header::RANGE, range)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "{range}"
                );
                secured(&response);
                assert_eq!(
                    response.headers()[header::CONTENT_RANGE],
                    format!("bytes */{}", bytes.len())
                );
                assert_eq!(
                    response.json::<Value>().await.unwrap()["error"]["code"],
                    "invalid_range"
                );
            }
            let response = server
                .client
                .head(&url)
                .header(header::RANGE, "malformed")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            secured(&response);
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                bytes.len().to_string()
            );
            assert!(response.bytes().await.unwrap().is_empty());
            for condition in ["*".to_owned(), format!("\"other\", W/{etag}")] {
                let response = server
                    .client
                    .get(&url)
                    .header(header::IF_NONE_MATCH, condition)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
                secured(&response);
                assert!(response.bytes().await.unwrap().is_empty());
            }
            let response = server
                .client
                .get(&url)
                .header(header::IF_NONE_MATCH, "\"different\"")
                .header(header::IF_MODIFIED_SINCE, "Wed, 31 Dec 2098 23:59:59 GMT")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        server.shutdown().await;
    }));
}

#[test]
fn resources_tree_members_errors_and_disk_mutation() {
    let h = setup("test.r4.trees", 974014);
    h.runtime.block_on(bounded(async {
        let state = urls(&h, "test.r4.trees").await;
        let server = Server::new(&h, false).await;
        let tree = endpoint(&server, state["resources"]["tree"]["url"].as_str().unwrap());
        let response = server.client.get(&tree).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        secured(&response);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        let index: Value = response.json().await.unwrap();
        assert_eq!(index["entries"][0]["path"], "fixture.osu");
        let response = server.client.head(&tree).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        secured(&response);
        for path in ["fixture.osu", "alias.osu"] {
            let response = server
                .client
                .get(format!("{tree}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            secured(&response);
            assert_eq!(response.bytes().await.unwrap().as_ref(), FILE);
        }
        let response = server
            .client
            .get(format!("{tree}absent.png"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        secured(&response);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "unknown_resource"
        );
        let file = endpoint(&server, state["resources"]["file"]["url"].as_str().unwrap());
        assert_eq!(
            server.client.get(&file).send().await.unwrap().status(),
            StatusCode::OK
        );
        std::fs::write(
            h.root.path().join("fixture.osu"),
            b"changed-content-and-size",
        )
        .unwrap();
        let response = server.client.get(&file).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        secured(&response);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "unpublished_resource"
        );
        let response = server
            .client
            .get(format!("{}/resources/invalid-token", server.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        secured(&response);
        h.host.process_exited(974014);
        let response = server.client.get(&tree).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        secured(&response);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "unpublished_tree"
        );
        server.shutdown().await;
    }));
}
