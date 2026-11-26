//! Development-only access to the production broker credential adapter.
use crate::plugin::PluginHost;
use axum::extract::Request;
use std::{io, net::SocketAddr, path::PathBuf, sync::Arc};

/// Publishes the real private descriptor and attaches the same credential object
/// that production listener wiring installs. No authentication bypass is used.
#[derive(Clone)]
pub struct ActionBroker(Arc<crate::server::broker::BrokerToken>);
impl ActionBroker {
    pub fn publish(path: PathBuf, address: SocketAddr, host: &Arc<PluginHost>) -> io::Result<Self> {
        let token = Arc::new(crate::server::broker::BrokerToken::new(path)?);
        token.set_cli_host(Arc::downgrade(host));
        token.publish(address)?;
        Ok(Self(token))
    }
    pub fn attach(&self, request: &mut Request) {
        request.extensions_mut().insert(Some(self.0.clone()));
    }
}
