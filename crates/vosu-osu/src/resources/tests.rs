#![expect(
    clippy::disallowed_methods,
    reason = "Tests write isolated temporary resource fixtures"
)]

use super::*;
use std::{collections::BTreeMap, fs, time::Duration};
use vosu_plugin_api::{CborValue, ResourceRef, ResourceResolved, StreamChunk, StreamRead};

fn reference(resources: &PreparedResources, kind: &str) -> ResourceRef {
    resources
        .references()
        .into_iter()
        .find(|r| r.kind == kind)
        .unwrap()
}
fn stream(resources: &mut PreparedResources, reference: &ResourceRef) -> u64 {
    let ResourceResolved::Stream { stream, .. } =
        resources.resolve(&reference.kind, &reference.key).unwrap()
    else {
        panic!("not stream")
    };
    stream
}
fn bytes(resources: &PreparedResources, stream: u64, offset: u64, len: u32) -> Vec<u8> {
    let StreamChunk::Data { data } = resources
        .read(StreamRead {
            stream,
            offset,
            len,
        })
        .unwrap()
    else {
        panic!("not bytes")
    };
    data.0
}
fn selected(root: &std::path::Path) -> Selection {
    Selection {
        roots: RootPaths {
            songs: Some(root.into()),
            skin: Some(root.into()),
        },
        chart: Some("map.osu".into()),
        audio: Some("sound.ogg".into()),
        background: Some("image.png".into()),
        set: Some(TreeSource::Folder(String::new())),
        skin: Some(TreeSource::Folder(String::new())),
        ..Selection::default()
    }
}
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (path, bytes) in [
        ("map.osu", b"stable chart".as_slice()),
        ("sound.ogg", b"distinct audio"),
        ("image.png", b"background pixels"),
    ] {
        fs::write(root.path().join(path), bytes).unwrap();
    }
    root
}

#[test]
fn resources_five_kinds_and_missing_roots_are_not_cwd() {
    let root = fixture();
    let prepared = PreparedResources::prepare(selected(root.path()));
    let mut kinds: Vec<_> = prepared.references().into_iter().map(|r| r.kind).collect();
    kinds.sort();
    assert_eq!(kinds, ["audio", "background", "file", "set", "skin"]);
    assert_eq!(
        prepared
            .declarations(71)
            .iter()
            .map(|r| r.handle)
            .collect::<Vec<_>>(),
        [Some(71), Some(71)]
    );
    for songs in [
        None,
        Some(std::path::PathBuf::new()),
        Some("relative".into()),
    ] {
        let selection = Selection {
            roots: RootPaths { songs, skin: None },
            chart: Some("Cargo.toml".into()),
            ..Selection::default()
        };
        let prepared = PreparedResources::prepare(selection);
        assert!(prepared.references().is_empty());
        assert!(prepared.declarations(72).is_empty());
    }
}

#[test]
fn resources_mutable_files_keep_original_bytes_and_new_identity() {
    let root = fixture();
    let mut old = PreparedResources::prepare(selected(root.path()));
    let old_ref = reference(&old, CHART);
    fs::write(root.path().join("map.osu"), b"mutant chart").unwrap();
    let mut new = PreparedResources::prepare(selected(root.path()));
    let new_ref = reference(&new, CHART);
    assert_ne!(old_ref.content_id, new_ref.content_id);
    assert_ne!(old_ref.key, new_ref.key);
    let lease = stream(&mut old, &old_ref);
    assert_eq!(bytes(&old, lease, 0, 100), b"stable chart");
    assert!(new.resolve(CHART, &old_ref.key).is_none());
    let lease = stream(&mut new, &new_ref);
    assert_eq!(bytes(&new, lease, 0, 100), b"mutant chart");
}

