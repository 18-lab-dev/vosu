//! The T0 production ring as a candidate transport: one
//! `vosu_sys::plugin_process::RingObjects` per direction (a sealed memfd and
//! an eventfd on Linux, an unnamed section and an auto-reset event on
//! Windows) driven by
//! `vosu_core::plugin::process::channel`, with the frozen per-direction frame
//! policy. Endpoint handoff is inheritance of exactly the four objects and
//! nothing else, the same handoff the pipe candidate gets for its one
//! handle. (The bench marks them inheritable around one `Command::spawn`;
//! production spawn code, L0/W0, uses an explicit allowlist instead.)

use std::process::Command;

use vosu_core::plugin::process::channel::{self, Closer, Receiver, RingEnd, Sender};
use vosu_core::plugin::process::frame::{Direction, DirectionPolicy, FrameHeader};
use vosu_core::plugin::process::ring::{self, PayloadBuffer, REGION_BYTES};
use vosu_sys::plugin_process::RingObjects;

use super::candidate::{FrameIo, TransportCounters};

/// One attached side of the ring channel.
pub struct RingIo {
    sender: Sender,
    receiver: Receiver,
    buffer: PayloadBuffer,
    _closer: Closer,
    frames_sent: u64,
    frames_received: u64,
}

impl RingIo {
    fn attach(outbound: RingObjects, inbound: RingObjects, receives: Direction) -> Self {
        let endpoint = channel::connect(
            RingEnd::from(outbound),
            RingEnd::from(inbound),
            Box::new(DirectionPolicy::new(receives)),
        )
        .expect("attach ring channel");
        Self {
            sender: endpoint.sender,
            receiver: endpoint.receiver,
            buffer: PayloadBuffer::new(),
            _closer: endpoint.closer,
            frames_sent: 0,
            frames_received: 0,
        }
    }
}

impl FrameIo for RingIo {
    fn send(&mut self, header: &FrameHeader, payload: &[u8]) {
        self.sender.send(header, payload).expect("ring send");
        self.frames_sent += 1;
    }

    fn recv(&mut self) -> FrameHeader {
        let header = self.receiver.recv(&mut self.buffer).expect("ring receive");
        self.frames_received += 1;
        header
    }

    fn payload(&self) -> &[u8] {
        self.buffer.bytes()
    }

    /// T0's channel documents exactly two payload copies per frame: the
    /// sender's into the ring and the receiver's out into the aligned
    /// buffer, one on each side. It reads the clock once per send (its 2 s
    /// backpressure deadline); a receive with no deadline reads none.
    fn counters(&self) -> TransportCounters {
        TransportCounters {
            frames: self.frames_sent + self.frames_received,
            copies: self.frames_sent + self.frames_received,
            clock_reads: self.frames_sent,
        }
    }
}

/// Core's half before the child exists: both directions created and
/// initialized, `host_to_plugin` first.
pub struct Parent {
    host_to_plugin: RingObjects,
    plugin_to_host: RingObjects,
}

