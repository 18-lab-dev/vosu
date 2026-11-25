//! Native command-line entry point; deliberately independent of daemon startup.
use serde::Deserialize;
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

mod actions;

pub fn arguments(args: impl IntoIterator<Item = OsString>) -> Option<Vec<OsString>> {
    let mut args = args.into_iter();
    let executable = args.next()?;
    let stem = Path::new(&executable).file_stem()?.to_str()?;
    let alias = if cfg!(windows) {
        stem.eq_ignore_ascii_case("vosux")
    } else {
        stem == "vosux"
    };
    if alias || args.next().is_some_and(|a| a == "x") {
        Some(args.collect())
    } else {
        None
    }
}
#[derive(Debug, PartialEq)]
enum Invocation {
    Help,
    List(bool),
    Open(String),
    Autostart(String, Option<bool>),
    Action(actions::Invocation),
    Tool(String, Vec<OsString>),
}
fn parse(args: Vec<OsString>) -> Result<Invocation, String> {
    let mut list = false;
    let mut json = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help") => return Ok(Invocation::Help),
            Some("--list") => list = true,
            Some("--json") => json = true,
            Some("--autostart") => {
                let id = args
                    .next()
                    .and_then(|s| s.into_string().ok())
                    .ok_or("--autostart requires a tool id")?;
                let value = match args.next().as_deref().and_then(|s| s.to_str()) {
                    Some("on") => Some(true),
                    Some("off") => Some(false),
                    Some("default") => None,
                    _ => return Err("usage: vosux --autostart <id> on|off|default".into()),
                };
                if list || json || args.next().is_some() || !crate::tools::manifest::valid_id(&id) {
                    return Err("usage: vosux --autostart <id> on|off|default".into());
                }
                return Ok(Invocation::Autostart(id, value));
            }
            Some("--open") => {
                let id = args
                    .next()
                    .and_then(|s| s.into_string().ok())
                    .ok_or("--open requires a tool id")?;
                if list || json || args.next().is_some() || !crate::tools::manifest::valid_id(&id) {
                    return Err("usage: vosux --open <tool>".into());
                }
                return Ok(Invocation::Open(id));
            }
            Some(id) if id.contains('.') => {
                if list || json {
                    return Err("--list and --json cannot be combined with an action".into());
                }
                return actions::parse(id, args.collect()).map(Invocation::Action);
            }
            Some(id) if crate::tools::manifest::valid_id(id) => {
                if list || json {
                    return Err("--list and --json cannot be combined with a tool id".into());
                }
                return Ok(Invocation::Tool(id.into(), args.collect()));
            }
            _ => {
                return Err(format!(
                    "unknown option or invalid tool id: {}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    Ok(Invocation::List(json))
}
type Failure = (i32, String);
fn unavailable(_: impl std::fmt::Display) -> Failure {
    (69, "vosu is not running; start vosu first".into())
}
fn broker_path() -> std::io::Result<PathBuf> {
    vosu_sys::platform::Paths::system(
        vosu_sys::platform::BuildChannel::from_version(env!("CARGO_PKG_VERSION"))
            .map_err(std::io::Error::other)?,
    )?
    .broker_path()
}
#[derive(Deserialize)]
struct Broker {
    origin: String,
    token: String,
    #[serde(default)]
    actions: std::collections::BTreeMap<String, vosu_plugin_api::ActionSessionMode>,
}
struct Connection {
    broker: Broker,
    client: reqwest::Client,
}
impl Connection {
    async fn request(&self, method: reqwest::Method, path: &str) -> Result<Value, Failure> {
        self.request_body(method, path, &serde_json::json!({}))
            .await
    }
    async fn request_body(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<Value, Failure> {
        self.request_url(method, &format!("/tools/_host/v1/cli{path}"), body)
            .await
    }
    async fn request_url(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<Value, Failure> {
        let reply = self
            .client
            .request(method, format!("{}{path}", self.broker.origin))
            .bearer_auth(&self.broker.token)
            .json(body)
            .send()
            .await
            .map_err(unavailable)?;
        let status = reply.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(unavailable("stale broker credential"));
        }
        let value = reply
            .json::<Value>()
            .await
            .map_err(|e| (1, e.to_string()))?;
        if !status.is_success() {
            let code = match status.as_u16() {
                404 => 127,
                403 => 77,
                409 => 2,
                _ => 1,
            };
            return Err((
                code,
                value["error"]["message"]
                    .as_str()
                    .or_else(|| value["error"].as_str())
                    .unwrap_or("daemon request failed")
                    .into(),
            ));
        }
        Ok(value)
    }
    async fn open(&self, id: &str) -> Result<(), Failure> {
        let value = self
            .request(reqwest::Method::POST, &format!("/{id}/open"))
            .await?;
        let url = value["url"]
            .as_str()
            .ok_or((1, "missing launch URL".into()))?;
        vosu_sys::process_spawn::open(url).map_err(|e| (1, format!("could not open tool: {e}")))?;
        Ok(())
    }
}
pub fn run(args: Vec<OsString>) -> i32 {
    let result = (|| {
        let invocation = parse(args).map_err(|e| (2, e))?;
        if invocation == Invocation::Help {
            println!(
                "Usage: vosux [--list [--json] | --open <tool> | --autostart <id> on|off|default | <tool> [ARGS...]]\n       vosux <plugin-id> <action> [json-args] [--detach] [--session <id>]\n       vosu x [OPTIONS] <tool> [ARGS...]\nOptions apply only before a dotless tool id; tool arguments are passed verbatim.\nCatalog sources and installation are managed in the dashboard (install plans need review)."
            );
            return Ok(0);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| (1, e.to_string()))?;
        runtime.block_on(execute(invocation))
    })();
    match result {
        Ok(code) => code,
        Err((code, message)) => {
            eprintln!("vosux: {message}");
            code
        }
    }
}
async fn execute(invocation: Invocation) -> Result<i32, Failure> {
    execute_from_broker(invocation, &broker_path().map_err(unavailable)?).await
}

async fn execute_from_broker(invocation: Invocation, path: &Path) -> Result<i32, Failure> {
    // `broker.json` is republished with `atomic_replace` on every listener
    // stage change (`runtime/worker.rs`), a cross-process write this
    // separate `vosux` process can race; goes through the retrying helper
    // to absorb the measured transient Windows missing-name window instead
    // of surfacing this cross-process read as "unavailable".
    let broker: Broker = serde_json::from_slice(
        &vosu_sys::platform::read_published_async(path)
            .await
            .map_err(unavailable)?,
    )
    .map_err(unavailable)?;
    let origin = url::Url::parse(&broker.origin).map_err(unavailable)?;
    if origin.scheme() != "http"
        || !origin
            .host_str()
            .and_then(|s| s.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        return Err(unavailable("invalid broker origin"));
    }
    let connection = Connection {
        broker,
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| (1, e.to_string()))?,
    };
    match invocation {
        Invocation::Help => unreachable!(),
        Invocation::Action(invocation) => actions::execute(&connection, invocation).await,
        Invocation::Autostart(id, autostart) => {
            connection
                .request_body(
                    reqwest::Method::PUT,
                    &format!("/{id}/autostart"),
                    &serde_json::json!({"autostart":autostart}),
                )
                .await?;
            Ok(0)
        }
        Invocation::List(json) => {
            let value = connection.request(reqwest::Method::GET, "").await?;
            if json {
                println!("{}", serde_json::to_string(&value).unwrap());
            } else {
                for tool in value["tools"]
                    .as_array()
                    .ok_or((1, "invalid tools response".into()))?
                {
                    println!(
                        "{}\t{}\t{}\t{}",
                        tool["id"].as_str().unwrap_or_default(),
                        tool["status"].as_str().unwrap_or_default(),
                        tool["cli"]["usage"]
                            .as_str()
                            .unwrap_or(if tool["ui"] == true {
                                "UI (--open)"
                            } else {
                                "no CLI"
                            }),
                        tool["cli"]["summary"].as_str().unwrap_or_default()
                    );
                }
            }
            Ok(0)
        }
        Invocation::Open(id) => {
            connection.open(&id).await?;
            Ok(0)
        }
        Invocation::Tool(id, args) => {
            let value = match connection
                .request(reqwest::Method::POST, &format!("/{id}"))
                .await
            {
                Err((2, _)) if args.is_empty() => {
                    let listing = connection.request(reqwest::Method::GET, "").await?;
                    if listing["tools"].as_array().is_some_and(|tools| {
                        tools
                            .iter()
                            .any(|tool| tool["id"] == id && tool["ui"] == true)
                    }) {
                        connection.open(&id).await?;
                        return Ok(0);
                    }
                    return Err((2, format!("{id} has no CLI or UI")));
                }
                Err((2, _)) => return Err((2, format!("{id} has no CLI; use vosux --open {id}"))),
                result => result?,
            };
            #[derive(Deserialize)]
            struct Launch {
                session: String,
                argv: Vec<String>,
                cwd: PathBuf,
                env: std::collections::BTreeMap<String, String>,
            }
            let launch: Launch = serde_json::from_value(value).map_err(|e| (1, e.to_string()))?;
            let result = child(&launch.argv, &args, &launch.cwd, &launch.env).await;
            let _ = connection
                .request(
                    reqwest::Method::DELETE,
                    &format!("/session/{}", launch.session),
                )
                .await;
            result
                .map(exit_code)
                .map_err(|e| (1, format!("could not run {id}: {e}")))
        }
    }
}
fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
}
async fn child(
    argv: &[String],
    args: &[OsString],
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
) -> std::io::Result<ExitStatus> {
    let executable = argv
        .first()
        .ok_or_else(|| std::io::Error::other("empty CLI command"))?;
    #[cfg(unix)]
    let (mut interrupt, mut quit, mut terminate, mut hangup) = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::interrupt())?,
            signal(SignalKind::quit())?,
            signal(SignalKind::terminate())?,
            signal(SignalKind::hangup())?,
        )
    };
    #[cfg(windows)]
    let _handler = ConsoleHandler::install()?;
    let mut child = vosu_sys::process_spawn::spawn_tokio(
        tokio::process::Command::new(executable)
            .args(&argv[1..])
            .args(args)
            .current_dir(cwd)
            .envs(env)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit()),
    )?;
    #[cfg(unix)]
    loop {
        let signal = tokio::select! {
            result=child.wait()=>return result,
            _=interrupt.recv()=>continue,
            _=quit.recv()=>continue,
            _=terminate.recv()=>libc::SIGTERM,
            _=hangup.recv()=>libc::SIGHUP,
        };
        if let Some(pid) = child.id() {
            // SAFETY: pid is the live direct child; kill does not dereference memory.
            unsafe {
                libc::kill(pid as libc::pid_t, signal);
            }
        }
    }
    #[cfg(windows)]
    child.wait().await
}
#[cfg(windows)]
struct ConsoleHandler;
#[cfg(windows)]
unsafe extern "system" fn console_event(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}
#[cfg(windows)]
impl ConsoleHandler {
    fn install() -> std::io::Result<Self> {
        // SAFETY: the callback has static lifetime, matches the Windows ABI, and
        // handles only console interrupts. Registering (not ignoring globally)
        // preserves the child's default handler when it inherits the console.
        if unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(console_event), 1)
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self)
    }
}
#[cfg(windows)]
impl Drop for ConsoleHandler {
    fn drop(&mut self) {
        // SAFETY: unregister the exact static callback installed above.
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(console_event), 0);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    #[test]
    fn dispatch() {
        assert_eq!(
            arguments(args(&["/bin/vosux", "demo"])),
            Some(args(&["demo"]))
        );
        assert_eq!(
            arguments(args(&["vosu", "x", "demo"])),
            Some(args(&["demo"]))
        );
        assert_eq!(arguments(args(&["vosu", "--tool", "x"])), None);
    }
    #[test]
    fn options_stop_at_tool() {
        assert_eq!(
            parse(args(&["demo", "--help", "--", "space value", "--json"])).unwrap(),
            Invocation::Tool(
                "demo".into(),
                args(&["--help", "--", "space value", "--json"])
            )
        );
        assert_eq!(parse(args(&[])).unwrap(), Invocation::List(false));
        assert_eq!(
            parse(args(&["--list", "--json"])).unwrap(),
            Invocation::List(true)
        );
        for a in [
            &["--open"][..],
            &["--list", "demo"],
            &["--json", "demo"],
            &["--unknown"],
        ] {
            assert!(parse(args(a)).is_err());
        }
    }
    #[test]
    fn autostart_options() {
        for (value, expected) in [("on", Some(true)), ("off", Some(false)), ("default", None)] {
            assert_eq!(
                parse(args(&["--autostart", "server-pp", value])).unwrap(),
                Invocation::Autostart("server-pp".into(), expected)
            );
        }
        for values in [
            vec!["--autostart"],
            vec!["--autostart", "server-pp"],
            vec!["--autostart", "../bad", "on"],
            vec!["--autostart", "server-pp", "yes"],
            vec!["--autostart", "server-pp", "on", "extra"],
            vec!["--json", "--autostart", "server-pp", "on"],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }
    #[test]
    fn removed_catalog_options_never_install_without_a_reviewed_plan() {
        // Catalog installation is a dashboard install plan with one reviewed
        // confirmation; the CLI has no immediate install/update/uninstall.
        for values in [
            vec!["--sources"],
            vec!["--install", "practice"],
            vec!["--update", "practice"],
            vec!["--uninstall", "practice", "--keep-data"],
            vec!["--uninstall", "practice", "--remove-data"],
        ] {
            assert!(parse(args(&values)).is_err(), "{values:?}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn exit_mapping() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code(ExitStatus::from_raw(42 << 8)), 42);
        assert_eq!(exit_code(ExitStatus::from_raw(2)), 130);
        assert_eq!(exit_code(ExitStatus::from_raw(15)), 143);
    }
}
