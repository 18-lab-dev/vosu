use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libbpf_rs::{
    ErrorKind, Link, MapCore, MapFlags, MapHandle, Object, ObjectBuilder, RingBuffer,
    RingBufferBuilder,
};

use super::{
    MAX_PENDING_EVENTS, Pid, ProcessEvent, ProcessEventKind, ProcessEventLoop, ProcessEventPoll,
    ProcessTrackingControl,
};

const BPF_OBJECT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/process_events.bpf.o"));
const DROPPED_KEY: [u8; 4] = 0_u32.to_ne_bytes();
// Linux reserves this namespace inode as PROC_PID_INIT_INO.
const INITIAL_PID_NAMESPACE_INODE: u64 = 0xEFFF_FFFC;

const EVENT_RECORD_SIZE: usize = 8;

fn parse_event(sample: &[u8]) -> Option<ProcessEvent> {
    if sample.len() != EVENT_RECORD_SIZE || sample[5..] != [0, 0, 0] {
        return None;
    }

    let tgid = u32::from_ne_bytes(sample[..4].try_into().ok()?);
    let kind = match sample[4] {
        1 => ProcessEventKind::Inspect,
        2 => ProcessEventKind::Exit,
        _ => return None,
    };
    Some(ProcessEvent { tgid, kind })
}

#[derive(Default)]
struct CallbackState {
    events: Vec<ProcessEvent>,
    observed: u64,
    malformed: u64,
    overflowed: bool,
}

impl CallbackState {
    fn record(&mut self, event: ProcessEvent) {
        self.observed = self.observed.saturating_add(1);
        if let Some(index) = self
            .events
            .iter()
            .position(|pending| pending.tgid == event.tgid)
        {
            self.overflowed |= self.events[index].kind != event.kind;
            self.events[index] = event;
        } else if self.events.len() < MAX_PENDING_EVENTS {
            self.events.push(event);
        } else {
            self.overflowed = true;
        }
    }
}

struct EbpfTrackingControl {
    tracked: MapHandle,
    shadow: Mutex<BTreeSet<Pid>>,
}

impl ProcessTrackingControl for EbpfTrackingControl {
    fn replace_tracked(&self, tracked: &BTreeSet<Pid>) -> io::Result<()> {
        if tracked.len() > MAX_PENDING_EVENTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} tracked TGIDs exceed the bounded eBPF capacity of {MAX_PENDING_EVENTS}",
                    tracked.len()
                ),
            ));
        }

        let mut shadow = self
            .shadow
            .lock()
            .map_err(|_| io::Error::other("eBPF tracked-map shadow was poisoned"))?;
        let removed = shadow.difference(tracked).copied().collect::<Vec<_>>();
        for tgid in removed {
            delete_if_present(&self.tracked, &tgid.to_ne_bytes(), "tracked TGID")?;
            shadow.remove(&tgid);
        }

        for tgid in tracked {
            self.tracked
                .update(&tgid.to_ne_bytes(), &[1], MapFlags::ANY)
                .map_err(|error| {
                    io::Error::other(format!(
                        "cannot seed tracked TGID {tgid} in eBPF tracked map: {error}"
                    ))
                })?;
            shadow.insert(*tgid);
        }
        Ok(())
    }
}

fn delete_if_present(map: &MapHandle, key: &[u8], label: &str) -> io::Result<()> {
    match map.delete(key) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io::Error::other(format!(
            "cannot delete eBPF {label}: {error}"
        ))),
    }
}

struct EbpfProcessEvents {
    // The links detach before the object closes because fields drop in declaration order.
    _links: [Link; 4],
    ring: RingBuffer<'static>,
    _object: Object,
    callback: Arc<Mutex<CallbackState>>,
    dropped: u64,
}

impl ProcessEventLoop for EbpfProcessEvents {
    fn poll(
        &mut self,
        timeout: Duration,
        events: &mut Vec<ProcessEvent>,
    ) -> io::Result<ProcessEventPoll> {
        let timeout = timeout.min(Duration::from_millis(i32::MAX as u64));
        let result = self.ring.poll_raw(timeout);
        if result < 0 {
            return Err(io::Error::other(format!(
                "eBPF ring-buffer poll failed: {}",
                io::Error::from_raw_os_error(-result)
            )));
        }

        let dropped = read_dropped(&self._object)?;
        let ring_overflow = samples_were_dropped(self.dropped, dropped);
        self.dropped = dropped;

        let mut callback = self
            .callback
            .lock()
            .map_err(|_| io::Error::other("eBPF ring-buffer callback state was poisoned"))?;
        let malformed = std::mem::take(&mut callback.malformed);
        if malformed != 0 {
            callback.events.clear();
            callback.observed = 0;
            callback.overflowed = false;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("eBPF ring buffer contained {malformed} malformed process events"),
            ));
        }

        let observed = std::mem::take(&mut callback.observed);
        let local_overflow = std::mem::take(&mut callback.overflowed);
        events.append(&mut callback.events);
        drop(callback);

        Ok(ProcessEventPoll {
            observed,
            needs_reconcile: local_overflow || ring_overflow,
        })
    }
}

