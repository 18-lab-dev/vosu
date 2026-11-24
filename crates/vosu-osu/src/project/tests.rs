use super::*;
use serde_json::Value;
use vosu_plugin_api::{PublishBatchWriter, PublishValueRef, decode_publish_batch_ref};

#[test]
fn pointer_partition_is_non_overlapping_and_covers_every_required_key() {
    let mut seen = std::collections::BTreeSet::new();
    for (index, at) in OsuPointer::ALL.iter().enumerate() {
        assert_eq!(*at as usize, index);
        assert!(seen.insert((at.ns(), at.pointer())));
        for other in OsuPointer::ALL {
            if at != other && at.ns() == other.ns() {
                assert!(
                    !other
                        .pointer()
                        .strip_prefix(at.pointer())
                        .is_some_and(|tail| tail.starts_with('/')),
                    "{at:?} overlaps {other:?}"
                );
            }
        }
    }
    let schemas = [
        ("vosu.osu", crate::OSU_NAMESPACE_SCHEMA),
        (
            "vosu.client",
            include_str!("../../../../docs/v2/schemas/vosu.client.schema.json"),
        ),
        (
            "vosu.status",
            include_str!("../../../../docs/v2/schemas/vosu.status.schema.json"),
        ),
        (
            "vosu.chart",
            include_str!("../../../../docs/v2/schemas/vosu.chart.schema.json"),
        ),
        (
            "vosu.play",
            include_str!("../../../../docs/v2/schemas/vosu.play.schema.json"),
        ),
        (
            "vosu.skin",
            include_str!("../../../../docs/v2/schemas/vosu.skin.schema.json"),
        ),
    ];
    fn required(schema: &Value, path: &str, out: &mut Vec<String>) {
        let Some(keys) = schema.get("required").and_then(Value::as_array) else {
            return;
        };
        for key in keys {
            let key = key.as_str().unwrap();
            let path = format!("{path}/{key}");
            out.push(path.clone());
            required(&schema["properties"][key], &path, out);
        }
    }
    for (ns, text) in schemas {
        let schema: Value = serde_json::from_str(text).unwrap();
        let mut keys = Vec::new();
        required(&schema, "", &mut keys);
        for path in keys {
            // Core-owned fields are materialized by the bus, never the plugin.
            if ns == "vosu.client"
                || (ns == "vosu.chart" && path == "/resources")
                || (ns == "vosu.skin" && path == "/tree")
            {
                continue;
            }
            assert!(
                OsuPointer::ALL.iter().any(|at| at.ns() == ns
                    && (at.pointer() == path
                        || path
                            .strip_prefix(at.pointer())
                            .is_some_and(|tail| tail.starts_with('/'))
                        || at
                            .pointer()
                            .strip_prefix(&path)
                            .is_some_and(|tail| tail.starts_with('/')))),
                "missing {ns}{path}"
            );
        }
        for at in OsuPointer::ALL.iter().filter(|at| at.ns() == ns) {
            let mut node = &schema;
            for token in at.pointer().split('/').skip(1) {
                node = &node["properties"][token];
                assert!(!node.is_null(), "unknown pointer {at:?}");
            }
        }
    }
}

#[test]
fn diff_sink_emits_only_changes() {
    let mut last = LastValues::default();
    let mut writer = PublishBatchWriter::new();
    let mut out = Vec::new();
    let emit = |last: &mut LastValues, writer: &mut PublishBatchWriter, score| {
        let mut sink = DiffSink::new(last, writer);
        sink.put(OsuPointer::PlayScore, PublishValueRef::U64(score));
        sink.put(OsuPointer::ChartTitle, PublishValueRef::String("title"));
        sink.put_cbor(OsuPointer::OsuProfile, &mut |buf| {
            buf.extend_from_slice(&[0x81, 1])
        });
    };
    writer.begin(71);
    emit(&mut last, &mut writer, 1);
    assert_eq!(writer.len(), 3);
    writer.begin(71);
    emit(&mut last, &mut writer, 1);
    assert!(writer.is_empty());
    writer.begin(71);
    emit(&mut last, &mut writer, 2);
    assert_eq!(writer.len(), 1);
    writer.finish_into(&mut out).unwrap();
    let batch = decode_publish_batch_ref(&out).unwrap();
    let entry = batch.entries().next().unwrap();
    assert_eq!(entry.pointer, "/score");
    assert!(matches!(entry.value, PublishValueRef::U64(2)));
    writer.begin(71);
    DiffSink::new(&mut last, &mut writer).put(OsuPointer::PlayScore, PublishValueRef::I64(2));
    assert_eq!(
        writer.len(),
        1,
        "equal bits with different types are changes"
    );
    writer.begin(71);
    last.invalidate();
    emit(&mut last, &mut writer, 2);
    assert_eq!(writer.len(), 3, "failed batches are retried in full");
}
