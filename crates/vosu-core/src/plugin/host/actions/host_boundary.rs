//! Runtime implementation of the dependency-inverted action domain boundary.
use super::*;
use crate::actions::{InvocationHost, PreparedInvocation};

impl InvocationHost for PluginHost {
    fn prepare_action(
        &self,
        principal: Principal,
        action: Arc<ActionDescriptor>,
        session: Option<String>,
        arguments: Value,
        policy: InvocationPolicy,
    ) -> Result<PreparedInvocation, InvocationError> {
        PluginHost::prepare_action(self, principal, action, session, arguments, policy)
    }

    fn start_action(
        &self,
        invocation: InvocationId,
        provider: PluginInstanceId,
        payload: Vec<u8>,
        sink: Arc<dyn InvocationSink>,
        policy: InvocationPolicy,
    ) -> Result<(), InvocationError> {
        PluginHost::start_action(
            self,
            invocation,
            provider,
            OwnedMessage {
                opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_ACTION_INVOKE as u32,
                flags: 0,
                correlation_id: 0,
                payload,
            },
            sink,
            policy,
        )
    }

    fn cancel_action(&self, invocation: InvocationId) {
        PluginHost::cancel_action(self, invocation);
    }

    fn discard_prepared_action(&self, invocation: InvocationId) {
        PluginHost::discard_prepared_action(self, invocation);
    }
}
