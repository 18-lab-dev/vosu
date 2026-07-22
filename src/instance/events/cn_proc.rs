use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use super::{
    MAX_PENDING_EVENTS, ProcessEvent, ProcessEventKind, ProcessEventLoop, ProcessEventPoll,
};

const CN_IDX_PROC: u32 = 1;
const CN_VAL_PROC: u32 = 1;
const PROC_CN_MCAST_LISTEN: u32 = 1;
const PROC_CN_MCAST_IGNORE: u32 = 2;
const PROC_EVENT_EXEC: u32 = 0x0000_0002;
const PROC_EVENT_COMM: u32 = 0x0000_0200;
const PROC_EVENT_EXIT: u32 = 0x8000_0000;
const FILTERED_EVENTS: u32 = PROC_EVENT_EXEC | PROC_EVENT_COMM | PROC_EVENT_EXIT;
const RECEIVE_BUFFER_SIZE: usize = 64 * 1024;
const MAX_RECEIVES_PER_POLL: usize = 64;
const NETLINK_HEADER_LEN: usize = 16;
const CONNECTOR_HEADER_LEN: usize = 20;
const PROC_EVENT_LEN: usize = 40;
const LEGACY_INPUT_LEN: usize = 4;
const FILTERED_INPUT_LEN: usize = 8;
const MAX_SUBSCRIPTION_LEN: usize = NETLINK_HEADER_LEN + CONNECTOR_HEADER_LEN + FILTERED_INPUT_LEN;
const ALIGNMENT: usize = 4;
const LISTEN_SEQUENCE: u32 = 1;
const FILTER_SEQUENCE: u32 = 2;
const ACK_TIMEOUT: Duration = Duration::from_secs(1);

struct CnProcEventLoop {
    socket: OwnedFd,
    receive_buffer: Box<[u8]>,
}

struct SubscriptionCleanup {
    fd: libc::c_int,
    armed: bool,
}

impl SubscriptionCleanup {
    fn new(fd: libc::c_int) -> Self {
        Self { fd, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for SubscriptionCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = send_ignore(self.fd);
        }
    }
}

struct ConnectorMessage<'a> {
    acknowledgement: u32,
    payload: &'a [u8],
}

pub(super) fn open() -> io::Result<Box<dyn ProcessEventLoop>> {
    let raw_fd = unsafe {
        // SAFETY: socket has no pointer arguments. A successful descriptor is immediately owned.
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_CONNECTOR,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let socket = unsafe {
        // SAFETY: raw_fd was just returned by socket and has not been transferred elsewhere.
        OwnedFd::from_raw_fd(raw_fd)
    };

    let mut address: libc::sockaddr_nl = unsafe {
        // SAFETY: sockaddr_nl is valid when zero-initialized before its public fields are assigned.
        std::mem::zeroed()
    };
    address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    address.nl_pid = unsafe {
        // SAFETY: getpid has no memory-safety preconditions.
        libc::getpid() as u32
    };
    address.nl_groups = CN_IDX_PROC;

    let bound = unsafe {
        // SAFETY: address points to an initialized sockaddr_nl for the duration of bind.
        libc::bind(
            socket.as_raw_fd(),
            (&raw const address).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if bound < 0 {
        return Err(io::Error::last_os_error());
    }

    // Establish the backwards-compatible all-events subscription first. Kernels that support
    // proc_input then replace its filter in-place; older kernels ignore the longer update while
    // retaining this subscription.
    send_subscription(
        socket.as_raw_fd(),
        PROC_CN_MCAST_LISTEN,
        None,
        LISTEN_SEQUENCE,
    )?;
    let cleanup = SubscriptionCleanup::new(socket.as_raw_fd());
    receive_subscription_ack(socket.as_raw_fd())?;
    send_subscription(
        socket.as_raw_fd(),
        PROC_CN_MCAST_LISTEN,
        Some(FILTERED_EVENTS),
        FILTER_SEQUENCE,
    )?;
    cleanup.disarm();

    Ok(Box::new(CnProcEventLoop {
        socket,
        receive_buffer: vec![0; RECEIVE_BUFFER_SIZE].into_boxed_slice(),
    }))
}

impl ProcessEventLoop for CnProcEventLoop {
    fn poll(
        &mut self,
        timeout: Duration,
        events: &mut Vec<ProcessEvent>,
    ) -> io::Result<ProcessEventPoll> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let mut descriptor = libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let wait_ms = remaining_millis(timeout, deadline);
            let result = unsafe {
                // SAFETY: descriptor points to one initialized pollfd for the duration of poll.
                libc::poll(&raw mut descriptor, 1, wait_ms)
            };
            if result == 0 {
                return Ok(ProcessEventPoll::default());
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    if deadline.is_some_and(|end| Instant::now() >= end) {
                        return Ok(ProcessEventPoll::default());
                    }
                    continue;
                }
                return Err(error);
            }
            if descriptor.revents & (libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(io::Error::other(format!(
                    "CN_PROC netlink poll failed with revents {:#x}",
                    descriptor.revents
                )));
            }
            if descriptor.revents & (libc::POLLIN | libc::POLLERR) != 0 {
                let mut outcome = receive_available(
                    self.socket.as_raw_fd(),
                    &mut self.receive_buffer,
                    events,
                )?;
                if descriptor.revents & libc::POLLERR != 0 {
                    outcome.needs_reconcile = true;
                }
                return Ok(outcome);
            }
        }
    }
}