pub(super) fn open() -> io::Result<(Box<dyn ProcessEventLoop>, Arc<dyn ProcessTrackingControl>)> {
    ensure_initial_pid_namespace()?;

    let mut builder = ObjectBuilder::default();
    builder
        .name("vosu_process_events")
        .map_err(|error| io::Error::other(format!("eBPF object configuration failed: {error}")))?;
    let open_object = builder
        .open_memory(BPF_OBJECT)
        .map_err(|error| io::Error::other(format!("eBPF object open failed: {error}")))?;
    let object = open_object
        .load()
        .map_err(|error| io::Error::other(format!("eBPF object load failed: {error}")))?;

    let exec_link = attach_raw(&object, "process_exec", "sched_process_exec")?;
    let fork_link = attach_raw(&object, "process_fork", "sched_process_fork")?;
    let exit_link = attach_raw(&object, "process_exit", "sched_process_exit")?;
    let rename_link = attach_raw(&object, "task_rename", "task_rename")?;

    let tracked = clone_map_handle(&object, "tracked")?;
    let tracking_control: Arc<dyn ProcessTrackingControl> = Arc::new(EbpfTrackingControl {
        tracked,
        shadow: Mutex::new(BTreeSet::new()),
    });

    let callback = Arc::new(Mutex::new(CallbackState {
        events: Vec::with_capacity(MAX_PENDING_EVENTS),
        ..CallbackState::default()
    }));
    let ring_callback = Arc::clone(&callback);
    let mut ring_builder = RingBufferBuilder::new();
    let events = object
        .maps()
        .find(|map| map.name() == OsStr::new("events"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "eBPF events map is missing"))?;
    ring_builder
        .add(&events, move |sample| {
            let Ok(mut callback) = ring_callback.lock() else {
                return 1;
            };
            if let Some(event) = parse_event(sample) {
                callback.record(event);
            } else {
                callback.malformed = callback.malformed.saturating_add(1);
            }
            0
        })
        .map_err(|error| io::Error::other(format!("eBPF ring-buffer setup failed: {error}")))?;
    let ring = ring_builder
        .build()
        .map_err(|error| io::Error::other(format!("eBPF ring-buffer creation failed: {error}")))?;
    let dropped = read_dropped(&object)?;

    Ok((
        Box::new(EbpfProcessEvents {
            _links: [exec_link, fork_link, exit_link, rename_link],
            ring,
            _object: object,
            callback,
            dropped,
        }),
        tracking_control,
    ))
}
fn clone_map_handle(object: &Object, name: &str) -> io::Result<MapHandle> {
    let map = object
        .maps()
        .find(|map| map.name() == OsStr::new(name))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("eBPF {name} map is missing"),
            )
        })?;
    MapHandle::try_from(&map)
        .map_err(|error| io::Error::other(format!("cannot clone eBPF {name} map: {error}")))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NamespaceIdentity {
    inode: u64,
}

fn ensure_initial_pid_namespace() -> io::Result<()> {
    let current = namespace_identity(Path::new("/proc/self/ns/pid"), "current")?;
    let status = fs::read_to_string("/proc/self/status").map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot inspect current PID namespace hierarchy: {error}"),
        )
    })?;
    let depth = parse_nspid_depth(&status)?;

    if !is_initial_pid_namespace(current, depth) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "eBPF process events require the initial PID namespace; falling back to polling",
        ));
    }

    Ok(())
}

fn is_initial_pid_namespace(current: NamespaceIdentity, nspid_depth: usize) -> bool {
    current.inode == INITIAL_PID_NAMESPACE_INODE && nspid_depth == 1
}

fn namespace_identity(path: &Path, label: &str) -> io::Result<NamespaceIdentity> {
    let metadata = path.metadata().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot identify {label} PID namespace: {error}"),
        )
    })?;
    Ok(NamespaceIdentity {
        inode: metadata.ino(),
    })
}

