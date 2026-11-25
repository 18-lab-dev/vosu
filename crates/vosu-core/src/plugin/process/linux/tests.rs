//! End to end on native Linux: core's real [`HostSpawner`] starts this test
//! binary as the plugin host. An `.init_array` entry (which runs before the
//! test harness starts any thread) plays the child when `argv[1]` is
//! `plugin-host`: it arms parent death, adopts the channel, drops
//! privileges, and reports over the inherited rings with the private
//! lifecycle frames, so core observes the drop's readback through the
//! production channel rather than a side file.

use std::ffi::{CStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vosu_sys::plugin_process::{arm_parent_death, drop_privileges};

use super::super::channel::Endpoint;
use super::super::frame::{FrameHeader, control};
use super::super::ring::PayloadBuffer;
use super::{HostSpawner, SpawnedHost, adopt_channel};

const IDENTITY: &str = "vosu-core-plugin-host-test";
const BOUND: Duration = Duration::from_secs(20);

type InitArray = extern "C" fn(libc::c_int, *const *const libc::c_char, *const *const libc::c_char);

#[used]
#[unsafe(link_section = ".init_array")]
static PLUGIN_HOST_TEST_ENTRY: InitArray = entry;

extern "C" fn entry(
    argc: libc::c_int,
    argv: *const *const libc::c_char,
    _envp: *const *const libc::c_char,
) {
    if argc < 2 || argv.is_null() {
        return;
    }
    let args: Vec<OsString> = (0..argc as usize)
        .map(|index| {
            // SAFETY: glibc passes `argc` valid NUL-terminated strings to
            // `.init_array` entries.
            let arg = unsafe { CStr::from_ptr(*argv.add(index)) };
            OsString::from_vec(arg.to_bytes().to_vec())
        })
        .collect();
    if args[1] != "plugin-host" {
        return;
    }
    let code = child(&args);
    // SAFETY: ends this re-executed process without running the harness.
    unsafe { libc::_exit(code) }
}

fn frame(opcode: u32) -> FrameHeader {
    FrameHeader {
        opcode,
        flags: 0,
        correlation: 0,
    }
}

/// The §7.6 startup order up to `READY`, then a controlled destroy.
fn child(args: &[OsString]) -> i32 {
    if args.len() != 5 || args[2] != IDENTITY {
        return 70;
    }
    let Some(parent) = args[3].to_str().and_then(|pid| pid.parse().ok()) else {
        return 70;
    };
    if arm_parent_death(parent).is_err() {
        return 72;
    }
    let Ok(Endpoint {
        sender,
        mut receiver,
        ..
    }) = adopt_channel(&args[4])
    else {
        return 73;
    };
    let (opcode, payload) = match drop_privileges() {
        Ok(readback) => (
            control::READY,
            format!(
                "effective={:x} permitted={:x} inheritable={:x} ambient={:x} nnp={} seccomp={} userns={:?}",
                readback.effective,
                readback.permitted,
                readback.inheritable,
                readback.ambient,
                readback.no_new_privs,
                readback.seccomp,
                readback.user_namespace_errno,
            ),
        ),
        Err(failure) => (control::FAILED, failure.to_string()),
    };
    if sender.send(&frame(opcode), payload.as_bytes()).is_err() {
        return 74;
    }
    let mut buffer = PayloadBuffer::new();
    match receiver.recv_until(&mut buffer, Instant::now().checked_add(BOUND)) {
        Ok(Some(header)) if header.opcode == control::DESTROY => {}
        _ => return 75,
    }
    if sender.send(&frame(control::DESTROYED), &[]).is_err() {
        return 76;
    }
    0
}

#[test]
fn spawned_plugin_host_drops_privileges_and_speaks_over_its_inherited_rings() {
    let spawner =
        HostSpawner::with_executable(std::env::current_exe().unwrap(), IDENTITY.to_owned())
            .unwrap();
    let SpawnedHost { child, channel } = spawner.spawn().unwrap();
    let Endpoint {
        sender,
        mut receiver,
        ..
    } = channel;
    let mut buffer = PayloadBuffer::new();
    let deadline = || Instant::now().checked_add(BOUND);

    let header = receiver
        .recv_until(&mut buffer, deadline())
        .unwrap()
        .expect("the plugin host reported its startup within the bound");
    let report = String::from_utf8_lossy(buffer.bytes()).into_owned();
    assert_eq!(header, frame(control::READY), "{report}");
    assert_eq!(
        report,
        format!(
            "effective=0 permitted=0 inheritable=0 ambient=0 nnp=1 seccomp=2 userns=Some({})",
            libc::EPERM
        )
    );
    assert!(child.try_wait().unwrap().is_none());

    sender.send(&frame(control::DESTROY), &[]).unwrap();
    let header = receiver
        .recv_until(&mut buffer, deadline())
        .unwrap()
        .expect("the plugin host acknowledged destroy");
    assert_eq!(header, frame(control::DESTROYED));
    let status = child
        .wait_timeout(BOUND)
        .unwrap()
        .expect("the plugin host exited after destroy");
    assert_eq!(status.code(), Some(0));
}

#[test]
fn a_child_that_cannot_exec_is_a_spawn_failure_with_nothing_left_running() {
    let spawner =
        HostSpawner::with_executable(PathBuf::from("/nonexistent/vosu"), IDENTITY.to_owned())
            .unwrap();
    let error = spawner.spawn().unwrap_err();
    assert!(
        matches!(
            error,
            super::LaunchError::Spawn(vosu_sys::plugin_process::SpawnError::Child { .. })
        ),
        "{error:?}"
    );
}