impl Drop for CnProcEventLoop {
    fn drop(&mut self) {
        let _ = send_ignore(self.socket.as_raw_fd());
    }
}

fn remaining_millis(original: Duration, deadline: Option<Instant>) -> libc::c_int {
    let remaining = deadline
        .map(|end| end.saturating_duration_since(Instant::now()))
        .unwrap_or(original);
    if remaining.is_zero() {
        return 0;
    }
    remaining
        .as_millis()
        .saturating_add(u128::from(
            !remaining.subsec_nanos().is_multiple_of(1_000_000),
        ))
        .min(libc::c_int::MAX as u128) as libc::c_int
}

fn subscription_message(
    operation: u32,
    event_filter: Option<u32>,
    sequence: u32,
) -> ([u8; MAX_SUBSCRIPTION_LEN], usize) {
    let payload_len = if event_filter.is_some() {
        FILTERED_INPUT_LEN
    } else {
        LEGACY_INPUT_LEN
    };
    let message_len = NETLINK_HEADER_LEN + CONNECTOR_HEADER_LEN + payload_len;
    let mut message = [0_u8; MAX_SUBSCRIPTION_LEN];
    write_u32(&mut message, 0, message_len as u32);
    write_u16(&mut message, 4, libc::NLMSG_DONE as u16);
    write_u32(&mut message, 8, sequence);
    write_u32(&mut message, 12, unsafe {
        // SAFETY: getpid has no memory-safety preconditions.
        libc::getpid() as u32
    });

    let connector = NETLINK_HEADER_LEN;
    write_u32(&mut message, connector, CN_IDX_PROC);
    write_u32(&mut message, connector + 4, CN_VAL_PROC);
    write_u32(&mut message, connector + 8, sequence);
    write_u16(&mut message, connector + 16, payload_len as u16);

    let payload = connector + CONNECTOR_HEADER_LEN;
    write_u32(&mut message, payload, operation);
    if let Some(filter) = event_filter {
        write_u32(&mut message, payload + 4, filter);
    }
    (message, message_len)
}

fn send_subscription(
    fd: libc::c_int,
    operation: u32,
    event_filter: Option<u32>,
    sequence: u32,
) -> io::Result<()> {
    let (message, message_len) = subscription_message(operation, event_filter, sequence);
    let mut kernel: libc::sockaddr_nl = unsafe {
        // SAFETY: sockaddr_nl is valid when zero-initialized before its family is assigned.
        std::mem::zeroed()
    };
    kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;

    let sent = unsafe {
        // SAFETY: message and kernel are initialized and borrowed only for this sendto call.
        libc::sendto(
            fd,
            message.as_ptr().cast(),
            message_len,
            0,
            (&raw const kernel).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    if sent as usize != message_len {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short CN_PROC subscription write",
        ));
    }
    Ok(())
}

fn send_ignore(fd: libc::c_int) -> io::Result<()> {
    retry_interrupted(|| {
        send_subscription(
            fd,
            PROC_CN_MCAST_IGNORE,
            None,
            FILTER_SEQUENCE.wrapping_add(1),
        )
    })
}

fn retry_interrupted(mut operation: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    loop {
        match operation() {
            Err(error) if error.raw_os_error() == Some(libc::EINTR) => {}
            result => return result,
        }
    }
}

