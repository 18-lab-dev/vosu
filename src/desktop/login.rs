//! Opt-in graphical-session startup through the systemd user manager.

use crate::platform::{PathResolver, SystemPathResolver};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const UNIT_NAME: &str = "vosu.service";
const MARKER: &str = "# Written by vosu: Start at login";

/// Runs commands without involving a shell; tests can record argv instead.
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<()>;
}

pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<()> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(io::Error::other(format!(
                            "{program} {} failed ({status})",
                            args.join(" ")
                        )))
                    };
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return match result {
                        Err(error) => Err(error),
                        _ => Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("{program} {} timed out after 10 seconds", args.join(" ")),
                        )),
                    };
                }
            }
        }
    }
}

/// Reconciles only vosu's marked user unit. Enabling never starts a second daemon.
pub fn reconcile(enabled: bool) -> io::Result<()> {
    reconcile_with(enabled, &SystemPathResolver, &SystemCommandRunner)
}

/// Injected path and command boundaries; no process-global environment mutation.
pub fn reconcile_with(
    enabled: bool,
    resolver: &impl PathResolver,
    runner: &impl CommandRunner,
) -> io::Result<()> {
    let path = unit_path(resolver)?;
    let existing = owned_unit(&path)?;
    if enabled {
        let unit = unit_text(&resolver.current_exe()?)?;
        if existing.as_deref() != Some(unit.as_str()) {
            let parent = path.parent().expect("unit path has a parent");
            std::fs::create_dir_all(parent)?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(unit.as_bytes())?;
            temporary.as_file().sync_all()?;
            if existing.is_some() {
                // Recheck ownership before replacing an existing unit.
                owned_unit(&path)?;
                temporary.persist(&path).map_err(|error| error.error)?;
            } else {
                temporary
                    .persist_noclobber(&path)
                    .map_err(|error| error.error)?;
            }
        }
        runner.run("systemctl", &["--user", "daemon-reload"])?;
        runner.run("systemctl", &["--user", "enable", UNIT_NAME])?;
    } else if existing.is_some() {
        // An absent unit is already disabled; do not touch another provider's unit.
        runner.run("systemctl", &["--user", "disable", UNIT_NAME])?;
        owned_unit(&path)?;
        std::fs::remove_file(&path)?;
        runner.run("systemctl", &["--user", "daemon-reload"])?;
    }
    Ok(())
}

fn unit_path(resolver: &impl PathResolver) -> io::Result<PathBuf> {
    let nonempty = |name| {
        resolver
            .env_var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let root = nonempty("XDG_CONFIG_HOME")
        .or_else(|| nonempty("HOME").map(|home| home.join(".config")))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "HOME is required when XDG_CONFIG_HOME is unset",
            )
        })?;
    Ok(root.join("systemd/user").join(UNIT_NAME))
}

fn owned_unit(path: &Path) -> io::Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "refusing non-regular vosu.service",
            ));
        }
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(path)?;
    if text.lines().next() != Some(MARKER) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing vosu.service not written by vosu",
        ));
    }
    Ok(Some(text))
}

