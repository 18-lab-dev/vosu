//! One job-artifact download's bytes (AF.12): the plugin's `ARTIFACT_OPEN`
//! answer opened as the same retained-root file or request-owned stream
//! lease a resource uses. Nothing is cached: every download opens afresh
//! and its file handle or lease ends with it.
use super::ResolvedResource;
use super::resolve::open_root_file;
use crate::plugin::{ArtifactSource, PluginHost, PluginInstanceId, ResourceRequestError};
use std::time::Duration;
use vosu_plugin_api::CborValue;

/// Opens `artifact` of `provider` for one download, bounded by `deadline`
/// for the provider's answer. The caller checks the opened size against
/// the declared size before serving any byte.
pub async fn open_artifact(
    host: &PluginHost,
    provider: PluginInstanceId,
    artifact: &str,
    key: CborValue,
    deadline: Duration,
) -> Result<ResolvedResource, ResourceRequestError> {
    match host
        .open_artifact(provider, artifact, key, deadline)
        .await?
    {
        ArtifactSource::File { root, path } => {
            open_root_file(root, &path).await.map(ResolvedResource::File)
        }
        ArtifactSource::Stream(lease) => Ok(ResolvedResource::Stream(lease)),
    }
}