fn receive_subscription_ack(fd: libc::c_int) -> io::Result<()> {
    let deadline = Instant::now().checked_add(ACK_TIMEOUT);
    let mut buffer = [0_u8; RECEIVE_BUFFER_SIZE];
    loop {
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe {
            // SAFETY: descriptor points to one initialized pollfd for the duration of poll.
            libc::poll(
                &raw mut descriptor,
                1,
                remaining_millis(ACK_TIMEOUT, deadline),
            )
        };
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for CN_PROC LISTEN acknowledgement",
            ));
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::other(format!(
                "CN_PROC acknowledgement poll failed with revents {:#x}",
                descriptor.revents
            )));
        }

        let received = unsafe {
            // SAFETY: buffer is writable for its full length for this nonblocking receive.
            libc::recv(
                fd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if received < 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR | libc::EAGAIN) => continue,
                _ => return Err(error),
            }
        }
        if received == 0 {
            return Err(invalid_data("empty CN_PROC acknowledgement datagram"));
        }
        if parse_ack_datagram(&buffer[..received as usize])? {
            return Ok(());
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for CN_PROC LISTEN acknowledgement",
            ));
        }
    }
}

fn parse_ack_datagram(bytes: &[u8]) -> io::Result<bool> {
    let mut acknowledged = false;
    visit_connector_messages(bytes, |message| {
        // cn_proc_ack passes the request sequence to send_msg, which replaces it with
        // the per-CPU event sequence. The incremented acknowledgement is the stable marker.
        if message.acknowledgement != 1 {
            return Ok(());
        }
        if message.payload.len() != PROC_EVENT_LEN {
            return Err(invalid_data("invalid CN_PROC acknowledgement length"));
        }
        if read_u32(message.payload, 0) != 0 {
            return Ok(());
        }
        let error = read_u32(message.payload, 16) as i32;
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        acknowledged = true;
        Ok(())
    })?;
    Ok(acknowledged)
}

fn receive_available(
    fd: libc::c_int,
    buffer: &mut [u8],
    events: &mut Vec<ProcessEvent>,
) -> io::Result<ProcessEventPoll> {
    let mut outcome = ProcessEventPoll::default();
    for _ in 0..MAX_RECEIVES_PER_POLL {
        let mut iovec = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
        };
        let mut message: libc::msghdr = unsafe {
            // SAFETY: zero is a valid initial state for msghdr; the data iovec is assigned below.
            std::mem::zeroed()
        };
        message.msg_iov = &raw mut iovec;
        message.msg_iovlen = 1;
        let received = unsafe {
            // SAFETY: msghdr references the live writable buffer and iovec for this nonblocking call.
            libc::recvmsg(fd, &raw mut message, libc::MSG_DONTWAIT)
        };
        if received < 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) => return Ok(outcome),
                Some(libc::ENOBUFS) => {
                    outcome.needs_reconcile = true;
                    return Ok(outcome);
                }
                _ => return Err(error),
            }
        }
        if received == 0 {
            outcome.needs_reconcile = true;
            return Ok(outcome);
        }
        if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
            outcome.needs_reconcile = true;
            continue;
        }

        match parse_event_datagram(&buffer[..received as usize], events) {
            Ok(parsed) => {
                outcome.observed = outcome.observed.saturating_add(parsed.observed);
                outcome.needs_reconcile |= parsed.needs_reconcile;
            }
            Err(_) => outcome.needs_reconcile = true,
        }
    }
    outcome.needs_reconcile = true;
    Ok(outcome)
}

fn parse_event_datagram(
    bytes: &[u8],
    events: &mut Vec<ProcessEvent>,
) -> io::Result<ProcessEventPoll> {
    let mut outcome = ProcessEventPoll::default();
    visit_connector_messages(bytes, |message| {
        let Some(event) = parse_connector_event(message.payload)? else {
            return Ok(());
        };
        outcome.observed = outcome.observed.saturating_add(1);
        if let Some(event) = event {
            if events.len() < MAX_PENDING_EVENTS {
                events.push(event);
            } else {
                outcome.needs_reconcile = true;
            }
        }
        Ok(())
    })?;
    Ok(outcome)
}