pub fn prepare() -> Parent {
    let host_to_plugin = RingObjects::create(REGION_BYTES).expect("create host->plugin ring");
    let plugin_to_host = RingObjects::create(REGION_BYTES).expect("create plugin->host ring");
    ring::initialize(host_to_plugin.region()).expect("initialize host->plugin ring");
    ring::initialize(plugin_to_host.region()).expect("initialize plugin->host ring");
    Parent {
        host_to_plugin,
        plugin_to_host,
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    use super::{Parent, REGION_BYTES, RingObjects};

    impl Parent {
        fn raw(&self) -> [RawFd; 4] {
            [
                self.host_to_plugin.mapping_fd().as_raw_fd(),
                self.host_to_plugin.doorbell_fd().as_raw_fd(),
                self.plugin_to_host.mapping_fd().as_raw_fd(),
                self.plugin_to_host.doorbell_fd().as_raw_fd(),
            ]
        }

        /// The child-side descriptor numbers (inherited unchanged).
        pub fn endpoint_arg(&self) -> String {
            let raw = self.raw();
            raw.map(|fd| fd.to_string()).join(",")
        }

        /// Clears close-on-exec on exactly the four descriptors, in the
        /// forked child only.
        pub fn inherit(&self, command: &mut Command) {
            let raw = self.raw();
            // SAFETY: the hook runs in the forked child before exec and calls
            // only `fcntl`, which is async-signal-safe and allocates nothing;
            // it changes only the child's copy of these descriptors.
            unsafe {
                command.pre_exec(move || {
                    for fd in raw {
                        if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }

        /// Nothing to undo: the parent's descriptors never lost
        /// close-on-exec.
        pub fn after_spawn(&self) {}
    }

    /// The four descriptor numbers this child was launched with, in
    /// `endpoint_arg` order.
    pub fn adopt(endpoint: &str) -> (RingObjects, RingObjects) {
        let fds: Vec<RawFd> = endpoint
            .split(',')
            .map(|fd| fd.parse().expect("descriptor number"))
            .collect();
        let [h2p_map, h2p_bell, p2h_map, p2h_bell] = fds[..] else {
            panic!("ring endpoint needs four descriptors, got {endpoint:?}");
        };
        // SAFETY: core launched this process with exactly these descriptors
        // open and inherited for it, and nothing else in this process owns
        // them; `from_inherited` then validates each one's kind and size.
        let owned = |fd: RawFd| unsafe { OwnedFd::from_raw_fd(fd) };
        (
            RingObjects::from_inherited(owned(h2p_map), owned(h2p_bell), REGION_BYTES)
                .expect("adopt host->plugin ring"),
            RingObjects::from_inherited(owned(p2h_map), owned(p2h_bell), REGION_BYTES)
                .expect("adopt plugin->host ring"),
        )
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::process::Command;

    use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};

    use super::{Parent, REGION_BYTES, RingObjects};

    fn set_inherit(handle: RawHandle, inherit: bool) {
        let flags = if inherit { HANDLE_FLAG_INHERIT } else { 0 };
        // SAFETY: `handle` is a live handle this process owns.
        let ok = unsafe { SetHandleInformation(handle as HANDLE, HANDLE_FLAG_INHERIT, flags) };
        assert_ne!(ok, 0, "SetHandleInformation");
    }

    impl Parent {
        fn raw(&self) -> [RawHandle; 4] {
            [
                self.host_to_plugin.mapping_handle().as_raw_handle(),
                self.host_to_plugin.doorbell_handle().as_raw_handle(),
                self.plugin_to_host.mapping_handle().as_raw_handle(),
                self.plugin_to_host.doorbell_handle().as_raw_handle(),
            ]
        }

        /// The handle values, which inheritance preserves in the child.
        pub fn endpoint_arg(&self) -> String {
            self.raw()
                .map(|handle| (handle as usize).to_string())
                .join(",")
        }

        /// Marks exactly the four handles inheritable for the next spawn.
        pub fn inherit(&self, _command: &mut Command) {
            for handle in self.raw() {
                set_inherit(handle, true);
            }
        }

        /// Makes the four handles non-inheritable again.
        pub fn after_spawn(&self) {
            for handle in self.raw() {
                set_inherit(handle, false);
            }
        }
    }

    pub fn adopt(endpoint: &str) -> (RingObjects, RingObjects) {
        let handles: Vec<usize> = endpoint
            .split(',')
            .map(|handle| handle.parse().expect("handle value"))
            .collect();
        let [h2p_map, h2p_bell, p2h_map, p2h_bell] = handles[..] else {
            panic!("ring endpoint needs four handles, got {endpoint:?}");
        };
        // SAFETY: core launched this process with exactly these handles
        // inherited for it, and nothing else in this process owns them;
        // `from_inherited` maps the section at the trusted expected size.
        let owned = |handle: usize| unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        (
            RingObjects::from_inherited(owned(h2p_map), owned(h2p_bell), REGION_BYTES)
                .expect("adopt host->plugin ring"),
            RingObjects::from_inherited(owned(p2h_map), owned(p2h_bell), REGION_BYTES)
                .expect("adopt plugin->host ring"),
        )
    }
}

impl Parent {
    pub fn configure(&self, command: &mut Command) -> String {
        self.inherit(command);
        self.endpoint_arg()
    }

    /// Core's side, once the child holds its copies.
    pub fn into_core(self) -> RingIo {
        self.after_spawn();
        RingIo::attach(
            self.host_to_plugin,
            self.plugin_to_host,
            Direction::PluginToHost,
        )
    }
}

/// The child's side, from its `--endpoint` argument.
pub fn attach_child(endpoint: &str) -> RingIo {
    let (host_to_plugin, plugin_to_host) = platform::adopt(endpoint);
    RingIo::attach(plugin_to_host, host_to_plugin, Direction::HostToPlugin)
}
