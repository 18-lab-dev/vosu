use super::*;

#[tokio::test]
async fn context_requires_a_live_token_and_explicit_context_permission() {
    let root = tempfile::tempdir().unwrap();
    let reader = put(
        &root.path().join("dev"),
        "reader",
        "[permissions]\nhost=['context']",
    );
    let plain = put(&root.path().join("dev"), "plain", "");
    let host = bounded!(host(root.path(), vec![reader, plain]));
    for (tool, token) in [
        ("reader", "reader-context-token"),
        ("plain", "plain-context-token"),
    ] {
        host.cli_sessions.lock().insert(
            format!("{tool}-context"),
            CliSession {
                tool: tool.into(),
                token: token.into(),
                expires: Instant::now() + Duration::from_secs(60),
            },
        );
    }
    for (token, status) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("forged-context-token"), StatusCode::UNAUTHORIZED),
        (Some("plain-context-token"), StatusCode::FORBIDDEN),
        (Some("reader-context-token"), StatusCode::OK),
    ] {
        let mut req = request(Method::GET, "/tools/_host/v1/context");
        if let Some(token) = token {
            req.headers_mut()
                .insert("x-vosu-tool-token", token.parse().unwrap());
        }
        let response = bounded!(host.overlay(req, Bytes::new()));
        assert_eq!(response.status(), status);
        if status == StatusCode::OK {
            assert_eq!(
                bounded!(value(response)),
                json!({"client":null,"songs_dir":null,"beatmap":null})
            );
        }
    }
    bounded!(host.shutdown());
}
