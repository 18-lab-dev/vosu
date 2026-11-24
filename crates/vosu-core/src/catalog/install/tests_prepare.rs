//! Package preparation counterexamples, each with an unmodified positive control.
use super::tests::*;
use super::*;
use serde_json::{Value, json};

/// Install one official in-process plugin whose files/entry were mutated.
async fn install_mutated(mutate: impl FnOnce(&mut Plugin)) -> (Fixture, Option<String>) {
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let mut package = plugin("acme.dep", "1.0.0", "in-process", json!([]));
    mutate(&mut package);
    package.publish(&fixture.server, "/dep.zip");
    let job = fixture
        .install(
            plan(
                &catalog(&[&package], &[]),
                &record(OFFICIAL, true),
                PackageKind::Plugin,
                "acme.dep",
            ),
            &[],
        )
        .await;
    (fixture, job.error)
}

fn set_manifest(package: &mut Plugin, pointer: &str, value: Value) {
    *package.manifest.pointer_mut(pointer).unwrap() = value;
    package.files[0].1 = toml::to_string(&package.manifest).unwrap().into_bytes();
}

#[tokio::test]
async fn positive_control_installs_unmodified_package() {
    let (fixture, error) = install_mutated(|_| {}).await;
    assert_eq!(error, None);
    assert_eq!(fixture.records().records.len(), 1);
}

#[tokio::test]
async fn manifest_id_version_and_single_lane_mismatches_are_refused() {
    for (label, mutate, expected) in [
        (
            "id",
            Box::new(|p: &mut Plugin| {
                // A self-consistent package for another id (own namespace).
                set_manifest(p, "/provides/0/id", json!("field:acme.other"));
                set_manifest(p, "/id", json!("acme.other"));
            }) as Box<dyn FnOnce(&mut Plugin)>,
            "catalog.mirror at /id",
        ),
        (
            "version",
            Box::new(|p: &mut Plugin| set_manifest(p, "/version", json!("1.0.1"))),
            "catalog.mirror at /version",
        ),
        (
            "single lane",
            Box::new(|p: &mut Plugin| {
                p.entry["provides"][0]["lanes"][0]["lane"] = json!("precise");
            }),
            "catalog.mirror at /provides/0/lanes",
        ),
        (
            "schema lane bytes",
            Box::new(|p: &mut Plugin| {
                p.files[1].1 = json!({"title":"acme.dep","version":"1.0.0","type":"object",
                    "properties":{"combo":{"x-sampling-lane":{"lane":"regular","activity":"always"}}}})
                .to_string()
                .into_bytes();
            }),
            "catalog.mirror at /provides/0/lanes",
        ),
        (
            "runtime",
            Box::new(|p: &mut Plugin| set_manifest(p, "/runtime", json!("process"))),
            "catalog.mirror at /runtime",
        ),
    ] {
        let (fixture, error) = install_mutated(mutate).await;
        assert_eq!(
            error,
            Some(format!(
                "package differs from its catalog entry: {expected}"
            )),
            "{label}"
        );
        fixture.assert_untouched();
    }
}

#[tokio::test]
async fn schema_path_escape_and_missing_target_library_are_refused() {
    let (fixture, error) = install_mutated(|p| {
        set_manifest(p, "/provides/0/schema", json!("../escape.json"));
    })
    .await;
    assert_eq!(
        error.as_deref(),
        Some("package differs from its catalog entry: manifest.path at /provides")
    );
    fixture.assert_untouched();
    let (fixture, error) = install_mutated(|p| {
        p.files.retain(|(name, _)| !name.starts_with("lib/"));
    })
    .await;
    assert_eq!(
        error.as_deref(),
        Some("package asset escapes or is missing: lib/libacme_dep.so")
    );
    fixture.assert_untouched();
}

#[tokio::test]
async fn windows_entry_collisions_and_late_crc_fail_before_publication() {
    for (label, extra, expected) in [
        (
            "case",
            vec![("Schemas/acme.dep.json", b"{}".to_vec())],
            "unsafe package archive: portable name collision",
        ),
        (
            "device",
            vec![("lib/AUX.so", b"x".to_vec())],
            "unsafe package archive: nonportable entry name",
        ),
    ] {
        let (fixture, error) = install_mutated(|p| {
            p.files
                .extend(extra.into_iter().map(|(n, b)| (n.into(), b)))
        })
        .await;
        assert_eq!(error.as_deref(), Some(expected), "{label}");
        fixture.assert_untouched();
    }
    // Late CRC: the last entry's checksum fields are corrupted, so every
    // earlier entry decodes and is written before the failure.
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let mut package = plugin("acme.dep", "1.0.0", "in-process", json!([]));
    package
        .files
        .push(("zz-last.bin".into(), b"checked only at its end".to_vec()));
    let mut bytes = package.archive();
    let crc = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
        .unwrap()
        .by_name("zz-last.bin")
        .unwrap()
        .crc32()
        .to_le_bytes();
    let mut patched = 0;
    for index in 0..bytes.len() - 4 {
        if bytes[index..index + 4] == crc {
            bytes[index] ^= 0xff;
            patched += 1;
        }
    }
    assert_eq!(patched, 2);
    package.entry["packages"] = json!({TARGET:{"url":fixture.server.url("/crc.zip"),
        "sha256":sha(&bytes),"size":bytes.len()}});
    fixture.server.serve("/crc.zip", bytes);
    let job = fixture
        .install(
            plan(
                &catalog(&[&package], &[]),
                &record(OFFICIAL, true),
                PackageKind::Plugin,
                "acme.dep",
            ),
            &[],
        )
        .await;
    assert_eq!(
        job.error.as_deref(),
        Some("unsafe package archive: malformed or corrupt archive")
    );
    fixture.assert_untouched();
}

#[test]
fn confined_reads_refuse_links_traversal_and_non_files() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("schemas")).unwrap();
    std::fs::write(root.path().join("schemas/a.json"), b"{}").unwrap();
    assert!(preflight::confined(root.path(), "schemas/a.json").is_ok());
    for path in [
        "../a.json",
        "schemas/../schemas/a.json",
        "/abs",
        "schemas",
        "missing.json",
        "schemas\\a.json",
        "con.json",
    ] {
        assert!(preflight::confined(root.path(), path).is_err(), "{path}");
    }
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.json"),
            root.path().join("schemas/link.json"),
        )
        .unwrap();
        assert!(preflight::confined(root.path(), "linked/secret.json").is_err());
        assert!(preflight::confined(root.path(), "schemas/link.json").is_err());
    }
}

#[test]
fn oversized_entry_is_refused_by_the_pinned_cap() {
    // The plan pins the cap; expanded size uses the same cap (catalog-v2.md).
    let file = {
        let mut file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut file, &zip(&[("big.bin", &[7u8; 4096])])).unwrap();
        file
    };
    let parent = tempfile::tempdir().unwrap();
    let error = preflight::stage(file, parent.path(), 1024, &CancellationToken::new()).unwrap_err();
    assert_eq!(
        error,
        InstallError::UnsafeArchive("expanded size limit exceeded".into())
    );
}

/// Native Windows only: a junction (reparse point) is never followed.
#[cfg(windows)]
#[test]
fn windows_junction_in_package_tree_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.json"), b"{}").unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.path().join("linked"))
        .arg(outside.path())
        .status()
        .unwrap();
    assert!(status.success());
    // Positive control: the target file itself is readable through the link.
    assert!(root.path().join("linked/secret.json").is_file());
    assert!(preflight::confined(root.path(), "linked/secret.json").is_err());
}
