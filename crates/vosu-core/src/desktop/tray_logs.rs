//! Best-effort visible log viewers, launched outside the tray/UI thread.
use std::{io, process::Command};

pub(super) fn show() -> io::Result<()> {
    show_platform()
}

fn launch(command: &mut Command) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn()?;
    // A terminal can live indefinitely. A detached thread reaps it without
    // retaining a Tokio blocking worker or delaying daemon shutdown.
    if let Err(error) = std::thread::Builder::new()
        .name("vosu-log-viewer".into())
        .spawn(move || {
            let _ = child.wait();
        })
    {
        tracing::warn!(%error, "Log viewer child reaper unavailable");
    }
    Ok(())
}

#[cfg(windows)]
fn show_platform() -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["logs", "--follow", "--window"]);
    command.creation_flags(CREATE_NEW_CONSOLE);
    launch(&mut command)
}

#[cfg(target_os = "linux")]
fn terminal_command(terminal: &std::ffi::OsStr, executable: &std::path::Path) -> Command {
    let mut command = Command::new(terminal);
    let name = std::path::Path::new(terminal)
        .file_name()
        .unwrap_or(terminal);
    command.arg(if name == "gnome-terminal" || name == "kgx" {
        "--"
    } else {
        "-e"
    });
    command.arg(executable).args(["logs", "--follow"]);
    command
}

#[cfg(target_os = "linux")]
fn open_log_file() -> io::Result<()> {
    let log_file = crate::logging::system_logs_dir()?.join("latest.log");
    tracing::warn!(
        path = %log_file.display(),
        "No terminal available for live logs; opening the current log file instead"
    );
    launch(Command::new("xdg-open").arg(log_file))
}

#[cfg(target_os = "linux")]
fn show_platform() -> io::Result<()> {
    let executable = std::env::current_exe()?;
    let mut preferred = Command::new("xdg-terminal-exec");
    preferred.arg(&executable).args(["logs", "--follow"]);
    if launch(&mut preferred).is_ok() {
        return Ok(());
    }
    if let Some(terminal) = std::env::var_os("TERMINAL").filter(|value| !value.is_empty())
        && launch(&mut terminal_command(&terminal, &executable)).is_ok()
    {
        return Ok(());
    }
    for terminal in [
        "foot",
        "kitty",
        "alacritty",
        "gnome-terminal",
        "konsole",
        "xterm",
    ] {
        if launch(&mut terminal_command(terminal.as_ref(), &executable)).is_ok() {
            return Ok(());
        }
    }
    open_log_file()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn show_platform() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "No desktop log viewer on this platform",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{ffi::OsStr, path::Path};

    #[test]
    fn terminal_arguments_preserve_executable_paths() {
        let executable = Path::new("/opt/vosu nightly/vosu");
        for (terminal, separator) in [
            ("/usr/bin/gnome-terminal", "--"),
            ("kgx", "--"),
            ("kitty", "-e"),
            ("xterm", "-e"),
        ] {
            let command = terminal_command(OsStr::new(terminal), executable);
            assert_eq!(command.get_program(), OsStr::new(terminal));
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [
                    OsStr::new(separator),
                    executable.as_os_str(),
                    OsStr::new("logs"),
                    OsStr::new("--follow"),
                ]
            );
        }
    }
}