fn parse_nspid_depth(status: &str) -> io::Result<usize> {
    let values = status
        .lines()
        .find_map(|line| line.strip_prefix("NSpid:"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "/proc/self/status has no NSpid hierarchy",
            )
        })?;
    let mut depth = 0;
    for value in values.split_whitespace() {
        value.parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "/proc/self/status has a malformed NSpid hierarchy",
            )
        })?;
        depth += 1;
    }
    if depth == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "/proc/self/status has an empty NSpid hierarchy",
        ));
    }
    Ok(depth)
}

fn attach_raw(object: &Object, program_name: &str, tracepoint_name: &str) -> io::Result<Link> {
    let program = object
        .progs_mut()
        .find(|program| program.name() == OsStr::new(program_name))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("eBPF program {program_name} is missing"),
            )
        })?;
    program
        .attach_raw_tracepoint(tracepoint_name)
        .map_err(|error| {
            io::Error::other(format!(
                "eBPF raw tracepoint attach failed for {program_name}: {error}"
            ))
        })
}

fn read_dropped(object: &Object) -> io::Result<u64> {
    let map = object
        .maps()
        .find(|map| map.name() == OsStr::new("dropped"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "eBPF dropped map is missing"))?;
    let mut value = [0_u8; 8];
    let found = map
        .lookup_into(&DROPPED_KEY, &mut value, MapFlags::ANY)
        .map_err(|error| io::Error::other(format!("eBPF dropped-counter read failed: {error}")))?;
    if !found {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "eBPF dropped counter has no zero key",
        ));
    }
    Ok(u64::from_ne_bytes(value))
}

fn samples_were_dropped(previous: u64, current: u64) -> bool {
    previous != current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(tgid: u32, kind: u8) -> [u8; EVENT_RECORD_SIZE] {
        let mut record = [0; EVENT_RECORD_SIZE];
        record[..4].copy_from_slice(&tgid.to_ne_bytes());
        record[4] = kind;
        record
    }

    #[test]
    fn parses_only_complete_known_records() {
        assert_eq!(
            parse_event(&record(42, 1)),
            Some(ProcessEvent {
                tgid: 42,
                kind: ProcessEventKind::Inspect,
            })
        );
        assert_eq!(
            parse_event(&record(91, 2)),
            Some(ProcessEvent {
                tgid: 91,
                kind: ProcessEventKind::Exit,
            })
        );
        assert_eq!(parse_event(&record(42, 3)), None);
        assert_eq!(parse_event(&record(42, 1)[..7]), None);

        let mut nonzero_padding = record(42, 1);
        nonzero_padding[7] = 1;
        assert_eq!(parse_event(&nonzero_padding), None);
    }

    #[test]
    fn coalesces_latest_event_per_tgid_and_reconciles_mixed_kinds() {
        let mut callback = CallbackState::default();
        callback.record(ProcessEvent {
            tgid: 42,
            kind: ProcessEventKind::Inspect,
        });
        callback.record(ProcessEvent {
            tgid: 42,
            kind: ProcessEventKind::Exit,
        });

        assert_eq!(
            callback.events,
            [ProcessEvent {
                tgid: 42,
                kind: ProcessEventKind::Exit,
            }]
        );
        assert_eq!(callback.observed, 2);
        assert!(callback.overflowed);
    }

    #[test]
    fn detects_loss_including_counter_wraparound() {
        assert!(!samples_were_dropped(7, 7));
        assert!(samples_were_dropped(7, 8));
        assert!(samples_were_dropped(u64::MAX, 0));
    }

    #[test]
    fn parses_pid_namespace_hierarchy_depth() {
        assert_eq!(
            parse_nspid_depth("Name:\tvosu\nNSpid:\t231 17 4\n").unwrap(),
            3
        );
        assert!(parse_nspid_depth("Name:\tvosu\n").is_err());
        assert!(parse_nspid_depth("NSpid:\t231 invalid\n").is_err());
        assert!(parse_nspid_depth("NSpid:\t\n").is_err());
    }

    #[test]
    fn initial_namespace_evidence_is_strict() {
        let initial = NamespaceIdentity {
            inode: INITIAL_PID_NAMESPACE_INODE,
        };
        let child = NamespaceIdentity {
            inode: INITIAL_PID_NAMESPACE_INODE + 1,
        };

        assert!(is_initial_pid_namespace(initial, 1));
        assert!(!is_initial_pid_namespace(initial, 2));
        assert!(!is_initial_pid_namespace(child, 1));
    }
}
