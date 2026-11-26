//! Invocation-owned artifacts (AF.12, `docs/v2/plugin-abi.md` §6.13): the
//! ABI side of `ARTIFACT_DECLARE`/`DECLARED`/`OPEN`/`RELEASE`.
//!
//! A declaration is checked against the live invocation it names: the
//! declaring instance must be that invocation's provider, the invocation
//! must still be running, and its caller must be a public job (`tool` or
//! `cli`), never a relayed `ACTION_CALL`. Each accepted artifact reserves
//! its eventual `ARTIFACT_RELEASE` delivery at once, so release never
//! competes for capacity. Opening a download is a sessionless request on
//! the ordinary resource-request table: a plugin-wide root path or a
//! request-owned stream lease, never a session handle.

use std::sync::Arc;
use std::time::Duration;

use vosu_plugin_api::{
    ArtifactDeclare, ArtifactDeclared, ArtifactOpen, ArtifactRelease, CborValue, ErrorInfo,
    Message, ResourceResolved, is_artifact_name, vosu_status,
};

use super::resource_requests::{
    LeaseOwner, ResourceReply, ResourceRequest, ResourceRequestError,
};
use super::resources::RootCapability;
use super::{InstanceSlot, OwnedMessage, PluginHost};
use crate::actions::{ArtifactDeclaration, ArtifactHost, ArtifactRefusal};
use crate::identity::PluginInstanceId;
use crate::plugin::StreamLease;
use crate::plugin::outbox::SlotClass;

/// What one `ARTIFACT_OPEN` resolved to.
pub enum ArtifactSource {
    /// A path inside one of the provider's plugin-wide declared roots,
    /// still to be opened with retained-handle confinement.
    File { root: RootCapability, path: String },
    /// A request-owned stream lease of the provider's advertised size.
    Stream(StreamLease),
}

impl ArtifactHost for PluginHost {
    fn provider_live(&self, provider: PluginInstanceId) -> bool {
        matches!(
            self.instances
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&provider),
            Some(InstanceSlot::Live(_))
        )
    }

    fn expire_after(&self, delay: Duration, task: Box<dyn FnOnce() + Send>) {
        self.state_runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            task();
        });
    }
}

impl PluginHost {
    /// `VOSU_OP_ARTIFACT_DECLARE` from `instance`. The reply goes through a
    /// slot reserved before anything is recorded, so an accepted artifact
    /// always reaches its plugin's `ARTIFACT_DECLARED`.
    pub(super) fn handle_artifact_declare(
        &self,
        instance: PluginInstanceId,
        correlation: u64,
        declare: ArtifactDeclare,
    ) -> vosu_status {
        if correlation == 0 {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let Ok(reply_slot) = self.reserve_outbox(instance) else {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        };
        let reply = match self.declare_artifact(instance, declare) {
            Ok(artifact) => ArtifactDeclared::Artifact { artifact },
            Err(refusal) => ArtifactDeclared::Error {
                error: ErrorInfo {
                    code: refusal.code().into(),
                    message: refusal_message(refusal).into(),
                },
            },
        };
        match OwnedMessage::from_message(&Message::ArtifactDeclared(reply), correlation) {
            Ok(message) => {
                drop(reply_slot.send(message));
                vosu_status::VOSU_OK
            }
            Err(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        }
    }

    fn declare_artifact(
        &self,
        instance: PluginInstanceId,
        declare: ArtifactDeclare,
    ) -> Result<String, ArtifactRefusal> {
        if !is_artifact_name(&declare.name) {
            return Err(ArtifactRefusal::InvalidName);
        }
        // A finished, timed-out, unknown or other provider's invocation
        // answers alike, so a stale id reveals nothing about another caller.
        let sink = {
            let state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            let pending = state
                .pending
                .get(&declare.invocation)
                .filter(|pending| pending.action.provider == instance)
                .ok_or(ArtifactRefusal::InvocationFinished)?;
            if pending.caller.is_some() {
                return Err(ArtifactRefusal::CallerNotSupported);
            }
            Arc::clone(
                pending
                    .sink
                    .as_ref()
                    .ok_or(ArtifactRefusal::InvocationFinished)?,
            )
        };
        let release = self
            .reserve_work_result(instance, SlotClass::ArtifactRelease)
            .map_err(|_| ArtifactRefusal::TooManyArtifacts)?;
        let media_type = crate::resources::artifact_type(&declare.name);
        let host: std::sync::Weak<dyn ArtifactHost> = self.self_ref.clone();
        let key = declare.key.clone();
        let declaration = ArtifactDeclaration {
            provider: instance,
            host,
            key: declare.key,
            name: declare.name,
            media_type,
            size: declare.size,
            release: Box::new(move |artifact| {
                let message = Message::ArtifactRelease(ArtifactRelease {
                    artifact: artifact.to_owned(),
                    key,
                });
                // The key already fit in its declaration's frame.
                if let Ok(message) = OwnedMessage::from_message(&message, 0) {
                    drop(release.send(message));
                }
            }),
        };
        sink.declare_artifact(declaration)
    }

    /// One `ARTIFACT_OPEN` of `artifact` to `provider`, bounded by
    /// `deadline`. A stream lease closes when dropped; a path must name a
    /// plugin-wide declared root and is opened by the caller.
    pub async fn open_artifact(
        &self,
        provider: PluginInstanceId,
        artifact: &str,
        key: CborValue,
        deadline: Duration,
    ) -> Result<ArtifactSource, ResourceRequestError> {
        let reply = self
            .resource_requests(deadline)
            .request(
                LeaseOwner::Artifact(provider),
                ResourceRequest::ArtifactOpen(ArtifactOpen {
                    artifact: artifact.to_owned(),
                    key,
                }),
            )
            .await?;
        match reply {
            ResourceReply::Resolved(ResourceResolved::Path { root, path }) => {
                Ok(ArtifactSource::File {
                    root: self.plugin_root(provider, &root)?,
                    path,
                })
            }
            ResourceReply::Stream {
                lease,
                entry_path: None,
            } => Ok(ArtifactSource::Stream(lease)),
            _ => Err(ResourceRequestError::InvalidReply),
        }
    }
}

fn refusal_message(refusal: ArtifactRefusal) -> &'static str {
    match refusal {
        ArtifactRefusal::InvocationFinished => "The invocation is not running",
        ArtifactRefusal::CallerNotSupported => "Only a tool or CLI job owns artifacts",
        ArtifactRefusal::TooManyArtifacts => "Artifact capacity reached",
        ArtifactRefusal::InvalidName => "The artifact name is not one safe file name",
    }
}
