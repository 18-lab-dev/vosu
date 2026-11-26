//! Native command-line entry point; deliberately independent of daemon startup.
//! `vosux <plugin-id> <action> [json] [--detach] [--session <id>]` and
//! `vosu x …` invoke one CLI-enabled plugin action through API v1; the 1.x
//! tool form and its options were removed with the tool CLI (A7.2).
use serde::Deserialize;
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

mod actions;

const USAGE: &str = "usage: vosux <plugin-id> <action> [json-args] [--detach] [--session <id>]";

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
    Action(actions::Invocation),
}
fn parse(args: Vec<OsString>) -> Result<Invocation, String> {
    let mut args = args.into_iter();
    match args.next().as_ref().map(|arg| arg.to_str()) {
        Some(Some("--help")) => Ok(Invocation::Help),
        // A plugin id always contains a dot; a dotless first argument (the
        // removed 1.x tool form) or any other option is a usage error.
        Some(Some(id)) if id.contains('.') => {
            actions::parse(id, args.collect()).map(Invocation::Action)
        }
        _ => Err(USAGE.into()),
    }
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
}
pub fn run(args: Vec<OsString>) -> i32 {
    let result = (|| {
        let invocation = parse(args).map_err(|e| (2, e))?;
        if invocation == Invocation::Help {
            println!(
                "Usage: vosux <plugin-id> <action> [json-args] [--detach] [--session <id>]\n       vosu x <plugin-id> <action> [json-args] [--detach] [--session <id>]\nInvokes one CLI-enabled plugin action through the running vosu daemon.\nCatalog sources and installation are managed in the dashboard (install plans need review)."
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
    fn only_the_plugin_action_form_remains() {
        assert_eq!(parse(args(&["--help"])).unwrap(), Invocation::Help);
        assert!(matches!(
            parse(args(&["fixture.game", "echo", "{}"])).unwrap(),
            Invocation::Action(_)
        ));
        // Counterexample: the removed dotless tool form never runs a tool
        // binary; it and every removed tool option are usage errors (exit 2).
        for values in [
            vec![],
            vec!["demo"],
            vec!["demo", "--help"],
            vec!["server-pp", "--", "arg"],
            vec!["--list"],
            vec!["--list", "--json"],
            vec!["--json"],
            vec!["--open", "server-pp"],
            vec!["--autostart", "server-pp", "on"],
            vec!["--unknown"],
        ] {
            assert_eq!(parse(args(&values)).unwrap_err(), USAGE, "{values:?}");
            assert_eq!(run(args(&values)), 2, "{values:?}");
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
}
