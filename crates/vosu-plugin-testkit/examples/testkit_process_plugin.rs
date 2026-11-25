//! The M5 P0 testkit process plugin: a `runtime = "process"` `cdylib`
//! fixture that `tests/plugin_host_native` stages per test and loads in a
//! real `vosu plugin-host` child (`docs/testing/verification.md`, "M5 test
//! and fixture conventions").
//!
//! Every run is controlled by the control file staged next to the library
//! (`testkit_process_plugin/control.rs`), so two tests never share state and
//! nothing is keyed by PID. Each step writes a receipt naming the run's
//! nonce and this process's PID:
//!
//! - `constructor` (with the security readback on Linux) and `create`;
//! - `dispatch` for every delivered message (`opcode=`);
//! - `attach`/`read`/`detach` for sessions: each attached session gets one
//!   asynchronous read plan of the control's `read` range, and its result
//!   bytes are recorded, so two instances on distinct data-source targets
//!   prove which target each one read;
//! - `invoke`/`cancel` for actions, answered at once unless the run
//!   ignores cancellation;
//! - `destroy`.
//!
//! The fault modes are listed in [`control::Fault`]. Dispatch-time faults
//! fire once, on the first delivery of the control's `trigger` opcode, and
//! write their receipt (`crash`, `blocked`/`released`, `stall`, `corrupt`)
//! before acting.
//!
//! The library compiles its modules directly and never links the testkit
//! rlib, which exports the legacy echo plugin's entry symbol.

#[path = "testkit_process_plugin/control.rs"]
mod control;
#[path = "testkit_process_plugin/native.rs"]
mod native;

use control::{Control, Fault};
use vosu_plugin_api::{
    ActionResult, ActionStatus, HostServices, Init, Message, PlanBase, PlanOp, Plugin, PluginError,
    ReadResult, Stage, StageBase, VtableHostServices, vosu_status,
};

struct TestkitProcessPlugin {
    host: VtableHostServices,
    control: &'static Control,
    fired: bool,
}

impl Plugin for TestkitProcessPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let control = control::get().ok_or(PluginError(vosu_status::VOSU_ERR_INVALID_ARGUMENT))?;
        control.receipt("create", &format!("plugin={}", init.plugin_id));
        if control.fault == Fault::CreateCrash {
            std::process::abort();
        }
        Ok(Self {
            host,
            control,
            fired: false,
        })
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        let opcode = message.opcode() as u32;
        self.control
            .receipt("dispatch", &format!("opcode={opcode}"));
        if !self.fired
            && self.control.fault.fires_in_dispatch()
            && self.control.trigger == Some(opcode)
        {
            self.fired = true;
            self.fire(opcode);
            return Ok(());
        }
        match message {
            Message::SessionAttached(attached) => self.attach(attached.handle, attached.pid),
            Message::ReadPlanResult(result) => {
                let fields = match result.results.first() {
                    Some(ReadResult::Ok(bytes)) => format!("result=ok bytes={}", hex(bytes)),
                    Some(other) => format!("result={}", format!("{other:?}").replace(' ', "_")),
                    None => "result=empty".to_owned(),
                };
                self.control
                    .receipt("read", &format!("plan={} {fields}", result.plan));
            }
            Message::SessionDetached(detached) => {
                self.control
                    .receipt("detach", &format!("handle={}", detached.handle));
            }
            Message::ActionInvoke(invoke) => {
                self.control.receipt(
                    "invoke",
                    &format!("invocation={} action={}", invoke.invocation, invoke.action),
                );
                if self.control.fault != Fault::CancelIgnored {
                    self.finish(invoke.invocation, ActionStatus::Ok, Some(invoke.args), None);
                }
            }
            Message::ActionCancel(cancel) => {
                let ignored = self.control.fault == Fault::CancelIgnored;
                self.control.receipt(
                    "cancel",
                    &format!("invocation={} ignored={ignored}", cancel.invocation),
                );
                if !ignored {
                    self.finish(
                        cancel.invocation,
                        ActionStatus::Error,
                        None,
                        Some("cancelled"),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl TestkitProcessPlugin {
    /// Fires this run's dispatch-time fault. Returns only for the faults
    /// that leave the process running.
    fn fire(&self, opcode: u32) {
        match self.control.fault {
            Fault::DispatchCrash => {
                self.control.receipt("crash", &format!("opcode={opcode}"));
                std::process::abort();
            }
            Fault::DispatchBlock => {
                self.control.receipt("blocked", &format!("opcode={opcode}"));
                let released = self.control.wait_release();
                self.control
                    .receipt("released", &format!("opcode={opcode} released={released}"));
            }
            Fault::RingStall => {
                self.control.receipt("stall", &format!("opcode={opcode}"));
                native::stop_process();
            }
            Fault::CorruptFrame => {
                // The receipt precedes the forgery: after it, this process
                // sends nothing, so its own producer never overwrites it.
                self.control.receipt("corrupt", &format!("opcode={opcode}"));
                if let Err(error) = native::corrupt_plugin_to_host() {
                    self.control.receipt(
                        "corrupt_failed",
                        &format!("error={}", error.replace(' ', "_")),
                    );
                }
            }
            Fault::None
            | Fault::ConstructorCrash
            | Fault::CreateCrash
            | Fault::CancelIgnored
            | Fault::DestroyIgnored => {}
        }
    }

    fn attach(&self, handle: u64, pid: u32) {
        let identify = self.host.identify_session(
            handle,
            "testkit",
            "process",
            "Testkit process",
            "0.1.0",
            false,
        );
        let (address, length) = self.control.read;
        let submit = self.host.submit_read_plan(
            handle,
            handle,
            PlanBase::Absolute(address),
            vec![Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![PlanOp { offset: 0, length }],
            }],
        );
        self.control.receipt(
            "attach",
            &format!(
                "handle={handle} target={pid} identify={} submit={}",
                identify as u32, submit as u32
            ),
        );
    }

    fn finish(
        &self,
        invocation: u64,
        status: ActionStatus,
        value: Option<vosu_plugin_api::CborValue>,
        code: Option<&str>,
    ) {
        let _ = self.host.send(
            &Message::ActionResult(ActionResult {
                invocation,
                status,
                value,
                code: code.map(str::to_owned),
                message: None,
            }),
            0,
        );
    }
}

impl Drop for TestkitProcessPlugin {
    fn drop(&mut self) {
        self.control.receipt("destroy", "");
        if self.control.fault == Fault::DestroyIgnored {
            loop {
                std::thread::park();
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

vosu_plugin_api::export_plugin!(TestkitProcessPlugin);
