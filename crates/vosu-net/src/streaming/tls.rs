use super::{Duration, NetworkError, SocketAddr};

pub(super) fn pinned_client(
    host: &str,
    sockets: &[SocketAddr],
    connect: Duration,
) -> Result<reqwest::Client, NetworkError> {
    // Locked reqwest 0.13.4 `rustls` selects rustls-platform-verifier 0.7.1.
    // Its default verifier uses the OS store (native Windows verification;
    // rustls-native-certs on Linux), retaining the system's public roots.
    // Do not call tls_certs_only or install a fixture root here: X1 proves
    // installed internal CAs using this exact production builder.
    Ok(reqwest::Client::builder()
        .tls_backend_rustls()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect)
        .resolve_to_addrs(host, sockets)
        .build()?)
}