#[test]
fn resources_independent_stream_leases_ranges_eof_and_capacity() {
    let root = fixture();
    let mut prepared = PreparedResources::prepare(selected(root.path()));
    let reference = reference(&prepared, AUDIO);
    let a = stream(&mut prepared, &reference);
    let b = stream(&mut prepared, &reference);
    assert_ne!(a, b);
    prepared.close(a);
    assert!(
        prepared
            .read(StreamRead {
                stream: a,
                offset: 0,
                len: 4
            })
            .is_none()
    );
    assert_eq!(bytes(&prepared, b, 9, 5), b"audio");
    assert!(bytes(&prepared, b, u64::MAX, 4).is_empty());
    assert!(
        prepared
            .read(StreamRead {
                stream: b,
                offset: 0,
                len: vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES + 1
            })
            .is_none()
    );
    for _ in 1..64 {
        stream(&mut prepared, &reference);
    }
    assert!(prepared.resolve(AUDIO, &reference.key).is_none());
    prepared.close(b);
    assert!(prepared.resolve(AUDIO, &reference.key).is_some());
}

#[test]
fn resources_lazer_and_stable_use_distinct_roots_and_logical_names() {
    let stable = fixture();
    let lazer = tempfile::tempdir().unwrap();
    let hash = "0123456789abcdef0123456789abcdef";
    fs::write(lazer.path().join(hash), b"lazer unlike stable").unwrap();
    fs::write(stable.path().join(hash), b"stable hash impostor").unwrap();
    let mapping = BTreeMap::from([("Pretty.PNG".into(), hash.into())]);
    let mut selection = selected(stable.path());
    selection.lazer = true;
    selection.roots.songs = Some(lazer.path().into());
    selection.background = Some(hash.into());
    selection.background_name = Some("Pretty.PNG".into());
    selection.set = Some(TreeSource::Store(mapping.clone()));
    selection.skin = Some(TreeSource::Store(mapping));
    let mut prepared = PreparedResources::prepare(selection);
    let reference = reference(&prepared, BACKGROUND);
    assert_eq!(reference.name, "Pretty.PNG");
    let lease = stream(&mut prepared, &reference);
    assert_eq!(bytes(&prepared, lease, 0, 100), b"lazer unlike stable");
    let skin = self::reference(&prepared, SKIN);
    assert_eq!(
        prepared.lookup(SKIN, &skin.key, "pretty.png"),
        Some(ResourceResolved::TreePath {
            root: "songs".into(),
            path: hash.into(),
            entry_path: "Pretty.PNG".into()
        })
    );
    let set = self::reference(&prepared, SET);
    assert!(prepared.lookup(SET, &set.key, "pretty.png").is_none());
    let Some(ResourceResolved::Tree {
        entries, truncated, ..
    }) = prepared.list(SET, &set.key)
    else {
        panic!("no index")
    };
    assert!(!truncated);
    assert_eq!(entries[0].path, "Pretty.PNG");
    assert_eq!(entries[0].hash.as_deref(), Some(hash));
}

#[test]
fn resources_skin_exact_then_deterministic_case_fallback_and_rejects_escapes() {
    let root = fixture();
    fs::create_dir(root.path().join("Sub")).unwrap();
    fs::write(root.path().join("Sub/Hit.PNG"), b"case pixels").unwrap();
    let prepared = PreparedResources::prepare(selected(root.path()));
    let reference = reference(&prepared, SKIN);
    assert_eq!(
        prepared.lookup(SKIN, &reference.key, "sub\\hit.png"),
        Some(ResourceResolved::TreePath {
            root: "skin".into(),
            path: "Sub/Hit.PNG".into(),
            entry_path: "Sub/Hit.PNG".into()
        })
    );
    for path in [
        "../map.osu",
        "/map.osu",
        "C:\\map.osu",
        "map.osu:secret",
        "Sub//Hit.PNG",
        "Sub/./Hit.PNG",
        "missing.png",
    ] {
        assert!(
            prepared.lookup(SKIN, &reference.key, path).is_none(),
            "{path}"
        );
    }
    assert!(
        prepared
            .lookup(SKIN, &CborValue::Text("wrong-generation".into()), "map.osu")
            .is_none()
    );
}