fn parse_connector_event(payload: &[u8]) -> io::Result<Option<Option<ProcessEvent>>> {
    if payload.len() != PROC_EVENT_LEN {
        return Err(invalid_data("invalid process event length"));
    }
    let what = read_u32(payload, 0);
    let kind = match what {
        PROC_EVENT_EXEC => Some(ProcessEventKind::Inspect),
        PROC_EVENT_COMM => {
            if super::candidate_comm(&payload[24..40]) {
                Some(ProcessEventKind::Inspect)
            } else {
                None
            }
        }
        PROC_EVENT_EXIT => Some(ProcessEventKind::Exit),
        _ => return Ok(None),
    };
    let tgid = read_u32(payload, 20);
    Ok(Some(kind.map(|kind| ProcessEvent { tgid, kind })))
}

fn visit_connector_messages(
    mut bytes: &[u8],
    mut visitor: impl FnMut(ConnectorMessage<'_>) -> io::Result<()>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if bytes.len() < NETLINK_HEADER_LEN {
            return Err(invalid_data("truncated netlink header"));
        }
        let message_len = read_u32(bytes, 0) as usize;
        if message_len < NETLINK_HEADER_LEN || message_len > bytes.len() {
            return Err(invalid_data("invalid netlink message length"));
        }
        if read_u16(bytes, 4) != libc::NLMSG_DONE as u16 {
            return Err(invalid_data("unexpected netlink message type"));
        }
        let connector = &bytes[NETLINK_HEADER_LEN..message_len];
        if connector.len() < CONNECTOR_HEADER_LEN {
            return Err(invalid_data("truncated connector header"));
        }
        if read_u32(connector, 0) != CN_IDX_PROC || read_u32(connector, 4) != CN_VAL_PROC {
            return Err(invalid_data("unexpected connector identifier"));
        }
        let payload_len = read_u16(connector, 16) as usize;
        if payload_len != connector.len() - CONNECTOR_HEADER_LEN {
            return Err(invalid_data("invalid connector payload length"));
        }
        visitor(ConnectorMessage {
            acknowledgement: read_u32(connector, 12),
            payload: &connector[CONNECTOR_HEADER_LEN..],
        })?;

        let aligned_len =
            align(message_len).ok_or_else(|| invalid_data("netlink length overflow"))?;
        if aligned_len > bytes.len() {
            if message_len == bytes.len() {
                break;
            }
            return Err(invalid_data("truncated netlink message padding"));
        }
        bytes = &bytes[aligned_len..];
    }
    Ok(())
}

