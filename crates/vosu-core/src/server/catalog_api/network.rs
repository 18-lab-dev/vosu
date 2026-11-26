//! Network effects of the catalog routes: explicit source refresh and listed
//! artwork. Production goes through K0's `CatalogRuntime::refresh_sources`
//! (S0/C0/F0/N0: typed purposes, scoped credentials, private-host policy) and
//! the strict public-only, credential-free client for art.
//!
//! A `test-harness` build (rejected in release builds by `build.rs`) with
//! `VOSU_CATALOG_TEST_SOCKET=<loopback socket>` instead serves static
//! `https://catalog.invalid:<port>/…/catalog-v2.json` sources, their package
//! URLs and art from that exact loopback listener over plain HTTP. The
//! fetched bytes still pass through S0's one SDK-validated `complete_fetch`
//! publication; forge, credential and TLS behaviour is covered by the real
//! network suites, never by this harness.
use super::ProviderFuture;
use crate::catalog::runtime::{CatalogError, CatalogRuntime};
use axum::body::Bytes;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use vosu_net::{Deadlines, DownloadLimits, StreamingClient};

/// One catalog art image, at most 1 MiB.
pub(super) const MAX_ART_BYTES: u64 = 1024 * 1024;

pub trait CatalogNetwork: Send + Sync {
    /// One explicit refresh, published through S0 and followed by G0 refresh.
    fn refresh<'a>(
        &'a self,
        runtime: &'a CatalogRuntime,
        cancel: &'a CancellationToken,
    ) -> ProviderFuture<'a, Result<(), CatalogError>>;
    /// A listed art URL, credential-free and public-only.
    fn art<'a>(
        &'a self,
        url: &'a str,
        cancel: &'a CancellationToken,
    ) -> ProviderFuture<'a, Result<(u16, Bytes), String>>;
}

/// The production network, or the test harness when a `test-harness` build
/// names its exact loopback listener.
pub fn network() -> Arc<dyn CatalogNetwork> {
    #[cfg(feature = "test-harness")]
    if let Some(socket) = crate::catalog::sources::harness_socket() {
        return Arc::new(harness::Harness(socket));
    }
    Arc::new(Production)
}

struct Production;
impl CatalogNetwork for Production {
    fn refresh<'a>(
        &'a self,
        runtime: &'a CatalogRuntime,
        cancel: &'a CancellationToken,
    ) -> ProviderFuture<'a, Result<(), CatalogError>> {
        Box::pin(async move {
            runtime
                .refresh_sources(&StreamingClient::production(), cancel)
                .await
                .map(|_| ())
        })
    }
    fn art<'a>(
        &'a self,
        url: &'a str,
        cancel: &'a CancellationToken,
    ) -> ProviderFuture<'a, Result<(u16, Bytes), String>> {
        Box::pin(async move {
            StreamingClient::production()
                .with_deadlines_and_limits(Deadlines::default(), art_limits())
                .request_bytes(url, "vosu-catalog-art", cancel)
                .await
                .map(|response| (response.status, response.body))
                .map_err(|error| error.to_string())
        })
    }
}

fn art_limits() -> DownloadLimits {
    DownloadLimits {
        max_bytes: MAX_ART_BYTES,
        max_redirects: 5,
    }
}

#[cfg(feature = "test-harness")]
pub use harness::transport as harness_transport;

#[cfg(feature = "test-harness")]
mod harness {
    use super::*;
    use crate::catalog::{
        install::{DownloadFuture, PackageTransport, PublicPackageTransport},
        sources::{
            FetchedCatalog, HARNESS_HOST as HOST, SourceFailure, SourceIdentity, SourceLocation,
            harness_socket,
        },
    };
    use std::{net::SocketAddr, path::Path};

