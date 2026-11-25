//! The plugin host after its channel exists (`docs/v2/plugin-abi.md` §7.6
//! "Startup" steps 5–10, "Destroy and shutdown").
//!
//! 5. Read `CREATE` within the startup bound.
//! 6. Open the library and verify its SHA-256 against `library_sha256`
//!    through that open object ([`library`]).
//! 7. Load it, resolve `vosu_plugin_entry_v1` and validate the descriptor
//!    with the same pure checks an in-process load uses
//!    (`execution::descriptor::load_library`).
//! 8. Start the ring reader, then call `create`.
//! 9. Start the dispatch worker.
//! 10. Send `READY`.
//!
//! Any failure in 6–8 is reported as `FAILED` with its stage, and the child
//! exits without running anything further. Then the main thread waits for
//! the controlled `DESTROY` (runs `destroy` once every queued dispatch
//! finished, joins the worker, sends `DESTROYED`, exits 0) or for the
//! channel to end (exits at once, without `destroy`).

mod library;
mod services;

pub(super) use library::sha256;

use std::time::{Duration, Instant};

use vosu_plugin_api::{encode, vosu_bytes};

use super::channel::{Endpoint, Sender};
use super::control::{Create, EXIT_CHANNEL_LOST, EXIT_REPORTED, Failed, Ready, StartupStage};
use super::frame::{FrameHeader, control};
use super::ring::PayloadBuffer;
use crate::plugin::dispatch::PluginCtx;
use crate::plugin::error::HostError;
use crate::plugin::execution::descriptor;

/// How long the child waits for `CREATE`: the same 5-second startup bound
/// core applies from spawn, after which core has killed this process anyway.
const CREATE_WAIT: Duration = Duration::from_secs(5);

fn lifecycle(opcode: u32) -> FrameHeader {
    FrameHeader {
        opcode,
        flags: 0,
        correlation: 0,
    }
}

/// Sends one `FAILED` report and returns the exit status for it. A send
/// failure changes nothing: core then sees the exit without a report.
fn report(
    sender: &Sender,
    generation: u64,
    stage: StartupStage,
    code: &str,
    errno: Option<i32>,
    message: &str,
) -> i32 {
    let failed = Failed {
        generation,
        stage,
        code: code.to_owned(),
        errno,
        message: message.to_owned(),
    };
    let _ = sender.send(&lifecycle(control::FAILED), &failed.encode());
    EXIT_REPORTED
}

/// Reports a failure detected before `CREATE` was read (the Linux
/// `security` stage), with generation 0.
#[cfg_attr(
    not(target_os = "linux"),
    expect(dead_code, reason = "only Linux has a pre-CREATE security stage")
)]
pub(super) fn report_before_create(
    endpoint: &Endpoint,
    stage: StartupStage,
    code: &str,
    errno: Option<i32>,
    message: &str,
) -> i32 {
    report(&endpoint.sender, 0, stage, code, errno, message)
}

/// Steps 5–10 and the lifecycle that follows. Returns the exit status.
pub(super) fn run(endpoint: Endpoint) -> i32 {
    let Endpoint {
        sender,
        mut receiver,
        closer,
    } = endpoint;
    let mut buffer = PayloadBuffer::new();
    let create = match receiver.recv_until(&mut buffer, Instant::now().checked_add(CREATE_WAIT)) {
        Ok(Some(header)) if header.opcode == control::CREATE => Create::decode(buffer.bytes()),
        // A first frame other than CREATE is a core protocol violation;
        // silence past the bound means core is gone or gave up.
        _ => {
            let _ = closer.close();
            return EXIT_CHANNEL_LOST;
        }
    };
    let Ok(create) = create else {
        let _ = closer.close();
        return EXIT_CHANNEL_LOST;
    };
    let generation = create.generation;
    let fail = |stage, code: &str, errno, message: &str| {
        report(&sender, generation, stage, code, errno, message)
    };

    let verified = match library::open_verified(&create.library, &create.library_sha256) {
        Ok(verified) => verified,
        Err(failure) => {
            return fail(failure.stage, failure.code, failure.errno, &failure.message);
        }
    };
    // SAFETY: this process is the confined plugin host: the §7.3 drop (or
    // the Windows job assignment) completed, and the library's bytes are
    // the approved digest, read through the object `verified` holds open.
    let loaded = unsafe { descriptor::load_library(verified.load_path()) };
    drop(verified);
    let code = match loaded {
        Ok(code) => code,
        Err(error) => {
            let (stage, code) = load_failure(&error);
            return fail(stage, code, None, &error.to_string());
        }
    };
    let Ok(init) = encode(&create.init) else {
        return fail(
            StartupStage::Create,
            "init_invalid",
            None,
            "init does not encode",
        );
    };

    let runtime = services::Runtime::start(sender.clone(), closer.clone(), receiver);
    let vtable = code.vtable;
    let create_fn = vtable.create.expect("check_descriptor verified create");
    // SAFETY: `services::HOST_SERVICES` is a process-wide static and the
    // relay context is leaked for the process lifetime, so both outlive
    // every call the plugin makes through them; `init` outlives this call.
    let plugin_ctx = unsafe {
        create_fn(
            &services::HOST_SERVICES,
            runtime.host_ctx(),
            vosu_bytes {
                data: init.as_ptr(),
                len: init.len() as u32,
            },
        )
    };
    if plugin_ctx.is_null() {
        return fail(
            StartupStage::Create,
            "create_rejected",
            None,
            "the plugin's create returned NULL",
        );
    }
    let ctx = PluginCtx(plugin_ctx);
    let dispatch = vtable.dispatch.expect("check_descriptor verified dispatch");
    let runtime = runtime.start_dispatch(ctx, dispatch);
    let ready = Ready {
        generation,
        struct_size: vtable.struct_size,
        abi_version: vtable.abi_version,
    };
    if sender
        .send(&lifecycle(control::READY), &ready.encode())
        .is_err()
    {
        return EXIT_CHANNEL_LOST;
    }

    let status = match runtime.wait() {
        services::Outcome::Destroy(runtime) => {
            // Every queued dispatch finished and the worker is joined.
            let destroy = vtable.destroy.expect("check_descriptor verified destroy");
            // SAFETY: `ctx` is this instance's live `plugin_ctx`; no
            // dispatch is running or will run again.
            unsafe { destroy(ctx.raw()) };
            if sender.send(&lifecycle(control::DESTROYED), &[]).is_err() {
                EXIT_CHANNEL_LOST
            } else {
                runtime.finish();
                0
            }
        }
        services::Outcome::Lost => EXIT_CHANNEL_LOST,
    };
    // Plugin threads may outlive `destroy` until this process exits; they
    // must never find the library unmapped.
    std::mem::forget(code);
    status
}

/// The stage and code of a failed load (§7.6: `library` or `abi`).
fn load_failure(error: &HostError) -> (StartupStage, &'static str) {
    match error {
        HostError::AbiMismatch { .. } => (StartupStage::Abi, "abi_mismatch"),
        HostError::DescriptorTooSmall { .. } => (StartupStage::Abi, "descriptor_too_small"),
        HostError::MissingRequiredFunction => (StartupStage::Abi, "missing_required_function"),
        HostError::MissingEntry => (StartupStage::Library, "missing_entry"),
        _ => (StartupStage::Library, "load_failed"),
    }
}

#[cfg(test)]
mod tests;
