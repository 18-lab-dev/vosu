use super::*;

pub(super) async fn assert_native_broker(
    client: &reqwest::Client,
    base: &str,
    port: u16,
    roots: &TempDir,
    channel: vosu_sys::platform::BuildChannel,
    values_toml_path: &Path,
    expected_settings: &dyn Fn(&str) -> serde_json::Value,
) {
    // Native brokers authenticate explicitly, without forging browser provenance.
    let broker_path = channel
        .root(roots.path().join("runtime/vosu"))
        .join("broker.json");
    let credential: serde_json::Value =
        serde_json::from_slice(&fs::read(&broker_path).unwrap()).unwrap();
    assert_eq!(credential["version"], 1);
    assert_eq!(credential["origin"], base);
    let token = credential["token"].as_str().unwrap();
    let mut wrong = token.as_bytes().to_vec();
    wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
    let wrong = String::from_utf8(wrong).unwrap();
    for (authorization, status) in [
        (format!("Authorization: Bearer {wrong}\r\n"), 401),
        (
            format!("Authorization: Bearer {token}\r\nAuthorization: Bearer {token}\r\n"),
            401,
        ),
        (format!("Authorization: bearer {token}\r\n"), 101),
        ("Authorization: Basic cached\r\n".to_owned(), 101),
    ] {
        let _ = bounded!(websocket_connect_with_authorization(
            port,
            "/websocket/commands?l=seeded-counter",
            None,
            &authorization,
            status,
        ));
    }
    let _ = bounded!(websocket_connect_with_bearer(
        port,
        "/websocket/v2",
        None,
        Some(&wrong),
        101
    ));
    let uncredentialed = bounded!(client.get(format!("{base}/websocket/commands")).send())
        .unwrap()
        .status();
    for credential in [token, &wrong] {
        let response = bounded!(
            client
                .get(format!("{base}/websocket/commands"))
                .bearer_auth(credential)
                .send()
        )
        .unwrap();
        assert_eq!(response.status(), uncredentialed);
        assert_ne!(response.status(), 401);
    }
    let saved = bounded!(
        client
            .post(format!("{base}/api/counters/settings/seeded-counter"))
            .bearer_auth(token)
            .json(&serde_json::json!([{"uniqueID":"title","value":"native HTTP"}]))
            .send()
    )
    .unwrap();
    assert_eq!(saved.status(), 200);
    assert_eq!(
        bounded!(saved.json::<serde_json::Value>()).unwrap(),
        serde_json::json!({"result":"success"})
    );
    let _ = bounded!(websocket_connect_with_bearer(
        port,
        "/websocket/commands?l=seeded-counter",
        Some(base),
        Some("stale"),
        401,
    ));
    let (mut observer, mut observer_buffer) = bounded!(websocket_connect(
        port,
        "/websocket/commands?l=seeded-counter",
        None
    ));
    bounded!(observer.write_all(&masked_frame(1, b"getSettings:seeded-counter:"))).unwrap();
    let _ = bounded!(read_server_frame(&mut observer, &mut observer_buffer));
    let (mut broker, _) = bounded!(websocket_connect_with_bearer(
        port,
        "/websocket/commands?l=seeded-counter",
        None,
        Some(token),
        101,
    ));
    bounded!(broker.write_all(&masked_frame(
        1,
        br#"saveSettings:seeded-counter:[{"uniqueID":"title","value":"broker write"}]"#,
    )))
    .unwrap();
    let (opcode, payload) = bounded!(read_server_frame(&mut observer, &mut observer_buffer));
    assert_eq!(opcode, 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&payload).unwrap()["message"][0]["value"],
        "broker write"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let persisted = bounded!(
            client
                .get(format!("{base}/api/counters/settings/seeded-counter"))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
        )
        .unwrap();
        if persisted == expected_settings("broker write") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "broker did not persist settings: {persisted}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        fs::read_to_string(values_toml_path)
            .unwrap()
            .contains("broker write")
    );
    drop(broker);
    drop(observer);
    // The stock hosted settings protocol needs no token or new message shape.
    let (mut hosted, _) = bounded!(websocket_connect(
        port,
        "/websocket/commands?l=seeded-counter",
        Some(base)
    ));
    bounded!(hosted.write_all(&masked_frame(
        1,
        br#"saveSettings:seeded-counter:[{"uniqueID":"title","value":"hosted write"}]"#,
    )))
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let values = bounded!(
            client
                .get(format!("{base}/api/counters/settings/seeded-counter"))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
        )
        .unwrap();
        if values == expected_settings("hosted write") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "same-origin socket did not persist settings: {values}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(hosted);
}