    fn client(socket: SocketAddr, limits: DownloadLimits) -> StreamingClient {
        struct Resolver(std::net::IpAddr);
        impl vosu_net::AddressResolver for Resolver {
            fn resolve<'a>(&'a self, _: &'a str) -> vosu_net::ResolverFuture<'a> {
                Box::pin(async move { Ok(vec![self.0]) })
            }
        }
        let network = vosu_net::TestNetworkConfig::new(
            Arc::new(Resolver(socket.ip())),
            vec![HOST.into()],
            vec![socket],
        )
        .expect("validated harness listener");
        StreamingClient::for_test_network(network, Deadlines::default(), limits)
    }

    /// Only the scheme of a URL on the reserved harness host is rewritten.
    fn plaintext(url: &str) -> String {
        url.replacen(&format!("https://{HOST}:"), &format!("http://{HOST}:"), 1)
    }

    pub(super) struct Harness(pub(super) SocketAddr);
    impl CatalogNetwork for Harness {
        fn refresh<'a>(
            &'a self,
            runtime: &'a CatalogRuntime,
            cancel: &'a CancellationToken,
        ) -> ProviderFuture<'a, Result<(), CatalogError>> {
            Box::pin(async move {
                let fetch = runtime.fetch().ok_or(CatalogError::Disabled)?;
                let ticket = fetch.sources().begin_fetch()?;
                let client = client(
                    self.0,
                    DownloadLimits {
                        max_bytes: vosu_plugin_api::catalog::MAX_CATALOG_BYTES as u64,
                        max_redirects: 5,
                    },
                );
                let mut fetched = Vec::new();
                for record in ticket.sources().iter().filter(|record| record.enabled) {
                    let url = match SourceIdentity::parse(record.identity.as_str()) {
                        Ok(parsed) => match parsed.location {
                            SourceLocation::Static { url } if url.host_str() == Some(HOST) => {
                                Some(plaintext(url.as_str()))
                            }
                            _ => None,
                        },
                        Err(_) => None,
                    };
                    let result = match url {
                        Some(url) => match client.request_bytes(&url, "vosu-catalog", cancel).await
                        {
                            Ok(response) if response.status == 200 => Ok(response.body.to_vec()),
                            _ => Err(SourceFailure::Unavailable),
                        },
                        None => Err(SourceFailure::Unavailable),
                    };
                    fetched.push(FetchedCatalog {
                        source: record.identity.clone(),
                        result,
                    });
                }
                let inputs = runtime.registration_inputs();
                let facts = runtime.host_facts();
                let published = fetch.sources().complete_fetch(
                    ticket,
                    fetched,
                    &inputs.registrations(),
                    &inputs.revisions(),
                    &facts.application,
                    &facts.target,
                );
                runtime.refresh_offers();
                published.map(|_| ()).map_err(CatalogError::from)
            })
        }
        fn art<'a>(
            &'a self,
            url: &'a str,
            cancel: &'a CancellationToken,
        ) -> ProviderFuture<'a, Result<(u16, Bytes), String>> {
            Box::pin(async move {
                client(self.0, art_limits())
                    .request_bytes(&plaintext(url), "vosu-catalog-art", cancel)
                    .await
                    .map(|response| (response.status, response.body))
                    .map_err(|error| error.to_string())
            })
        }
    }

    struct Transport(PublicPackageTransport);
    impl PackageTransport for Transport {
        fn download<'a>(
            &'a self,
            url: &'a str,
            max_bytes: u64,
            destination: &'a Path,
            cancel: &'a CancellationToken,
        ) -> DownloadFuture<'a> {
            let url = plaintext(url);
            Box::pin(async move { self.0.download(&url, max_bytes, destination, cancel).await })
        }
    }

    /// Package transport for the harness listener; `None` without it.
    pub fn transport() -> Option<Arc<dyn PackageTransport>> {
        let socket = harness_socket()?;
        Some(Arc::new(Transport(PublicPackageTransport::new(client(
            socket,
            DownloadLimits::default(),
        )))))
    }
}
