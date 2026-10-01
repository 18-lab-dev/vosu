use super::*;
use futures_util::StreamExt;
use tempfile::TempDir;

struct FixedProbe(MachineData);
impl ReportProbe for FixedProbe {
    fn probe(&self) -> Pin<Box<dyn Future<Output = MachineData> + Send + '_>> {
        Box::pin(async { self.0.clone() })
    }
}

async fn rendered(document: &ReportDocument) -> String {
    let mut stream = ReportBodyStream::new(document.report.clone());
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.unwrap());
    }
    String::from_utf8(body).unwrap()
}

fn malicious_machine() -> MachineData {
    MachineData {
        system_type: DeviceType::Laptop,
        os: "<linux&>".into(),
        arch: "x86\"64".into(),
        cpu: "cpu<script>".into(),
        adapters: vec![
            GraphicsAdapter {
                name: "same<&>".into(),
                device_type: GraphicsDeviceType::DiscreteGpu,
            },
            GraphicsAdapter {
                name: "same<&>".into(),
                device_type: GraphicsDeviceType::IntegratedGpu,
            },
            GraphicsAdapter {
                name: "software".into(),
                device_type: GraphicsDeviceType::Cpu,
            },
        ],
        fallback_gpu_names: vec!["fallback".into()],
    }
}

#[tokio::test]
async fn escapes_every_dynamic_field_and_keeps_exact_sections() {
    let input = ReportInput {
        config: vec![("<KEY>".into(), "\"value&".into())],
        instances: vec![ReportInstance {
            pid: 9,
            client_type: "<lazer>".into(),
            bitness: "x64&".into(),
            version: "'v'".into(),
        }],
        counters: vec![ReportCounter {
            author: "<author>".into(),
            name: "name&".into(),
            version: "\"1\"".into(),
            folder_name: "folder'".into(),
        }],
        logs_dir: None,
    };
    let now = UNIX_EPOCH + Duration::from_millis(42);
    let report = collect_report_at(input.clone(), &FixedProbe(malicious_machine()), now)
        .await
        .unwrap();
    assert_eq!(report.date, DateTime::<Utc>::from(now));
    assert_eq!(report.spec.system_type, DeviceType::Laptop);
    assert_eq!(report.spec.os, "<linux&>");
    assert_eq!(report.spec.arch, "x86\"64");
    assert_eq!(report.spec.cpu, "cpu<script>");
    assert_eq!(report.spec.gpus, vec!["same<&>"]);
    assert_eq!(report.instances[0].pid, 9);
    assert_eq!(report.counters[0].folder_name, "folder'");
    let document = generate_report_document_at(input, &FixedProbe(malicious_machine()), now)
        .await
        .unwrap();
    let html = rendered(&document).await;
    for escaped in [
        "&lt;linux&amp;&gt;-x86&quot;64",
        "cpu&lt;script&gt;",
        "same&lt;&amp;&gt;",
        "&lt;KEY&gt;",
        "&quot;value&amp;",
        "&lt;lazer&gt;",
        "x64&amp;",
        "&#39;v&#39;",
        "&lt;author&gt;",
        "name&amp;",
        "&quot;1&quot;",
        "folder&#39;",
    ] {
        assert!(html.contains(escaped), "missing {escaped}");
    }
    assert!(!html.contains("software"));
    assert_eq!(html.matches("same&lt;&amp;&gt;").count(), 1);
    assert!(html.contains("System Configuration"));
    assert!(html.contains("vosu Settings"));
    assert!(html.contains("osu! Instances"));
    assert!(html.contains("Installed Overlays"));
    assert!(html.contains("1970-01-01T00:00:00.042Z"));
    assert!(html.contains("<td class=\"highlight\">System Type</td><td>Laptop</td>"));
    assert_eq!(document.attachment_name, "vosu-report-42.html");
    assert_eq!(document.content_type, CONTENT_TYPE);
    let response = document.into_response();
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"vosu-report-42.html\"",
    );
}

