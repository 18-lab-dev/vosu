use super::support::*;
use serde_json::json;
use vosu_plugin_api::manifest::{
    HostSupport, NamespaceIndex, PackageKind, Registration, ValidationContext,
};
fn registration(id: &str) -> Registration<'_> {
    Registration {
        kind: PackageKind::Plugin,
        id,
        source: "other-source",
        official: false,
        builtin: false,
        replaces: &[],
    }
}
#[test]
fn namespace_registration_is_symmetric_segment_delimited_and_order_independent() {
    for ids in [["a.b", "a.b.c"], ["a.b.c", "a.b"]] {
        let good = [registration(ids[0]), registration("other.plugin")];
        assert!(NamespaceIndex::new(&good).is_ok());
        let bad = ids.map(registration);
        let error = NamespaceIndex::new(&bad).unwrap_err();
        assert_eq!(error.rule, "manifest.namespace");
    }
    for ids in [["a.b", "a.bc", "a.b"], ["a.bc", "a.b", "a.b"]] {
        let registrations = ids.map(registration);
        let index = NamespaceIndex::new(&registrations).unwrap();
        assert_eq!(index.resolve("a.bc.camelCase"), Some(("a.bc", "camelCase")));
        assert_eq!(index.resolve("a.b.camelCase"), Some(("a.b", "camelCase")));
        assert_eq!(index.resolve("a.bcd.camelCase"), None);
    }
    for ns in [
        "vosu.client",
        "vosu.status",
        "vosu.chart",
        "vosu.play",
        "vosu.skin",
        "vosu.sessions",
    ] {
        for id in [ns.to_owned(), format!("{ns}.child")] {
            let mut good = manifest("plugin");
            good["id"] = json!("other.plugin");
            assert!(parse(&good).is_ok());
            good["id"] = json!(id);
            let error = parse(&good).unwrap_err();
            assert_eq!(error.rule, "manifest.id");
            assert_eq!(error.pointer, "/id");
        }
    }
}
#[test]
fn explicit_officialness_and_application_prerelease_support_are_distinct() {
    let app = "2.0.0-alpha.1".parse().unwrap();
    let mut value = manifest("plugin");
    value["id"] = json!("vosu.corpus");
    value["host"] = json!(">=2.0.0-alpha.1, <3.0.0");
    let package = parse(&value).unwrap();
    let mut context = ValidationContext {
        application_version: &app,
        source: "source",
        official: true,
        registrations: &[],
        namespace_revisions: &[],
    };
    assert_eq!(
        package.validate_context(&context).unwrap(),
        HostSupport::Supported
    );
    context.official = false;
    let error = package.validate_context(&context).unwrap_err();
    assert_eq!(error.rule, "manifest.namespace");
    assert_eq!(error.pointer, "/id");
    value["id"] = json!("acme.corpus");
    value["host"] = json!(">=2.0.0, <3.0.0");
    assert_eq!(
        parse(&value).unwrap().validate_context(&context).unwrap(),
        HostSupport::Unsupported
    );
}

#[test]
fn an_unselected_loopback_catalog_cannot_claim_a_reserved_plugin_identity() {
    let source = vosu_core::catalog::sources::SourceIdentity::parse(
        "https://catalog.invalid:44433/unselected/catalog-v2.json",
    )
    .unwrap()
    .identity;
    assert!(!source.is_official());
    let mut value = manifest("plugin");
    value["id"] = json!("vosu.test.catalog-fixture");
    let package = parse(&value).unwrap();
    let error = package
        .validate_context(&ValidationContext {
            application_version: &vosu::BUNDLE.version(),
            source: source.as_str(),
            official: source.is_official(),
            registrations: &[],
            namespace_revisions: &[],
        })
        .unwrap_err();
    assert_eq!(error.rule, "manifest.namespace");
    assert_eq!(error.pointer, "/id");
}

#[test]
fn authoritative_foreign_revision_and_source_local_replacements_matrix() {
    use vosu_plugin_api::manifest::{NamespaceRevision, SchemaProvenance};
    let app = &vosu::BUNDLE.version();
    let mut value = manifest("plugin");
    value["provides"] = json!([{"id":"field:vosu.play.combo","version":"1.0.0"}]);
    let package = parse(&value).unwrap();
    let revision = [NamespaceRevision {
        namespace: "vosu.play",
        version: "1.0.0".parse().unwrap(),
        provenance: SchemaProvenance::TrustedBundled,
    }];
    let mut context = ValidationContext {
        application_version: app,
        source: "source",
        official: false,
        registrations: &[],
        namespace_revisions: &revision,
    };
    assert_eq!(
        package.validate_context(&context).unwrap(),
        HostSupport::Supported
    );
    context.namespace_revisions = &[];
    let error = package.validate_context(&context).unwrap_err();
    assert_eq!(error.rule, "manifest.authoritative-version");
    assert_eq!(error.pointer, "/provides/0/version");
    for (version, provenance) in [
        ("1.1.0", SchemaProvenance::TrustedBundled),
        ("1.0.0", SchemaProvenance::ThirdParty),
    ] {
        let revision = [NamespaceRevision {
            namespace: "vosu.play",
            version: version.parse().unwrap(),
            provenance,
        }];
        let context = ValidationContext {
            namespace_revisions: &revision,
            ..context
        };
        assert_eq!(
            package.validate_context(&context).unwrap_err().rule,
            "manifest.authoritative-version"
        );
    }
    let mut value = manifest("overlay");
    value["replaces"] = json!(["old"]);
    let package = parse(&value).unwrap();
    for (kind, source, builtin, valid) in [
        (PackageKind::Overlay, "other", false, true),
        (PackageKind::Overlay, "source", false, false),
        (PackageKind::Overlay, "other", true, false),
        (PackageKind::Tool, "source", false, true),
    ] {
        let registrations = [Registration {
            kind,
            id: "old",
            source,
            official: false,
            builtin,
            replaces: &[],
        }];
        let context = ValidationContext {
            application_version: app,
            source: "source",
            official: false,
            registrations: &registrations,
            namespace_revisions: &[],
        };
        let result = package.validate_context(&context);
        assert_eq!(result.is_ok(), valid);
        if !valid {
            let error = result.unwrap_err();
            assert_eq!(error.rule, "manifest.replaces-conflict");
            assert_eq!(error.pointer, "/replaces/0");
        }
    }
}
