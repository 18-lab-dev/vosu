//! The measurement-only Windows named-pipe candidate (A5.6: never a
//! production transport without a separately approved secure-handoff
//! amendment).
//!
//! Everything but the byte carrier matches the ring candidate: the same
//! frames (`frame::encode_head` heads, 1 MiB cap), the same receive-side
//! `DirectionPolicy` admission before any payload copy, the same reusable
//! 8-byte-aligned payload buffer, one write per frame and no flush on either
//! side (`FlushFileBuffers` would wait for the peer to drain, which the ring
//! never does).
//!
//! Security is matched as closely as a named object allows: the pipe gets an
//! unguessable 128-bit random name, a DACL granting only the current user,
//! `FILE_FLAG_FIRST_PIPE_INSTANCE`, one instance, `PIPE_REJECT_REMOTE_CLIENTS`,
//! and core itself opens the only client end (without impersonation rights,
//! `SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS`) before the child exists.
//! The child receives that client handle by inheritance, exactly like the
//! ring's four handles, and never sees the name. The residual difference is
//! that the name is listed under `\\.\pipe\` for the session's lifetime;
//! with its single instance already connected, nothing else can connect.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::process::Command;

use vosu_core::plugin::process::frame::{
    Direction, DirectionPolicy, FrameHeader, FramePolicy, HEAD_BYTES, decode_head, encode_head,
};
use windows_sys::Win32::Foundation::{
    ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ALL_ACCESS, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    ReadFile, SECURITY_ANONYMOUS, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::candidate::{FrameIo, TransportCounters};

/// Kernel pipe buffer and local read buffer size: far above any frame this
/// runner sends (a few hundred bytes), so one read normally takes a frame.
const BUFFER_BYTES: usize = 64 * 1024;
/// `SECURITY_DESCRIPTOR_REVISION` (`winnt.h`).
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

/// One end of the pipe.
pub struct PipeIo {
    handle: OwnedHandle,
    policy: DirectionPolicy,
    /// Bytes read from the pipe and not yet consumed: `read[start..end]`.
    read: Vec<u8>,
    start: usize,
    end: usize,
    /// Reusable 8-byte-aligned payload storage and the last payload length.
    payload: Vec<u64>,
    payload_len: usize,
    /// Reusable staging for one frame's head and payload, written at once.
    stage: Vec<u8>,
    counters: TransportCounters,
}

impl PipeIo {
    fn new(handle: OwnedHandle, receives: Direction) -> Self {
        Self {
            handle,
            policy: DirectionPolicy::new(receives),
            read: vec![0; BUFFER_BYTES],
            start: 0,
            end: 0,
            payload: Vec::new(),
            payload_len: 0,
            stage: Vec::with_capacity(BUFFER_BYTES),
            counters: TransportCounters::default(),
        }
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// One `ReadFile` of at most `into.len()` bytes; at least one.
    fn read_some(handle: HANDLE, into: &mut [u8]) -> usize {
        let mut done = 0u32;
        let len = u32::try_from(into.len()).expect("read fits u32");
        // SAFETY: `into` is writable for `len` bytes and `done` is a valid
        // out pointer; the handle is synchronous, so no OVERLAPPED.
        let ok = unsafe {
            ReadFile(
                handle,
                into.as_mut_ptr(),
                len,
                &mut done,
                std::ptr::null_mut(),
            )
        };
        assert!(ok != 0 && done > 0, "pipe ReadFile: {}", last_error());
        done as usize
    }

    /// Ensures at least `needed` unconsumed bytes are buffered.
    fn fill(&mut self, needed: usize) {
        if self.end - self.start >= needed {
            return;
        }
        if self.start > 0 {
            self.read.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        while self.end - self.start < needed {
            let handle = self.raw();
            let read = Self::read_some(handle, &mut self.read[self.end..]);
            self.counters.copies += 1;
            self.end += read;
        }
    }
}

impl FrameIo for PipeIo {
    fn send(&mut self, header: &FrameHeader, payload: &[u8]) {
        let head = encode_head(header, payload.len()).expect("frame within the cap");
        self.stage.clear();
        self.stage.extend_from_slice(&head);
        self.stage.extend_from_slice(payload);
        self.counters.copies += 1;
        let mut written = 0usize;
        while written < self.stage.len() {
            let mut done = 0u32;
            let remaining = &self.stage[written..];
            // SAFETY: `remaining` is readable for its length and `done` is a
            // valid out pointer; synchronous handle, no OVERLAPPED.
            let ok = unsafe {
                WriteFile(
                    self.raw(),
                    remaining.as_ptr(),
                    u32::try_from(remaining.len()).expect("frame fits u32"),
                    &mut done,
                    std::ptr::null_mut(),
                )
            };
            assert!(ok != 0 && done > 0, "pipe WriteFile: {}", last_error());
            self.counters.copies += 1;
            written += done as usize;
        }
        self.counters.frames += 1;
    }

    fn recv(&mut self) -> FrameHeader {
        self.fill(HEAD_BYTES);
        let mut head = [0u8; HEAD_BYTES];
        head.copy_from_slice(&self.read[self.start..self.start + HEAD_BYTES]);
        self.start += HEAD_BYTES;
        let (header, len) = decode_head(&head).expect("valid frame head");
        self.policy
            .admit(&header, len)
            .expect("frame admitted by policy");
        let buffered = (self.end - self.start).min(len);
        let handle = self.raw();
        self.payload_len = len;
        let payload = aligned_mut(&mut self.payload, len);
        payload[..buffered].copy_from_slice(&self.read[self.start..self.start + buffered]);
        let mut filled = buffered;
        let mut kernel_reads = 0;
        while filled < len {
            filled += Self::read_some(handle, &mut payload[filled..]);
            kernel_reads += 1;
        }
        self.start += buffered;
        if len > 0 {
            self.counters.copies += 1 + kernel_reads;
        }
        self.counters.frames += 1;
        header
    }

    fn payload(&self) -> &[u8] {
        // SAFETY: as in `aligned_mut`, shared; `payload_len` never exceeds
        // the bytes `aligned_mut` sized `payload` for.
        unsafe { std::slice::from_raw_parts(self.payload.as_ptr().cast::<u8>(), self.payload_len) }
    }

    fn counters(&self) -> TransportCounters {
        self.counters
    }
}

/// Grows `words` to hold `len` bytes and returns them, 8-byte aligned.
fn aligned_mut(words: &mut Vec<u64>, len: usize) -> &mut [u8] {
    let needed = len.div_ceil(8);
    if words.len() < needed {
        words.resize(needed, 0);
    }
    // SAFETY: `words` holds at least `len.div_ceil(8) * 8 >= len`
    // initialized bytes, any byte pattern is a valid `u8`, and the unique
    // borrow of `words` covers the returned slice.
    unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), len) }
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A security descriptor whose DACL allows only the current user. The
/// buffers it points into live as long as this value.
struct UserOnly {
    _token_user: Vec<u64>,
    _acl: Vec<u64>,
    descriptor: Box<SECURITY_DESCRIPTOR>,
}

impl UserOnly {
    fn new() -> Self {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle names this process; `token` is a valid
        // out pointer.
        let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        assert_ne!(ok, 0, "OpenProcessToken: {}", last_error());
        // SAFETY: `token` was just opened and is owned here.
        let token = unsafe { OwnedHandle::from_raw_handle(token as RawHandle) };
        let mut token_user = vec![0u64; 64];
        let mut needed = 0u32;
        // SAFETY: `token_user` is writable for its byte length and `needed`
        // is a valid out pointer.
        let ok = unsafe {
            GetTokenInformation(
                token.as_raw_handle() as HANDLE,
                TokenUser,
                token_user.as_mut_ptr().cast(),
                (token_user.len() * 8) as u32,
                &mut needed,
            )
        };
        assert_ne!(ok, 0, "GetTokenInformation(TokenUser): {}", last_error());
        // SAFETY: GetTokenInformation wrote a `TOKEN_USER` at the start of
        // the (8-aligned) buffer.
        let sid = unsafe { (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        // SAFETY: `sid` points into `token_user`, a valid SID.
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        let acl_len = size_of::<ACL>() + 16 + sid_len;
        let mut acl = vec![0u64; acl_len.div_ceil(8)];
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        // SAFETY: `acl` is writable for `acl_len` bytes, enough for the header
        // and one access-allowed ACE carrying `sid`.
        unsafe {
            assert_ne!(
                InitializeAcl(acl_ptr, acl_len as u32, ACL_REVISION),
                0,
                "InitializeAcl"
            );
            assert_ne!(
                AddAccessAllowedAce(acl_ptr, ACL_REVISION, FILE_ALL_ACCESS, sid),
                0,
                "AddAccessAllowedAce: {}",
                last_error()
            );
        }
        let mut descriptor = Box::new(SECURITY_DESCRIPTOR::default());
        let descriptor_ptr = std::ptr::from_mut(descriptor.as_mut()).cast();
        // SAFETY: `descriptor` is a writable absolute security descriptor and
        // `acl` outlives it (both are owned by the returned value).
        unsafe {
            assert_ne!(
                InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION),
                0,
                "InitializeSecurityDescriptor"
            );
            assert_ne!(
                SetSecurityDescriptorDacl(descriptor_ptr, 1, acl_ptr, 0),
                0,
                "SetSecurityDescriptorDacl"
            );
        }
        Self {
            _token_user: token_user,
            _acl: acl,
            descriptor,
        }
    }

    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::from_mut(self.descriptor.as_mut()).cast(),
            bInheritHandle: 0,
        }
    }
}