#[tokio::test]
async fn newest_logs_are_deterministic_capped_and_escaped() {
    let directory = TempDir::new().unwrap();
    for index in 0..12 {
        let path = directory.path().join(format!("{index:02}&.log"));
        let level = if index == 11 { "derror" } else { "info" };
        fs::write(
            &path,
            format!("00:00:{index:02}.000 [{level}] line<{index}&\nraw<script>"),
        )
        .unwrap();
        let time = UNIX_EPOCH + Duration::from_secs(index + 1);
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }
    let input = ReportInput {
        logs_dir: Some(directory.path().into()),
        ..ReportInput::default()
    };
    let doc = generate_report_document_at(input, &FixedProbe(MachineData::unknown()), UNIX_EPOCH)
        .await
        .unwrap();
    let mut lazy_stream = ReportBodyStream::new(doc.report.clone());
    assert!(matches!(lazy_stream.phase, StreamPhase::Head));
    assert_eq!(lazy_stream.log_index, 0);
    let first_chunk = lazy_stream.next().await.unwrap().unwrap();
    assert!(matches!(lazy_stream.phase, StreamPhase::LogHeader));
    assert!(
        String::from_utf8(first_chunk.to_vec())
            .unwrap()
            .contains("System Configuration")
    );
    let html = rendered(&doc).await;
    assert_eq!(html.matches("class=\"group logs\"").count(), 10);
    assert!(!html.contains("01&amp;.log"));
    assert!(html.find("11&amp;.log").unwrap() < html.find("10&amp;.log").unwrap());
    assert!(html.contains("line&lt;11&amp;"));
    assert!(html.contains("raw&lt;script&gt;"));
    assert!(html.contains("<thead><tr><th>#</th><th class=\"timestamp\">Timestamp</th><th class=\"type\">Type</th><th class=\"message\">Message</th></tr></thead>"));
    assert!(html.contains("<tr><td class=\"highlight\">1</td><td>00:00:11.000</td><td class=\"status-debugError\">debugError</td>"));
    assert!(html.contains("<tr><td class=\"highlight\">2</td><td></td><td class=\"status-\"></td><td>raw&lt;script&gt;</td>"));
    assert!(!html.contains("status-derror"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_device_probe_uses_battery_chassis_and_unknown_fallback() {
    let root = TempDir::new().unwrap();
    let power = root.path().join("power");
    let chassis = root.path().join("chassis");
    fs::create_dir(&power).unwrap();
    fs::write(&chassis, "3\n").unwrap();
    assert_eq!(
        classify_linux_device(&power, &[&chassis]),
        DeviceType::Desktop
    );
    fs::create_dir(power.join("BAT0")).unwrap();
    assert_eq!(
        classify_linux_device(&power, &[&chassis]),
        DeviceType::Laptop
    );
    assert_eq!(
        classify_linux_device(
            &root.path().join("missing"),
            &[&root.path().join("also-missing")]
        ),
        DeviceType::Unknown
    );
}

#[tokio::test]
async fn missing_data_is_resilient_and_order_is_stable() {
    let input = ReportInput {
        instances: vec![
            ReportInstance {
                pid: 20,
                client_type: String::new(),
                bitness: String::new(),
                version: String::new(),
            },
            ReportInstance {
                pid: 2,
                client_type: String::new(),
                bitness: String::new(),
                version: String::new(),
            },
        ],
        counters: vec![
            ReportCounter {
                folder_name: "z".into(),
                name: "a".into(),
                author: String::new(),
                version: String::new(),
            },
            ReportCounter {
                folder_name: "a".into(),
                name: "z".into(),
                author: String::new(),
                version: String::new(),
            },
        ],
        logs_dir: None,
        ..ReportInput::default()
    };
    let document =
        generate_report_document_at(input, &FixedProbe(MachineData::unknown()), UNIX_EPOCH)
            .await
            .unwrap();
    let html = rendered(&document).await;
    assert!(html.contains("Unknown-x86_64") || html.contains("Unknown-Unknown"));
    assert!(html.find("<td>2</td>").unwrap() < html.find("<td>20</td>").unwrap());
    assert!(
        html.find("<td>z</td><td></td><td></td><td>a</td>").unwrap()
            < html.find("<td>a</td><td></td><td></td><td>z</td>").unwrap()
    );
}

#[test]
fn parser_requires_anchored_timestamp_and_normalizes_debug_error() {
    assert_eq!(
        parse_log_line("12:34:56.789 [derror] boom"),
        LogLine {
            timestamp: "12:34:56.789".into(),
            level: "debugError".into(),
            text: "boom".into(),
        },
    );
    for line in [
        "prefix 12:34:56.789 [info] text",
        "12:34:56 [info] text",
        "12:34:56.789[info] text",
        "12:34:56.789 [] text",
        "12:34:56.789 [info]text",
    ] {
        assert_eq!(
            parse_log_line(line),
            LogLine {
                timestamp: String::new(),
                level: String::new(),
                text: line.into(),
            },
        );
    }
}

#[tokio::test]
async fn configured_log_io_errors_surface() {
    let input = ReportInput {
        logs_dir: Some(PathBuf::from("/definitely/missing/vosu/logs")),
        ..ReportInput::default()
    };
    let error = generate_report_document_at(input, &FixedProbe(MachineData::unknown()), UNIX_EPOCH)
        .await
        .unwrap_err();
    assert!(matches!(error, ReportError::Logs(_)));
}

#[tokio::test]
async fn oversized_logs_are_bounded_with_visible_truncation() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("oversized.log");
    let mut contents = "00:00:00.000 [info] first\n".repeat(LOG_MAX_LINES + 1);
    contents.push_str(&"x".repeat(LOG_MAX_BYTES));
    fs::write(path, contents).unwrap();

    let input = ReportInput {
        logs_dir: Some(directory.path().into()),
        ..ReportInput::default()
    };
    let document =
        generate_report_document_at(input, &FixedProbe(MachineData::unknown()), UNIX_EPOCH)
            .await
            .unwrap();
    let html = rendered(&document).await;
    assert!(html.contains("[log truncated: maximum"));
    assert!(html.len() < LOG_MAX_BYTES * 2);
}
