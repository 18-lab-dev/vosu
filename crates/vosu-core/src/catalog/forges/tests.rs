use super::*;
use crate::catalog::sources::{SourceGeneration, SourceIdentity};
use serde_json::{Value, json};
use vosu_net::streaming::SourcePolicy;

pub(super) fn detected(value: &str, forge: Forge) -> DetectedSource {
    let parsed = SourceIdentity::parse(value).unwrap();
    let policy = match &parsed.location {
        SourceLocation::Static { url } => SourcePolicy::static_source(url, false),
        SourceLocation::Repository { origin, .. } => SourcePolicy::repository(origin, false),
        SourceLocation::GitHub { .. } => {
            SourcePolicy::repository(&Url::parse("https://github.com/").unwrap(), false)
        }
    }
    .unwrap();
    DetectedSource {
        parsed,
        forge,
        policy,
        generation: SourceGeneration(7),
    }
}
pub(super) fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/catalog-v2/forge-releases.json"
    ))
    .unwrap()
}
fn select_for(forge: Forge, body: &Value, source: &DetectedSource) -> Result<Url, FetchError> {
    let bytes = serde_json::to_vec(body).unwrap();
    match forge {
        Forge::GitHub => github::catalog_asset(&bytes, source),
        Forge::GitLab => gitlab::catalog_asset(&bytes, source),
        Forge::Gitea => gitea::catalog_asset(&bytes, source),
        Forge::Static => unreachable!(),
    }
}
fn cases() -> [(Forge, &'static str, &'static str, &'static str); 3] {
    [
        (Forge::GitHub, "alice/catalog", "github", "url"),
        (
            Forge::GitLab,
            "https://gitlab.com/acme/team/catalog",
            "gitlab",
            "direct_asset_url",
        ),
        (
            Forge::Gitea,
            "https://codeberg.org/alice/catalog",
            "gitea",
            "browser_download_url",
        ),
    ]
}
fn asset_array(body: &mut Value, forge: Forge) -> &mut Vec<Value> {
    if forge == Forge::GitLab {
        body["assets"]["links"].as_array_mut().unwrap()
    } else {
        body["assets"].as_array_mut().unwrap()
    }
}
#[test]
fn exact_asset_selection_rejects_case_differences_and_duplicate_names() {
    for (forge, source, key, _) in cases() {
        let source = detected(source, forge);
        let mut body = fixture()[key].clone();
        assert!(select_for(forge, &body, &source).is_ok());
        asset_array(&mut body, forge)[1]["name"] = json!("Catalog-v2.json");
        assert_eq!(
            select_for(forge, &body, &source),
            Err(FetchError::CatalogAsset)
        );
        asset_array(&mut body, forge)[1]["name"] = json!("catalog-v2.json");
        let duplicate = asset_array(&mut body, forge)[1].clone();
        asset_array(&mut body, forge).push(duplicate);
        assert_eq!(
            select_for(forge, &body, &source),
            Err(FetchError::CatalogAsset)
        );
        asset_array(&mut body, forge)[2]["name"] = json!("CATALOG-v2.json");
        assert!(
            select_for(forge, &body, &source).is_ok(),
            "nonmatching case is not a matching asset"
        );
    }
}
#[test]
fn malicious_initial_assets_reject_http_userinfo_foreign_and_changed_port() {
    for (forge, value, key, field) in cases() {
        let source = detected(value, forge);
        let mut body = fixture()[key].clone();
        let good = asset_array(&mut body, forge)[1][field]
            .as_str()
            .unwrap()
            .to_owned();
        let host = source.credential_origin().host();
        for bad in [
            format!("http://{host}/asset"),
            format!("https://token@{host}/asset"),
            format!("https://@{host}/asset"),
            format!("https://{host}:444/asset"),
            "https://evil.test/asset?secret=sentinel".into(),
        ] {
            asset_array(&mut body, forge)[1][field] = json!(bad);
            assert_eq!(
                select_for(forge, &body, &source),
                Err(FetchError::AssetOrigin)
            );
        }
        asset_array(&mut body, forge)[1][field] = json!(good);
        assert!(select_for(forge, &body, &source).is_ok());
    }
}
#[test]
fn release_endpoints_select_latest_and_encode_nested_project_and_tag() {
    assert_eq!(
        github::release_url(&detected("alice/catalog", Forge::GitHub))
            .unwrap()
            .as_str(),
        "https://api.github.com/repos/alice/catalog/releases/latest"
    );
    assert_eq!(
        github::release_url(&detected("alice/catalog@v2/rc?x=1#z%2F", Forge::GitHub))
            .unwrap()
            .as_str(),
        "https://api.github.com/repos/alice/catalog/releases/tags/v2%2Frc%3Fx=1%23z%252F"
    );
    assert_eq!(
        gitlab::release_url(&detected(
            "https://gitlab.com/acme/team/catalog.git/",
            Forge::GitLab
        ))
        .unwrap()
        .as_str(),
        "https://gitlab.com/api/v4/projects/acme%2Fteam%2Fcatalog/releases/permalink/latest"
    );
    assert!(SourceIdentity::parse("https://gitlab.com/acme/%2Fteam/catalog").is_err());
    assert_eq!(
        gitea::release_url(&detected(
            "https://codeberg.org/alice/catalog",
            Forge::Gitea
        ))
        .unwrap()
        .as_str(),
        "https://codeberg.org/api/v1/repos/alice/catalog/releases/latest"
    );
    let static_source = detected(
        "https://github.com/alice/catalog/catalog-v2.json",
        Forge::Static,
    );
    assert_eq!(
        static_source.credential_origin(),
        &Origin::parse("https://github.com:443").unwrap()
    );
    assert_eq!(
        static_source::catalog_url(&static_source).unwrap().as_str(),
        static_source.parsed.identity.as_str()
    );
    let github = detected("https://github.com/alice/catalog", Forge::GitHub);
    assert_eq!(
        github.credential_origin(),
        &Origin::parse("https://api.github.com:443").unwrap()
    );
    assert_eq!(
        github::release_url(&github).unwrap().host_str(),
        Some("api.github.com")
    );
}
#[test]
fn metadata_bounds_count_bytes_and_text_without_leaking_payloads() {
    let source = detected("alice/catalog", Forge::GitHub);
    let mut body = fixture()["github"].clone();
    let asset = body["assets"][1].clone();
    body["assets"] = json!(vec![asset; MAX_ASSETS + 1]);
    assert_eq!(
        select_for(Forge::GitHub, &body, &source),
        Err(FetchError::InvalidMetadata)
    );
    body = fixture()["github"].clone();
    body["assets"][0]["name"] = json!("x".repeat(MAX_METADATA_TEXT + 1));
    assert_eq!(
        select_for(Forge::GitHub, &body, &source),
        Err(FetchError::InvalidMetadata)
    );
    assert_eq!(
        github::catalog_asset(&vec![b' '; MAX_METADATA_BYTES + 1], &source),
        Err(FetchError::InvalidMetadata)
    );
    assert_eq!(
        github::catalog_asset(b"not JSON secret-token", &source),
        Err(FetchError::InvalidMetadata)
    );
}
#[test]
fn credential_kinds_and_guidance_are_closed_and_read_only() {
    let token = CredentialInput::Token {
        token: "distinct-token".into(),
    };
    let basic = CredentialInput::Basic {
        username: "distinct-user".into(),
        password: "distinct-password".into(),
    };
    for forge in [Forge::GitHub, Forge::GitLab, Forge::Gitea] {
        assert!(credential(forge, Some(&token)).unwrap().is_some());
        assert!(matches!(
            credential(forge, Some(&basic)),
            Err(FetchError::CredentialKind)
        ));
    }
    assert!(credential(Forge::Static, Some(&basic)).unwrap().is_some());
    assert!(matches!(
        credential(Forge::Static, Some(&token)),
        Err(FetchError::CredentialKind)
    ));
    for (forge, value, _, _) in cases() {
        let guidance = credential_guidance(&detected(value, forge)).unwrap();
        assert_eq!(guidance.url.scheme(), "https");
        assert!(!guidance.scope.contains("write"));
    }
    assert_eq!(
        credential_guidance(&detected("alice/catalog", Forge::GitHub))
            .unwrap()
            .url
            .as_str(),
        "https://github.com/settings/personal-access-tokens/new"
    );
    assert_eq!(
        credential_guidance(&detected(
            "https://gitlab.com/acme/team/catalog",
            Forge::GitLab
        ))
        .unwrap()
        .scope,
        "read_api only; grant access only to the required project."
    );
    assert_eq!(status(404), Err(FetchError::Unavailable));
    assert!(
        !FetchError::Unavailable
            .to_string()
            .contains("repository exists")
    );
}

#[test]
fn tag_traversal_is_encoded_or_refused_never_normalized_to_another_endpoint() {
    for tag in [".", ".."] {
        assert_eq!(
            github::release_url(&detected(&format!("alice/catalog@{tag}"), Forge::GitHub)),
            Err(FetchError::InvalidMetadata)
        );
    }
    assert_eq!(
        github::release_url(&detected("alice/catalog@v1/../../latest", Forge::GitHub))
            .unwrap()
            .as_str(),
        "https://api.github.com/repos/alice/catalog/releases/tags/v1%2F..%2F..%2Flatest"
    );
}