fn align(value: usize) -> Option<usize> {
    value
        .checked_add(ALIGNMENT - 1)
        .map(|v| v & !(ALIGNMENT - 1))
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_ne_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_ne_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_message(what: u32, tgid: u32, comm: &[u8]) -> Vec<u8> {
        let message_len = NETLINK_HEADER_LEN + CONNECTOR_HEADER_LEN + PROC_EVENT_LEN;
        let mut bytes = vec![0_u8; align(message_len).unwrap()];
        write_u32(&mut bytes, 0, message_len as u32);
        write_u16(&mut bytes, 4, libc::NLMSG_DONE as u16);
        let connector = NETLINK_HEADER_LEN;
        write_u32(&mut bytes, connector, CN_IDX_PROC);
        write_u32(&mut bytes, connector + 4, CN_VAL_PROC);
        write_u16(&mut bytes, connector + 16, PROC_EVENT_LEN as u16);
        let payload = connector + CONNECTOR_HEADER_LEN;
        write_u32(&mut bytes, payload, what);
        write_u32(&mut bytes, payload + 16, tgid + 1000);
        write_u32(&mut bytes, payload + 20, tgid);
        let comm_len = comm.len().min(16);
        bytes[payload + 24..payload + 24 + comm_len].copy_from_slice(&comm[..comm_len]);
        bytes
    }

    #[test]
    fn parser_extracts_exec_tgid_as_inspect_delta() {
        let mut events = Vec::new();
        let outcome =
            parse_event_datagram(&event_message(PROC_EVENT_EXEC, 42, &[]), &mut events).unwrap();

        assert_eq!(
            (outcome, events),
            (
                ProcessEventPoll {
                    observed: 1,
                    needs_reconcile: false,
                },
                vec![ProcessEvent {
                    tgid: 42,
                    kind: ProcessEventKind::Inspect,
                }]
            )
        );
    }

    #[test]
    fn parser_appends_candidate_comm_as_inspect_delta() {
        let mut events = Vec::new();
        let outcome =
            parse_event_datagram(&event_message(PROC_EVENT_COMM, 73, b"osu!"), &mut events)
                .unwrap();

        assert_eq!(
            (outcome, events),
            (
                ProcessEventPoll {
                    observed: 1,
                    needs_reconcile: false,
                },
                vec![ProcessEvent {
                    tgid: 73,
                    kind: ProcessEventKind::Inspect,
                }]
            )
        );
    }

    #[test]
    fn parser_observes_noncandidate_comm_without_appending() {
        let mut events = Vec::new();
        let outcome = parse_event_datagram(
            &event_message(PROC_EVENT_COMM, 73, b"unrelated"),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            outcome,
            ProcessEventPoll {
                observed: 1,
                needs_reconcile: false,
            }
        );
        assert!(events.is_empty());
    }

    #[test]
    fn parser_extracts_exit_tgid_as_exit_delta() {
        let mut events = Vec::new();
        let outcome =
            parse_event_datagram(&event_message(PROC_EVENT_EXIT, 99, &[]), &mut events).unwrap();

        assert_eq!(
            (outcome, events),
            (
                ProcessEventPoll {
                    observed: 1,
                    needs_reconcile: false,
                },
                vec![ProcessEvent {
                    tgid: 99,
                    kind: ProcessEventKind::Exit,
                }]
            )
        );
    }

    #[test]
    fn parser_rejects_unrelated_event_without_observing_or_appending() {
        let mut events = Vec::new();
        let outcome = parse_event_datagram(&event_message(1, 101, &[]), &mut events).unwrap();

        assert_eq!(outcome, ProcessEventPoll::default());
        assert!(events.is_empty());
    }

    #[test]
    fn parser_rejects_malformed_payload() {
        let mut bytes = event_message(PROC_EVENT_EXEC, 42, &[]);
        let connector = NETLINK_HEADER_LEN;
        write_u16(&mut bytes, connector + 16, 32);

        assert_eq!(
            parse_event_datagram(&bytes, &mut Vec::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn ack_parser_accepts_kernel_rewritten_connector_sequence() {
        let mut bytes = event_message(0, 0, &[]);
        let connector = NETLINK_HEADER_LEN;
        write_u32(&mut bytes, connector + 8, 0xfeed_beef);
        write_u32(&mut bytes, connector + 12, 1);
        write_u32(&mut bytes, connector + CONNECTOR_HEADER_LEN + 16, 0);

        assert!(parse_ack_datagram(&bytes).unwrap());
    }

    #[test]
    fn subscription_messages_use_legacy_then_filtered_proc_input_layouts() {
        let (legacy, legacy_len) =
            subscription_message(PROC_CN_MCAST_LISTEN, None, LISTEN_SEQUENCE);
        let legacy_payload = NETLINK_HEADER_LEN + CONNECTOR_HEADER_LEN;
        assert_eq!(legacy_len, 40);
        assert_eq!(read_u32(&legacy, 0), 40);
        assert_eq!(read_u16(&legacy, NETLINK_HEADER_LEN + 16), 4);
        assert_eq!(read_u32(&legacy, legacy_payload), PROC_CN_MCAST_LISTEN);

        let (filtered, filtered_len) =
            subscription_message(PROC_CN_MCAST_LISTEN, Some(FILTERED_EVENTS), FILTER_SEQUENCE);
        let filtered_payload = NETLINK_HEADER_LEN + CONNECTOR_HEADER_LEN;
        assert_eq!(filtered_len, 44);
        assert_eq!(read_u32(&filtered, 0), 44);
        assert_eq!(read_u16(&filtered, NETLINK_HEADER_LEN + 16), 8);
        assert_eq!(read_u32(&filtered, filtered_payload), PROC_CN_MCAST_LISTEN);
        assert_eq!(read_u32(&filtered, filtered_payload + 4), FILTERED_EVENTS);
    }

    #[test]
    fn cleanup_retries_interrupted_ignore_send() {
        let mut attempts = 0;

        retry_interrupted(|| {
            attempts += 1;
            if attempts < 3 {
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(())
            }
        })
        .unwrap();

        assert_eq!(attempts, 3);
    }

    #[test]
    fn cleanup_stops_after_non_interrupted_ignore_failure() {
        let mut attempts = 0;

        let error = retry_interrupted(|| {
            attempts += 1;
            Err(io::Error::from_raw_os_error(libc::EIO))
        })
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(attempts, 1);
    }
}
