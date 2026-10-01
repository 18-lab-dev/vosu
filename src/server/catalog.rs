//! Hardened remote counter catalog loading and source-compatible local merge.

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::net::{Deadlines, DownloadLimits, NetworkError, StreamingClient};

pub const CATALOG_ENDPOINT: &str = "https://vosu.app/api.json";
pub const CATALOG_MAX_BYTES: u64 = 1024 * 1024 * 1024;
pub const CATALOG_MAX_REDIRECTS: usize = 5;

pub fn catalog_user_agent(version: &str) -> String {
    format!("vosu/{version} (https://vosu.app; i@kotrik.ru)")
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CatalogOverlay {
    #[serde(rename = "_downloaded", default, skip_serializing_if = "is_false")]
    pub downloaded: bool,
    #[serde(rename = "_updatable", default, skip_serializing_if = "is_false")]
    pub updatable: bool,
    #[serde(rename = "_settings")]
    pub has_settings: bool,
    #[serde(rename = "folderName", skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    pub name: String,
    pub author: String,
    pub version: Value,
    pub resolution: Vec<Value>,
    pub authorlinks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usecase: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatiblewith: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<CatalogAsset>>,
    #[serde(rename = "downloadLink", skip_serializing_if = "Option::is_none")]
    pub download_link: Option<String>,
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// The remote API is intentionally looser than locally discovered counter
/// metadata: it currently omits `folderName` and `settings`, emits resolution
/// components as strings, and models `usecase` as an array.
#[derive(Debug, Deserialize)]
struct RemoteCatalogOverlay {
    #[serde(rename = "_settings", default)]
    has_settings: bool,
    #[serde(rename = "folderName", default)]
    folder_name: Option<String>,
    name: String,
    author: String,
    version: Value,
    #[serde(default)]
    resolution: Vec<Value>,
    #[serde(default)]
    authorlinks: Vec<String>,
    #[serde(default)]
    settings: Option<Vec<Value>>,
    #[serde(default)]
    usecase: Option<Value>,
    #[serde(default)]
    compatiblewith: Option<Vec<String>>,
    #[serde(default)]
    assets: Option<Vec<CatalogAsset>>,
    #[serde(rename = "downloadLink", default)]
    download_link: Option<String>,
    #[serde(flatten)]
    extra: IndexMap<String, Value>,
}

impl From<RemoteCatalogOverlay> for CatalogOverlay {
    fn from(remote: RemoteCatalogOverlay) -> Self {
        Self {
            downloaded: false,
            updatable: false,
            has_settings: remote.has_settings,
            folder_name: remote.folder_name,
            name: remote.name,
            author: remote.author,
            version: remote.version,
            resolution: remote.resolution,
            authorlinks: remote.authorlinks,
            settings: remote.settings,
            usecase: remote.usecase,
            compatiblewith: remote.compatiblewith,
            assets: remote.assets,
            download_link: remote.download_link,
            extra: remote.extra,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CatalogAsset {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalOverlay {
    pub name: String,
    pub author: String,
    pub version: String,
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error(transparent)]
    Network(#[from] NetworkError),
    #[error("catalog request timed out")]
    Timeout,
    #[error("catalog response status {0}")]
    Status(u16),
    #[error("catalog response exceeds {CATALOG_MAX_BYTES} bytes")]
    TooLarge,
    #[error("catalog JSON is malformed: {0}")]
    Malformed(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct CatalogService {
    client: StreamingClient,
    endpoint: Arc<str>,
    user_agent: Arc<str>,
}

impl CatalogService {
    pub fn production(version: &str) -> Self {
        Self::new(StreamingClient::production(), CATALOG_ENDPOINT, version)
    }

    /// Construct with an injected hardened client and endpoint. Production code
    /// passes `StreamingClient::production`; the feature-gated test harness may
    /// pass only its constrained reserved-host/exact-socket client.
    pub fn new(client: StreamingClient, endpoint: impl Into<Arc<str>>, version: &str) -> Self {
        let client = client.with_deadlines_and_limits(
            Deadlines::default(),
            DownloadLimits {
                max_bytes: CATALOG_MAX_BYTES,
                max_redirects: CATALOG_MAX_REDIRECTS,
            },
        );
        Self {
            client,
            endpoint: endpoint.into(),
            user_agent: catalog_user_agent(version).into(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub async fn fetch(
        &self,
        local: &[LocalOverlay],
        cancel: &CancellationToken,
    ) -> Result<Vec<CatalogOverlay>, CatalogError> {
        let response = self
            .client
            .request_bytes(&self.endpoint, &self.user_agent, cancel)
            .await
            .map_err(map_network_error)?;
        let remote = decode_response(response.status, response.body.as_ref())?;
        Ok(merge_local(remote, local))
    }
}

fn map_network_error(error: NetworkError) -> CatalogError {
    match error {
        NetworkError::TooLarge(_) => CatalogError::TooLarge,
        NetworkError::Timeout => CatalogError::Timeout,
        other => CatalogError::Network(other),
    }
}

/// Validate the complete response before decoding. This is public so the HTTP
/// harness can exercise status/malformed/size behavior without any live network.
pub fn decode_response(status: u16, body: &[u8]) -> Result<Vec<CatalogOverlay>, CatalogError> {
    validate_response_length(body.len() as u64)?;
    if !(200..300).contains(&status) {
        return Err(CatalogError::Status(status));
    }
    let remote: Vec<RemoteCatalogOverlay> = serde_json::from_slice(body)?;
    Ok(remote.into_iter().map(CatalogOverlay::from).collect())
}

pub fn validate_response_length(length: u64) -> Result<(), CatalogError> {
    if length > CATALOG_MAX_BYTES {
        Err(CatalogError::TooLarge)
    } else {
        Ok(())
    }
}

/// Annotate remote entries in their original order, exactly as the source page
/// does. Identity matching is case-sensitive `(name, author)`; version comparison
/// is case-insensitive. Local-only counters are not inserted into the available
/// catalog.
pub fn merge_local(mut remote: Vec<CatalogOverlay>, local: &[LocalOverlay]) -> Vec<CatalogOverlay> {
    for overlay in &mut remote {
        if let Some(installed) = local
            .iter()
            .find(|installed| installed.name == overlay.name && installed.author == overlay.author)
        {
            overlay.downloaded = true;
            overlay.updatable = catalog_version(&overlay.version)
                .is_some_and(|version| !version.eq_ignore_ascii_case(&installed.version));
        } else {
            overlay.downloaded = false;
            overlay.updatable = false;
        }
    }
    remote
}

fn catalog_version(version: &Value) -> Option<String> {
    match version {
        Value::String(version) if !version.is_empty() => Some(version.clone()),
        Value::Number(version) => Some(version.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(entries: Value) -> Vec<u8> {
        serde_json::to_vec(&entries).unwrap()
    }

    fn valid(name: &str, author: &str, version: &str, folder: &str) -> Value {
        serde_json::json!({
            "folderName": folder,
            "name": name,
            "author": author,
            "version": version,
            "resolution": [640, 480],
            "authorlinks": ["https://example.invalid"],
            "settings": [],
            "futureProperty": {"preserved": true}
        })
    }

    // Captured upstream tosu catalog input intentionally verifies legacy source compatibility.
    const CAPTURED_UPSTREAM_TOSU_APP_ENTRY: &str = r#"[
      {
        "_settings": true,
        "usecase": ["ingame", "obs-overlay"],
        "id": "307ba23a-ac0f-461e-83ce-8f34586c9158",
        "name": "OffsetSuggester",
        "version": "1.0",
        "author": "Pinossaur",
        "authorlinks": ["https://github.com/Pinossaur"],
        "compatiblewith": ["tosu"],
        "resolution": ["350", "230"],
        "assets": [{"type":"image","url":"https://example.invalid/preview.png"}],
        "downloadLink": "https://example.invalid/counter.zip"
      }
    ]"#;

    #[test]
    fn valid_catalog_preserves_order_and_unknown_properties() {
        let bytes = body(serde_json::json!([
            valid("Second", "B", "2", "second"),
            valid("First", "A", "1", "first")
        ]));
        let parsed = decode_response(200, &bytes).unwrap();
        assert_eq!(
            parsed
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["Second", "First"]
        );
        assert_eq!(parsed[0].extra["futureProperty"]["preserved"], true);
    }

    #[test]
    fn captured_remote_wire_shape_accepts_optional_and_value_shaped_fields() {
        let parsed = decode_response(200, CAPTURED_UPSTREAM_TOSU_APP_ENTRY.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].folder_name, None);
        assert_eq!(parsed[0].settings, None);
        assert_eq!(
            parsed[0].resolution,
            vec![Value::String("350".into()), Value::String("230".into())]
        );
        assert_eq!(
            parsed[0].usecase,
            Some(serde_json::json!(["ingame", "obs-overlay"]))
        );
        assert_eq!(
            parsed[0].extra["id"],
            "307ba23a-ac0f-461e-83ce-8f34586c9158"
        );
    }

    #[test]
    fn status_malformed_shape_and_size_are_terminal_errors() {
        assert!(matches!(
            decode_response(503, b"[]"),
            Err(CatalogError::Status(503))
        ));
        assert!(matches!(
            decode_response(200, b"{"),
            Err(CatalogError::Malformed(_))
        ));
        assert!(matches!(
            decode_response(200, b"{}"),
            Err(CatalogError::Malformed(_))
        ));
        assert!(validate_response_length(CATALOG_MAX_BYTES).is_ok());
        assert!(matches!(
            validate_response_length(CATALOG_MAX_BYTES + 1),
            Err(CatalogError::TooLarge)
        ));
    }

    #[test]
    fn local_merge_is_deterministic_and_source_compatible() {
        let bytes = body(serde_json::json!([
            valid("Same", "Author", "V2", "same"),
            valid("Case", "author", "1", "case"),
            valid("Absent", "Nobody", "1", "absent")
        ]));
        let remote = decode_response(200, &bytes).unwrap();
        let local = vec![
            LocalOverlay {
                name: "Case".into(),
                author: "Author".into(),
                version: "0".into(),
            },
            LocalOverlay {
                name: "Same".into(),
                author: "Author".into(),
                version: "v2".into(),
            },
            LocalOverlay {
                name: "local only".into(),
                author: "Nobody".into(),
                version: "1".into(),
            },
        ];
        let merged = merge_local(remote, &local);
        assert_eq!(
            merged
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["Same", "Case", "Absent"]
        );
        assert!(merged[0].downloaded);
        assert!(
            !merged[0].updatable,
            "version comparison is case-insensitive"
        );
        assert!(
            !merged[1].downloaded,
            "identity comparison is case-sensitive"
        );
        assert!(!merged[2].downloaded);
    }

    #[test]
    fn required_catalog_fields_are_not_silently_defaulted() {
        let bytes = body(serde_json::json!([{"name":"incomplete"}]));
        assert!(matches!(
            decode_response(200, &bytes),
            Err(CatalogError::Malformed(_))
        ));
    }
    #[cfg(feature = "test-harness")]
    mod constrained_network {
        use std::io;
        use std::net::{IpAddr, Ipv4Addr};
        use std::sync::Arc;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        use crate::net::{AddressResolver, ResolverFuture, TestNetworkConfig};

        use super::*;

        struct LoopbackResolver;

        impl AddressResolver for LoopbackResolver {
            fn resolve<'a>(&'a self, _host: &'a str) -> ResolverFuture<'a> {
                Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
            }
        }

        async fn constrained_service(
            status: u16,
            response_body: Vec<u8>,
            advertised_length: Option<u64>,
        ) -> (CatalogService, tokio::task::JoinHandle<String>) {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let socket = listener.local_addr().unwrap();
            let request = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..read]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let length = advertised_length.unwrap_or(response_body.len() as u64);
                let reason = if status == 200 { "OK" } else { "Failure" };
                let headers = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(&response_body).await.unwrap();
                String::from_utf8(bytes).unwrap()
            });
            let config = TestNetworkConfig::new(
                Arc::new(LoopbackResolver),
                vec!["catalog.invalid".to_owned()],
                vec![socket],
            )
            .unwrap();
            let client = StreamingClient::for_test_network(
                config,
                Deadlines::default(),
                DownloadLimits::default(),
            );
            let service = CatalogService::new(
                client,
                format!("http://catalog.invalid:{}/api.json", socket.port()),
                "0.1.0",
            );
            (service, request)
        }

        #[tokio::test]
        async fn constrained_harness_covers_valid_status_malformed_and_size() -> io::Result<()> {
            let valid_body = body(serde_json::json!([valid("A", "B", "1", "a")]));
            let (service, request) = constrained_service(200, valid_body, None).await;
            let result = service.fetch(&[], &CancellationToken::new()).await.unwrap();
            assert_eq!(result.len(), 1);
            let request = request.await.unwrap();
            assert!(request.contains("user-agent: vosu/0.1.0 (https://vosu.app; i@kotrik.ru)"));

            let (service, request) = constrained_service(503, b"[]".to_vec(), None).await;
            assert!(matches!(
                service.fetch(&[], &CancellationToken::new()).await,
                Err(CatalogError::Status(503))
            ));
            request.await.unwrap();

            let (service, request) = constrained_service(200, b"{".to_vec(), None).await;
            assert!(matches!(
                service.fetch(&[], &CancellationToken::new()).await,
                Err(CatalogError::Malformed(_))
            ));
            request.await.unwrap();

            let (service, request) =
                constrained_service(200, Vec::new(), Some(CATALOG_MAX_BYTES + 1)).await;
            assert!(matches!(
                service.fetch(&[], &CancellationToken::new()).await,
                Err(CatalogError::TooLarge)
            ));
            request.await.unwrap();
            Ok(())
        }
    }
}