#[test]
fn resources_truncated_index_never_authorizes_omitted_member() {
    let root = fixture();
    let files = (0..=super::set::MAX_ENTRIES)
        .map(|i| (format!("entry-{i:05}.png"), "image.png".into()))
        .collect();
    let mut selection = selected(root.path());
    selection.skin = Some(TreeSource::Store(files));
    let prepared = PreparedResources::prepare(selection);
    let reference = reference(&prepared, SKIN);
    let Some(ResourceResolved::Tree {
        entries, truncated, ..
    }) = prepared.list(SKIN, &reference.key)
    else {
        panic!("no index")
    };
    assert!(truncated);
    assert_eq!(entries.len(), super::set::MAX_ENTRIES);
    assert!(
        prepared
            .lookup(SKIN, &reference.key, "entry-20000.png")
            .is_none()
    );
    assert!(
        prepared
            .lookup(SKIN, &reference.key, "ENTRY-00007.PNG")
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn resources_symlink_escape_never_becomes_indexed_or_snapshotted() {
    let root = fixture();
    let outside = fixture();
    std::os::unix::fs::symlink(
        outside.path().join("map.osu"),
        root.path().join("escape.osu"),
    )
    .unwrap();
    let mut selection = selected(root.path());
    selection.chart = Some("escape.osu".into());
    let prepared = PreparedResources::prepare(selection);
    assert!(!prepared.references().iter().any(|r| r.kind == CHART));
    let reference = reference(&prepared, SET);
    assert!(prepared.lookup(SET, &reference.key, "escape.osu").is_none());
}

#[tokio::test]
async fn resources_cold_worker_preparation_and_shutdown_are_owned() {
    let root = fixture();
    let worker = ResourceWorker::start();
    let reply = worker.prepare(selected(root.path())).unwrap();
    assert!(worker.prepare(selected(root.path())).is_none());
    let mut prepared = tokio::time::timeout(Duration::from_secs(10), reply)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prepared.references().len(), 5);
    let old_reference = reference(&prepared, AUDIO);
    let old_stream = stream(&mut prepared, &old_reference);
    let reply = worker.prepare(selected(root.path())).unwrap();
    let mut replacement = tokio::time::timeout(Duration::from_secs(10), reply)
        .await
        .unwrap()
        .unwrap();
    let new_stream = stream(&mut replacement, &old_reference);
    assert_ne!(old_stream, new_stream);
    assert!(
        replacement
            .read(StreamRead {
                stream: old_stream,
                offset: 0,
                len: 8
            })
            .is_none()
    );
    assert_eq!(bytes(&prepared, old_stream, 0, 8), b"distinct");
    tokio::time::timeout(Duration::from_secs(10), worker.shutdown())
        .await
        .unwrap();
}

#[test]
fn resources_skin_exact_beats_folded_alias_and_failed_open_can_recover() {
    let root = fixture();
    let mut selection = selected(root.path());
    selection.chart = Some("not-yet.osu".into());
    selection.skin = Some(TreeSource::Store(BTreeMap::from([
        ("HIT.PNG".into(), "map.osu".into()),
        ("hit.png".into(), "image.png".into()),
    ])));
    let prepared = PreparedResources::prepare(selection.clone());
    assert!(!prepared.references().iter().any(|r| r.kind == CHART));
    let skin = reference(&prepared, SKIN);
    assert_eq!(
        prepared.lookup(SKIN, &skin.key, "hit.png"),
        Some(ResourceResolved::TreePath {
            root: "skin".into(),
            path: "image.png".into(),
            entry_path: "hit.png".into(),
        })
    );
    assert_eq!(
        prepared.lookup(SKIN, &skin.key, "Hit.Png"),
        Some(ResourceResolved::TreePath {
            root: "skin".into(),
            path: "map.osu".into(),
            entry_path: "HIT.PNG".into(),
        })
    );
    fs::write(root.path().join("not-yet.osu"), b"late arrival").unwrap();
    let mut recovered = PreparedResources::prepare(selection);
    let reference = reference(&recovered, CHART);
    let lease = stream(&mut recovered, &reference);
    assert_eq!(bytes(&recovered, lease, 0, 100), b"late arrival");
}