fn unit_text(executable: &Path) -> io::Result<String> {
    use std::os::unix::ffi::OsStrExt;
    if !executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable path must be absolute",
        ));
    }
    let mut escaped = String::new();
    for &byte in executable.as_os_str().as_bytes() {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'"' => escaped.push_str("\\\""),
            b'%' => escaped.push_str("%%"),
            b'$' => escaped.push_str("$$"),
            0x20..=0x7e => escaped.push(char::from(byte)),
            _ => {
                use std::fmt::Write as _;
                write!(&mut escaped, "\\x{byte:02x}").expect("String write cannot fail");
            }
        }
    }
    Ok(format!(
        "{MARKER}\n[Unit]\nDescription=vosu\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart=\"{escaped}\"\nRestart=on-failure\n\n[Install]\n# Follow the graphical session so the tray has a session bus and display.\nWantedBy=graphical-session.target\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::ffi::OsString;

    struct Resolver {
        root: PathBuf,
        executable: PathBuf,
        xdg: bool,
    }
    impl PathResolver for Resolver {
        fn current_exe(&self) -> io::Result<PathBuf> {
            Ok(self.executable.clone())
        }
        fn launch_dir(&self) -> io::Result<PathBuf> {
            Ok(self.root.clone())
        }
        fn env_var(&self, name: &str) -> Option<OsString> {
            match name {
                "XDG_CONFIG_HOME" if self.xdg => Some(self.root.clone().into_os_string()),
                "XDG_CONFIG_HOME" => Some(OsString::new()),
                "HOME" => Some(self.root.clone().into_os_string()),
                _ => None,
            }
        }
    }
    #[derive(Default)]
    struct Runner {
        calls: RefCell<Vec<Vec<String>>>,
        fail: bool,
    }
    impl CommandRunner for Runner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<()> {
            self.calls.borrow_mut().push(
                std::iter::once(program)
                    .chain(args.iter().copied())
                    .map(str::to_owned)
                    .collect(),
            );
            if self.fail {
                Err(io::Error::other("injected command failure"))
            } else {
                Ok(())
            }
        }
    }
    fn resolver(root: &Path) -> Resolver {
        Resolver {
            root: root.into(),
            executable: "/opt/vosu".into(),
            xdg: true,
        }
    }

    #[test]
    fn exact_unit_text_escapes_systemd_expansions_and_quotes() {
        assert_eq!(
            unit_text(Path::new("/opt/a b/\"vosu\\%$\n")).unwrap(),
            "# Written by vosu: Start at login\n[Unit]\nDescription=vosu\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart=\"/opt/a b/\\\"vosu\\\\%%$$\\x0a\"\nRestart=on-failure\n\n[Install]\n# Follow the graphical session so the tray has a session bus and display.\nWantedBy=graphical-session.target\n"
        );
    }

    #[test]
    fn enabling_reconciles_executable_and_disabling_removes_owned_unit() {
        let directory = tempfile::tempdir().unwrap();
        let mut resolver = resolver(directory.path());
        let runner = Runner::default();
        reconcile_with(true, &resolver, &runner).unwrap();
        let path = unit_path(&resolver).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            unit_text(&resolver.executable).unwrap()
        );
        resolver.executable = "/new/vosu".into();
        reconcile_with(true, &resolver, &runner).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("ExecStart=\"/new/vosu\"")
        );
        reconcile_with(false, &resolver, &runner).unwrap();
        assert!(!path.exists());
        assert_eq!(
            *runner.calls.borrow(),
            vec![
                vec!["systemctl", "--user", "daemon-reload"],
                vec!["systemctl", "--user", "enable", "vosu.service"],
                vec!["systemctl", "--user", "daemon-reload"],
                vec!["systemctl", "--user", "enable", "vosu.service"],
                vec!["systemctl", "--user", "disable", "vosu.service"],
                vec!["systemctl", "--user", "daemon-reload"],
            ]
        );
    }

    #[test]
    fn unrelated_and_symlinked_units_are_never_changed() {
        let directory = tempfile::tempdir().unwrap();
        let resolver = resolver(directory.path());
        let path = unit_path(&resolver).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[Unit]\nDescription=Someone else's unit\n").unwrap();
        let runner = Runner::default();
        for enabled in [false, true] {
            assert!(reconcile_with(enabled, &resolver, &runner).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[Unit]\nDescription=Someone else's unit\n"
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/unavailable", &path).unwrap();
        assert!(reconcile_with(true, &resolver, &runner).is_err());
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn failed_disable_preserves_unit_and_failures_propagate() {
        let directory = tempfile::tempdir().unwrap();
        let resolver = resolver(directory.path());
        reconcile_with(true, &resolver, &Runner::default()).unwrap();
        let runner = Runner {
            fail: true,
            ..Runner::default()
        };
        assert!(reconcile_with(false, &resolver, &runner).is_err());
        assert!(unit_path(&resolver).unwrap().exists());
        assert!(reconcile_with(true, &resolver, &runner).is_err());
    }

    #[test]
    fn empty_xdg_uses_home_and_absent_disable_is_noop() {
        let directory = tempfile::tempdir().unwrap();
        let mut resolver = resolver(directory.path());
        resolver.xdg = false;
        assert_eq!(
            unit_path(&resolver).unwrap(),
            directory.path().join(".config/systemd/user/vosu.service")
        );
        let runner = Runner::default();
        reconcile_with(false, &resolver, &runner).unwrap();
        assert!(runner.calls.borrow().is_empty());
    }
}
