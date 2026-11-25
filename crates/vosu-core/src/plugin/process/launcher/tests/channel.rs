//! A [`ProcessLauncher`] whose children the test plays over a real channel
//! (memfd/eventfd or section/event): core's real supervision, ingress and
//! endpoint, with a scripted process handle, so a real `PluginHost` can be
//! driven frame by frame (M5 B0's host-level broker tests).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use vosu_plugin_api::{Message, vosu_status};

use super::super::super::channel::Endpoint;
use super::super::super::control::{Create, Ready};
use super::super::super::frame::{FrameHeader, RequestKind, control as op};
use super::super::super::ring::PayloadBuffer;
use super::super::platform::Started;
use super::super::supervision::{self, Start};
use super::{ScriptedProcess, pair};
use crate::plugin::error::HostError;
use crate::plugin::execution::{ProcessEndpoint, ProcessLaunch, ProcessLauncher};

const BOUND: Duration = Duration::from_secs(20);

/// Launches test-played children, each with this `SYNC` budget.
pub(crate) struct ChannelLauncher {
    sync_budget: Duration,
    children: Mutex<VecDeque<ChannelChild>>,
    launched: Condvar,
}

impl ChannelLauncher {
    pub(crate) fn new(sync_budget: Duration) -> Arc<Self> {
        Arc::new(Self {
            sync_budget,
            children: Mutex::new(VecDeque::new()),
            launched: Condvar::new(),
        })
    }

    /// The oldest launched child not yet taken.
    pub(crate) fn take_child(&self) -> ChannelChild {
        let children = self.children.lock().unwrap();
        let (mut children, _) = self
            .launched
            .wait_timeout_while(children, BOUND, |children| children.is_empty())
            .unwrap();
        children
            .pop_front()
            .expect("a child launched within the bound")
    }
}

impl ProcessLauncher for ChannelLauncher {
    fn launch(&self, launch: ProcessLaunch<'_>) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        let (core, child) = pair();
        let process = Arc::new(ScriptedProcess::default());
        let create = Create {
            generation: 1,
            plugin_id: launch.manifest.id.clone(),
            library: launch.package.library().to_owned(),
            library_sha256: [0; 32],
            concurrent_dispatch: launch.manifest.concurrent_dispatch,
            sync_budget_ms: u32::try_from(self.sync_budget.as_millis()).unwrap(),
            init: vosu_plugin_api::decode(launch.init).unwrap(),
        }
        .encode()
        .unwrap();
        let start = Start {
            generation: 1,
            plugin_id: launch.manifest.id.clone(),
            create,
            deadline: Instant::now() + BOUND,
            ingress: launch.ingress,
            loss: launch.loss,
            sync_budget: self.sync_budget,
        };
        let started = Started {
            endpoint: core,
            process: process.clone(),
        };
        let supervising = std::thread::spawn(move || supervision::start(started, start));
        let mut child = ChannelChild {
            endpoint: child,
            process,
            buffer: PayloadBuffer::new(),
            next_request: 0,
        };
        let (header, _) = child.recv();
        assert_eq!(header.opcode, op::CREATE);
        let ready = Ready {
            generation: 1,
            struct_size: crate::plugin::execution::descriptor::descriptor_minimum() as u32,
            abi_version: 1,
        };
        child
            .endpoint
            .sender
            .send(
                &FrameHeader {
                    opcode: op::READY,
                    flags: 0,
                    correlation: 0,
                },
                &ready.encode(),
            )
            .unwrap();
        let endpoint = supervising
            .join()
            .unwrap()
            .map_err(HostError::ProcessStartupFailed)?;
        self.children.lock().unwrap().push_back(child);
        self.launched.notify_all();
        Ok(endpoint)
    }
}

/// The test's side of one launched child.
pub(crate) struct ChannelChild {
    endpoint: Endpoint,
    process: Arc<ScriptedProcess>,
    buffer: PayloadBuffer,
    next_request: u64,
}

impl ChannelChild {
    /// Sends `payload` as an acknowledged plugin → host frame and returns
    /// its request id, as the child's `send` does.
    pub(crate) fn send_raw(
        &mut self,
        opcode: u32,
        flags: u32,
        correlation: u64,
        payload: &[u8],
    ) -> u64 {
        self.next_request += 1;
        let request = self.next_request;
        let sync = flags & vosu_plugin_api::VOSU_READ_PLAN_SYNC != 0;
        let kind = if sync {
            RequestKind::Sync
        } else {
            RequestKind::Status
        };
        assert!(self.endpoint.ledger.open(request, kind));
        let header = FrameHeader {
            opcode,
            flags,
            correlation,
        };
        self.endpoint
            .sender
            .send_acknowledged(request, &header, payload)
            .unwrap();
        request
    }

    /// [`Self::send_raw`] of `message`'s encoding.
    pub(crate) fn send(&mut self, message: &Message, correlation: u64) -> u64 {
        let payload = message.encode_payload().unwrap();
        self.send_raw(
            message.opcode() as u32,
            message.flags(),
            correlation,
            &payload,
        )
    }

    /// The next frame core wrote, with its payload.
    pub(crate) fn recv(&mut self) -> (FrameHeader, Vec<u8>) {
        let header = self
            .endpoint
            .receiver
            .recv_until(&mut self.buffer, Some(Instant::now() + BOUND))
            .unwrap()
            .expect("core wrote a frame within the bound");
        (header, self.buffer.bytes().to_vec())
    }

    /// The next frame, which must be a `STATUS`: (request, status). The
    /// request is released, as the child's waiter is.
    pub(crate) fn status(&mut self) -> (u64, vosu_status) {
        let (header, payload) = self.recv();
        assert_eq!(
            header.opcode,
            op::STATUS,
            "expected a STATUS, got {header:?}"
        );
        self.endpoint.ledger.close(header.correlation);
        let raw = u32::from_le_bytes(payload[..4].try_into().unwrap());
        (header.correlation, vosu_status::try_from(raw).unwrap())
    }

    /// The next two frames, which must be `REPLY_NEXT` for a request and
    /// its `READ_PLAN_RESULT`: (request, result payload).
    pub(crate) fn sync_reply(&mut self) -> (u64, Vec<u8>) {
        let (header, _) = self.recv();
        assert_eq!(
            header.opcode,
            op::REPLY_NEXT,
            "expected REPLY_NEXT, got {header:?}"
        );
        self.endpoint.ledger.close(header.correlation);
        let (result, payload) = self.recv();
        assert_eq!(
            result.opcode,
            vosu_plugin_api::vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32
        );
        (header.correlation, payload)
    }

    /// The scripted OS exit of this child.
    pub(crate) fn exit(&self, status: i64) {
        self.process.exit(status);
    }
}