/// Core's half before the child exists: the connected server end and the
/// one client end the child will inherit.
pub struct Parent {
    server: OwnedHandle,
    client: OwnedHandle,
}

pub fn prepare() -> Parent {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).expect("random pipe name");
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = wide(&format!(
        r"\\.\pipe\vosu-p1-{}-{suffix}",
        std::process::id()
    ));
    let mut security = UserOnly::new();
    let attributes = security.attributes();
    // SAFETY: `name` is NUL-terminated; `attributes` and the descriptor it
    // points to outlive the call.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUFFER_BYTES as u32,
            BUFFER_BYTES as u32,
            0,
            &attributes,
        )
    };
    assert_ne!(
        server,
        INVALID_HANDLE_VALUE,
        "CreateNamedPipeW: {}",
        last_error()
    );
    // SAFETY: `server` was just created and is owned here.
    let server = unsafe { OwnedHandle::from_raw_handle(server as RawHandle) };
    // SAFETY: `name` is NUL-terminated; the client is opened without an
    // inheritable security descriptor and without impersonation rights.
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        client,
        INVALID_HANDLE_VALUE,
        "open pipe client: {}",
        last_error()
    );
    // SAFETY: `client` was just opened and is owned here.
    let client = unsafe { OwnedHandle::from_raw_handle(client as RawHandle) };
    // SAFETY: synchronous server handle; the client already connected, so
    // this returns at once (`ERROR_PIPE_CONNECTED`).
    let connected =
        unsafe { ConnectNamedPipe(server.as_raw_handle() as HANDLE, std::ptr::null_mut()) };
    // SAFETY: no preconditions.
    let error = unsafe { GetLastError() };
    assert!(
        connected != 0 || error == ERROR_PIPE_CONNECTED,
        "ConnectNamedPipe: {error}"
    );
    drop(security);
    Parent { server, client }
}

impl Parent {
    /// Marks only the client handle inheritable for the next spawn and
    /// returns its value for the child.
    pub fn configure(&self, _command: &mut Command) -> String {
        let client = self.client.as_raw_handle();
        // SAFETY: `client` is a live handle this process owns.
        let ok = unsafe {
            SetHandleInformation(client as HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
        };
        assert_ne!(ok, 0, "SetHandleInformation");
        (client as usize).to_string()
    }

    /// Core's side once the child holds the client: the parent's copy of the
    /// client end is closed.
    pub fn into_core(self) -> PipeIo {
        drop(self.client);
        PipeIo::new(self.server, Direction::PluginToHost)
    }
}

/// The child's side, from its `--endpoint` argument (the inherited client
/// handle value).
pub fn attach_child(endpoint: &str) -> PipeIo {
    let handle: usize = endpoint.parse().expect("pipe handle value");
    // SAFETY: core launched this process with exactly this handle inherited
    // for it, and nothing else in this process owns it.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
    PipeIo::new(handle, Direction::HostToPlugin)
}
