use super::*;

#[test]
fn runtime_pack_rejects_wrong_platform_and_ambiguous_runtime() {
    let target = ".NETCoreApp,Version=v10.0";
    let mut deps = serde_json::json!({
        "runtimeTarget": {"name": target},
        "targets": {target: {"runtimepack.Microsoft.NETCore.App.Runtime.wrong/10.0.12": {}}}
    });
    assert_eq!(
        runtime_pack(&deps).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let entries = deps["targets"][target].as_object_mut().unwrap();
    for version in ["8.0.29", "10.0.12"] {
        entries.insert(
            format!("runtimepack.Microsoft.NETCore.App.Runtime.{RID}/{version}"),
            serde_json::json!({}),
        );
    }
    assert_eq!(
        runtime_pack(&deps).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn runtime_manifest_cannot_link_outside_client_directory() {
    for filename in [
        "../escape.dll",
        "/tmp/escape.dll",
        "nested/file.dll",
        "C:escape.dll",
        "..\\escape.dll",
    ] {
        let pack = serde_json::json!({"runtime": {filename: {}}, "native": {}});
        assert_eq!(
            runtime_files(&pack).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn oversized_runtime_manifest_fails_before_parsing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.json");
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_MANIFEST + 1)
        .unwrap();
    let error = read_manifest(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn framework_replacements_include_transitive_dependencies_but_not_game_assemblies() {
    let target = ".NETCoreApp,Version=v8.0";
    let helper_pack = serde_json::json!({
        "runtime": {"System.Text.Json.dll": {"assemblyVersion": "8.0.0.0"}}
    });
    let mut client_pack = serde_json::json!({
        "runtime": {"System.Private.CoreLib.dll": {"assemblyVersion": "8.0.0.0"}}
    });
    let deps = serde_json::json!({
        "targets": {target: {
            "System.Text.Json/10.0.9": {
                "runtime": {"lib/net8.0/System.Text.Json.dll": {"assemblyVersion": "10.0.0.0"}},
                "dependencies": {"System.Text.Encodings.Web": "10.0.9"}
            },
            "System.Text.Encodings.Web/10.0.9": {
                "runtime": {"lib/net8.0/System.Text.Encodings.Web.dll": {"assemblyVersion": "10.0.0.0"}},
                "dependencies": {"System.IO.Pipelines": "10.0.9"}
            },
            "System.IO.Pipelines/10.0.9": {
                "runtime": {"lib/net8.0/System.IO.Pipelines.dll": {"assemblyVersion": "10.0.0.0"}},
                "dependencies": {"System.Text.Json": "10.0.9"}
            },
            "osu.Game/1.0.0": {
                "runtime": {"osu.Game.dll": {"assemblyVersion": "1.0.0.0"}}
            }
        }}
    });

    include_framework_overrides(&deps, target, &helper_pack, &mut client_pack).unwrap();

    for filename in [
        "System.Text.Json.dll",
        "System.Text.Encodings.Web.dll",
        "System.IO.Pipelines.dll",
    ] {
        assert_eq!(
            client_pack["runtime"][filename]["assemblyVersion"],
            "10.0.0.0"
        );
    }
    assert_eq!(
        client_pack["runtime"]["System.Private.CoreLib.dll"]["assemblyVersion"],
        "8.0.0.0"
    );
    assert!(client_pack["runtime"].get("osu.Game.dll").is_none());
}
