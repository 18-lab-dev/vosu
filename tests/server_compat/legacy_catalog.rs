// 1.x counter catalog decoding, destination policy, report escaping and ZIP
// containment. Included by `tests/server_compat.rs`, sharing its imports.

#[test]
fn catalog_zip_destination_and_report_escaping_reject_hostile_inputs() {
    let overlays = decode_response(
        200,
        br#"[{"name":"A","author":"B","version":"1","downloadUrl":"https://example.test/a.zip"}]"#,
    )
    .unwrap();
    assert_eq!(overlays.len(), 1);
    assert!(decode_response(503, b"[]").is_err());
    assert!(decode_response(200, b"not-json").is_err());

    for address in [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6("fc00::1".parse().unwrap()),
        IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()),
    ] {
        assert!(
            !is_global_unicast(address),
            "{address} must not be a public destination"
        );
    }
    assert!(is_global_unicast("8.8.8.8".parse().unwrap()));
    assert_eq!(
        escape_html("<script>&\"'"),
        "&lt;script&gt;&amp;&quot;&#39;"
    );

    let root = TempDir::new().unwrap();
    let archive = root.path().join("bad.zip");
    let file = std::fs::File::create(&archive).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    writer
        .start_file("../escape.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"escape").unwrap();
    writer.finish().unwrap();
    let destination = root.path().join("installed");
    assert!(
        extract_zip_atomic(
            &archive,
            &destination,
            ZipLimits::default(),
            &CancellationToken::new()
        )
        .is_err()
    );
    assert!(!root.path().join("escape.txt").exists());
    assert!(!destination.exists());
}
