use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const SERVICE_NAME: &str = "vosu-capabilities.service";
const SERVICE_PATH: &str = "/etc/systemd/system/vosu-capabilities.service";
const CAPABILITY_SET: &str = if cfg!(feature = "linux-process-events") {
    "cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep"
} else {
    "cap_sys_ptrace=ep"
};
const SETCAP_CANDIDATES: &[&str] = &[
    "/usr/sbin/setcap",
    "/usr/bin/setcap",
    "/sbin/setcap",
    "/bin/setcap",
];
const SYSTEMCTL_CANDIDATES: &[&str] = &["/usr/bin/systemctl", "/bin/systemctl"];

pub(super) fn install(executable: &Path) -> io::Result<()> {
    ensure_root(effective_uid())?;
    ensure_executable(executable)?;

    let setcap = trusted_tool(SETCAP_CANDIDATES, "setcap")?;
    let systemctl = trusted_tool(SYSTEMCTL_CANDIDATES, "systemctl")?;
    let service = service_unit(&setcap, executable);
    write_service(Path::new(SERVICE_PATH), service.as_bytes())?;
    run_systemctl(&systemctl, &["daemon-reload"], "daemon-reload")?;
    run_systemctl(&systemctl, &["start", SERVICE_NAME], "service start")?;

    println!(
        "Installed {SERVICE_PATH} and applied {CAPABILITY_SET} to {executable:?}. Restart vosu without sudo."
    );
    Ok(())
}

fn effective_uid() -> libc::uid_t {
    // SAFETY: `geteuid` has no arguments and only reads process credentials.
    unsafe { libc::geteuid() }
}

fn ensure_root(effective_uid: libc::uid_t) -> io::Result<()> {
    if effective_uid == 0 {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "--setcap requires root privileges. Rerun this executable with sudo (for example, `sudo vosu --setcap`); do not run the daemon itself as root",
    ))
}

fn ensure_executable(executable: &Path) -> io::Result<()> {
    if !executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("vosu executable path is not absolute: {executable:?}"),
        ));
    }

    let metadata = fs::metadata(executable).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot inspect vosu executable {executable:?}: {error}"),
        )
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("vosu executable is not an executable file: {executable:?}"),
        ));
    }

    Ok(())
}

fn trusted_tool(candidates: &[&str], name: &str) -> io::Result<PathBuf> {
    for candidate in candidates {
        let Ok(path) = fs::canonicalize(candidate) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && metadata.uid() == 0
            && metadata.permissions().mode() & 0o022 == 0
        {
            return Ok(path);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("could not find a root-owned, non-writable {name} in standard system paths"),
    ))
}

fn service_unit(setcap: &Path, executable: &Path) -> String {
    format!(
        "[Unit]\nDescription=Apply required Linux capabilities to vosu\n\n[Service]\nType=oneshot\nExecStart={} {CAPABILITY_SET} {}\n",
        systemd_argument(setcap.as_os_str()),
        systemd_argument(executable.as_os_str()),
    )
}

fn systemd_argument(argument: &OsStr) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let bytes = argument.as_bytes();
    let mut escaped = String::with_capacity(bytes.len().saturating_mul(4).saturating_add(2));
    escaped.push('"');
    for &byte in bytes {
        match byte {
            b'%' => escaped.push_str("%%"),
            b'$' => escaped.push_str("$$"),
            _ if byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b'.' | b'_' | b'-' | b'{' | b'}') =>
            {
                escaped.push(char::from(byte));
            }
            _ => {
                escaped.push('\\');
                escaped.push('x');
                escaped.push(char::from(HEX[usize::from(byte >> 4)]));
                escaped.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
    }
    escaped.push('"');
    escaped
}

fn write_service(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("systemd service path has no parent: {path:?}"),
        )
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot create temporary systemd service in {parent:?}: {error}"),
        )
    })?;
    temporary.write_all(contents)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| {
        io::Error::new(
            error.error.kind(),
            format!(
                "cannot install systemd service at {path:?}: {}",
                error.error
            ),
        )
    })?;
    File::open(parent)?.sync_all()
}

fn run_systemctl(systemctl: &Path, arguments: &[&str], operation: &str) -> io::Result<()> {
    let status = Command::new(systemctl)
        .args(arguments)
        .status()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot run systemctl {operation}: {error}"),
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "systemctl {operation} failed with {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    #[test]
    fn non_root_installation_is_rejected() {
        let error = ensure_root(1000).expect_err("non-root installation must fail");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(not(feature = "linux-process-events"))]
    #[test]
    fn standard_build_grants_only_process_memory_capability() {
        assert_eq!(CAPABILITY_SET, "cap_sys_ptrace=ep");
    }

    #[cfg(feature = "linux-process-events")]
    #[test]
    fn process_event_build_grants_ebpf_loader_capabilities() {
        assert_eq!(
            CAPABILITY_SET,
            "cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep"
        );
    }

    #[test]
    fn service_targets_the_running_executable_with_compiled_capabilities() {
        let service = service_unit(
            Path::new("/usr/bin/setcap"),
            Path::new("/opt/%n/${HOME}/vosu builds/vosu"),
        );

        assert_eq!(
            service,
            format!(
                "[Unit]\nDescription=Apply required Linux capabilities to vosu\n\n[Service]\nType=oneshot\nExecStart=\"/usr/bin/setcap\" {CAPABILITY_SET} \"/opt/%%n/$${{HOME}}/vosu\\x20builds/vosu\"\n"
            )
        );
    }

    #[test]
    fn systemd_argument_escapes_specifier_and_environment_expansion() {
        let argument = OsStr::new("/opt/%n/${HOME}/vosu");

        assert_eq!(systemd_argument(argument), "\"/opt/%%n/$${HOME}/vosu\"");
    }

    #[test]
    fn systemd_argument_preserves_non_utf8_path_bytes() {
        let argument = OsStr::from_bytes(b"/opt/vosu-\xff");

        assert_eq!(systemd_argument(argument), "\"/opt/vosu-\\xff\"");
    }
}
