use crate::{BUNDLE, BUNDLED};
use vosu_plugin_api::manifest::PackageManifest;

#[test]
fn root_library_exports_only_the_bundle_and_bundled_array() {
    let source = syn::parse_file(include_str!("../lib.rs")).unwrap();
    let mut private_modules = Vec::new();
    let mut public_items = Vec::new();
    for item in source.items {
        match item {
            syn::Item::Mod(item)
                if matches!(item.vis, syn::Visibility::Inherited)
                    && item.content.is_none()
                    && item.attrs.is_empty() =>
            {
                private_modules.push(item.ident.to_string());
            }
            syn::Item::Use(item)
                if matches!(item.vis, syn::Visibility::Public(_))
                    && item.leading_colon.is_none()
                    && item.attrs.is_empty() =>
            {
                let syn::UseTree::Path(path) = item.tree else {
                    panic!("expected the bundle's re-export");
                };
                assert_eq!(path.ident, "plugins");
                let syn::UseTree::Group(group) = *path.tree else {
                    panic!("expected an explicit list of bundle exports");
                };
                for item in group.items {
                    let syn::UseTree::Name(name) = item else {
                        panic!("forbidden export from the root library");
                    };
                    public_items.push(name.ident.to_string());
                }
            }
            _ => panic!("root library must not define or hide a service graph"),
        }
    }
    assert_eq!(private_modules, ["bundle", "plugins"]);
    assert_eq!(public_items, ["BUNDLE", "BUNDLED"]);
}

#[test]
fn executable_bundle_version_matches_the_package_and_bundled_manifests() {
    assert_eq!(BUNDLE.version, env!("CARGO_PKG_VERSION"));
    let host = BUNDLE.version();
    // The harness appends a synthetic plugin with its own fixture version.
    let production = if cfg!(feature = "test-harness") {
        BUNDLED.split_last().unwrap().1
    } else {
        BUNDLED
    };
    for entry in production {
        let manifest = PackageManifest::parse(entry.manifest)
            .unwrap()
            .into_plugin()
            .unwrap();
        assert_eq!(manifest.version, BUNDLE.version, "{}", manifest.id);
        assert!(
            semver::VersionReq::parse(&manifest.host)
                .unwrap()
                .matches(&host),
            "{}",
            manifest.id
        );
    }
    assert!(
        !semver::VersionReq::parse(">=1.2.0, <3.0.0")
            .unwrap()
            .matches(&host)
    );
}

#[test]
fn bundled_manifests_match_the_exact_executable_inventory() {
    let ids: Vec<_> = BUNDLED
        .iter()
        .map(|entry| {
            PackageManifest::parse(entry.manifest)
                .unwrap()
                .into_plugin()
                .unwrap()
                .id
        })
        .collect();
    assert_eq!(ids, BUNDLE.plugin_ids);
}

#[test]
fn dashboard_instance_is_the_bundled_manifests_position() {
    let manifests = BUNDLED.iter().map(|entry| {
        PackageManifest::parse(entry.manifest)
            .unwrap()
            .into_plugin()
            .unwrap()
    });
    let position = manifests
        .enumerate()
        .find(|(_, manifest)| manifest.id == BUNDLE.dashboard)
        .unwrap()
        .0;
    assert_eq!(
        BUNDLE.instance(BUNDLE.dashboard),
        Some(vosu_core::plugin::PluginInstanceId::new(position as u64))
    );
}

#[test]
fn bundle_poll_duration_is_bounded_even_for_an_overflowing_interval() {
    let duration = BUNDLE.hosts.poll_duration(f64::MAX);
    assert_eq!(duration, std::time::Duration::from_secs(60));
    assert!(std::time::Instant::now().checked_add(duration).is_some());
}
