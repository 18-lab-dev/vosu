//! Native Windows: core and a real, confined child process exchange frames
//! over the inherited rings. The child is this test binary re-executed
//! with `--exact HELPER_TEST`; [`helper_process_entry`] does nothing in an
//! ordinary run and acts as the plugin host when it finds an endpoint
//! argument.

use std::ffi::OsStr;
use std::time::{Duration, Instant};

use vosu_plugin_api::vosu_opcode;
use vosu_sys::plugin_process::{EndpointError, KILL_EXIT_CODE, current_job};

use super::super::frame::FrameHeader;
use super::super::ring::PayloadBuffer;
use super::*;

const HELPER_TEST: &str = "plugin::process::windows_launch::tests::helper_process_entry";
const BOUND: Duration = Duration::from_secs(30);

/// Not a test of its own: the plugin-host stand-in's entry point.
#[test]
fn helper_process_entry() {
    let Some(arg) = std::env::args().rev().find(|arg| arg.starts_with("h2p=")) else {
        return;
    };
    let job_is_strict = current_job()
        .unwrap()
        .is_some_and(|job| job.is_plugin_job() && job.process_ids.assigned == 1);
    let Endpoint {
        sender,
        mut receiver,
        closer: _closer,
        ..
    } = attach_inherited(&arg).unwrap();
    let mut buffer = PayloadBuffer::new();
    let header = receiver
        .recv_until(&mut buffer, Some(Instant::now() + BOUND))
        .unwrap()
        .expect("core sent a frame");
    let mut reply = buffer.bytes().to_vec();
    reply.extend_from_slice(&std::process::id().to_le_bytes());
    reply.push(u8::from(job_is_strict));
    // Every public plugin → host frame but a log record is acknowledged.
    sender
        .send_acknowledged(
            1,
            &FrameHeader {
                opcode: vosu_opcode::VOSU_OP_PUBLISH_BATCH as u32,
                flags: 0,
                correlation: header.correlation,
            },
            &reply,
        )
        .unwrap();
    // Stay until core kills the job.
    let _ = receiver.recv(&mut buffer);
    std::process::exit(0);
}

fn launch_helper() -> Launched {
    let exe = std::env::current_exe().unwrap();
    let args = ["--exact", HELPER_TEST, "--nocapture", "--test-threads=1"].map(OsStr::new);
    launch(&exe, &args).unwrap()
}

#[test]
fn launch_connects_core_and_a_confined_child_over_inherited_rings() {
    let Launched {
        endpoint:
            Endpoint {
                sender,
                mut receiver,
                closer: _closer,
                ..
            },
        process,
    } = launch_helper();
    let payload: Vec<u8> = (0..4096_u32).map(|index| (index * 7) as u8).collect();
    sender
        .send(
            &FrameHeader {
                opcode: vosu_opcode::VOSU_OP_STATE_UPDATE as u32,
                flags: 0,
                correlation: 0x5157_0001,
            },
            &payload,
        )
        .unwrap();
    let mut buffer = PayloadBuffer::new();
    let mut recv = || {
        receiver
            .recv_until(&mut buffer, Some(Instant::now() + BOUND))
            .unwrap()
            .expect("the child answered within the bound")
    };
    assert_eq!(
        recv().opcode,
        super::super::frame::control::ACK_NEXT,
        "its ACK_NEXT"
    );
    let header = recv();
    assert_eq!(header.opcode, vosu_opcode::VOSU_OP_PUBLISH_BATCH as u32);
    assert_eq!(header.correlation, 0x5157_0001);
    let reply = buffer.bytes();
    assert_eq!(
        &reply[..payload.len()],
        &payload[..],
        "bytes relayed unchanged"
    );
    let pid = u32::from_le_bytes(reply[payload.len()..payload.len() + 4].try_into().unwrap());
    assert_eq!(pid, process.pid(), "the launched process answered");
    assert_eq!(
        reply[payload.len() + 4],
        1,
        "the child ran inside its own strict job"
    );
    assert!(process.job().contains(process.process()).unwrap());

    process.kill().unwrap();
    assert_eq!(process.wait(Some(BOUND)).unwrap(), Some(KILL_EXIT_CODE));
}

#[test]
fn launch_of_a_missing_program_is_a_spawn_stage_failure() {
    let missing = std::env::temp_dir().join("vosu-windows-launch-missing.exe");
    let error = launch(&missing, &[]).unwrap_err();
    assert!(matches!(error, LaunchError::Spawn(_)), "{error:?}");
    assert_eq!(error.startup_stage(), "spawn");
}

#[test]
fn attach_refuses_a_malformed_or_foreign_endpoint_argument() {
    assert!(matches!(
        attach_inherited("h2p=4:8"),
        Err(AttachError::Endpoints(EndpointError::Malformed))
    ));
    // Well-formed, but naming handles this process does not have.
    assert!(matches!(
        attach_inherited("h2p=7ffffff0:7ffffff4,p2h=7ffffff8:7ffffffc"),
        Err(AttachError::Endpoints(EndpointError::WrongKind { .. }))
    ));
}
