//! Plugin action parsing and the API v1 job client; dotless tools remain verbatim.
use super::{Connection, Failure};
use serde_json::{Value, json};
use std::{ffi::OsString, future::Future};
use vosu_plugin_api::ActionSessionMode;

#[derive(Debug, PartialEq)]
pub(super) struct Invocation {
    action: String,
    arguments: Value,
    detach: bool,
    session: Option<String>,
}

pub(super) fn parse(plugin: &str, args: Vec<OsString>) -> Result<Invocation, String> {
    fn identifier(value: &str) -> bool {
        value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
    }
    if !identifier(plugin) {
        return Err("invalid plugin id".into());
    }
    let mut args = args.into_iter();
    let action = args
        .next()
        .and_then(|v| v.into_string().ok())
        .ok_or("missing action name")?;
    if !identifier(&action) {
        return Err("invalid action name".into());
    }
    let mut invocation = Invocation {
        action: format!("{plugin}.{action}"),
        arguments: json!({}),
        detach: false,
        session: None,
    };
    let mut supplied_arguments = false;
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|_| "action arguments must be UTF-8")?;
        match arg.as_str() {
            "--detach" if !invocation.detach => invocation.detach = true,
            "--session" if invocation.session.is_none() => {
                let session = args
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .ok_or("--session requires an id")?;
                if session.is_empty() || session == "." || session == ".." {
                    return Err("invalid session id".into());
                }
                invocation.session = Some(session);
            }
            _ if !arg.starts_with("--") && !supplied_arguments => {
                invocation.arguments =
                    serde_json::from_str(&arg).map_err(|_| "invalid JSON arguments")?;
                supplied_arguments = true;
            }
            _ => return Err(format!("unexpected action argument: {arg}")),
        }
    }
    Ok(invocation)
}

fn path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .map(|part| if part == "+" { "%20" } else { part })
        .collect()
}

impl Invocation {
    fn route(&self, connection: &Connection) -> Result<String, Failure> {
        let mode = connection
            .broker
            .actions
            .get(&self.action)
            .ok_or_else(|| (127, "unknown CLI action".into()))?;
        match mode {
            ActionSessionMode::None if self.session.is_some() => {
                Err((2, "--session is not valid for a session-less action".into()))
            }
            ActionSessionMode::None => Ok(format!("/api/v1/actions/{}?wait=0", self.action)),
            ActionSessionMode::Required | ActionSessionMode::Optional => {
                let session = self.session.as_deref().unwrap_or("active");
                let encoded = path_segment(session);
                Ok(format!(
                    "/api/v1/sessions/{encoded}/actions/{}?wait=0",
                    self.action
                ))
            }
        }
    }
}

pub(super) async fn execute(
    connection: &Connection,
    invocation: Invocation,
) -> Result<i32, Failure> {
    execute_with_interrupt(connection, invocation, tokio::signal::ctrl_c()).await
}

async fn execute_with_interrupt(
    connection: &Connection,
    invocation: Invocation,
    interrupt: impl Future<Output = std::io::Result<()>>,
) -> Result<i32, Failure> {
    let route = invocation.route(connection)?;
    tokio::pin!(interrupt);
    let body = json!({"arguments": invocation.arguments});
    let create = connection.request_url(reqwest::Method::POST, &route, &body);
    tokio::pin!(create);
    let (mut job, interrupted) = if invocation.detach {
        (create.await?, false)
    } else {
        tokio::select! {
            result = &mut interrupt => {
                result.map_err(|e| (1, format!("could not listen for Ctrl-C: {e}")))?;
                // A request may already have created a job. Obtain its id before
                // cancelling rather than abandoning an invocation we cannot name.
                (create.await?, true)
            }
            job = &mut create => (job?, false),
        }
    };
    let id = job["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or((1, "missing job id".into()))?
        .to_owned();
    if invocation.detach {
        println!("{id}");
        return Ok(0);
    }
    let encoded = path_segment(&id);
    let path = format!("/api/v1/jobs/{encoded}");
    if interrupted {
        connection
            .request_url(reqwest::Method::DELETE, &path, &json!({}))
            .await?;
        return Ok(130);
    }
    let mut progress = Value::Null;
    loop {
        match job["state"].as_str() {
            Some("succeeded") => {
                println!("{}", job["result"]);
                return Ok(0);
            }
            Some("failed") => {
                return Err((
                    1,
                    format!(
                        "{}: {}",
                        job["error"]["code"].as_str().unwrap_or("action_failed"),
                        job["error"]["message"].as_str().unwrap_or("action failed")
                    ),
                ));
            }
            Some("cancelled") => return Ok(130),
            Some("running") => {}
            _ => return Err((1, "invalid job state".into())),
        }
        if job["progress"] != progress {
            progress = job["progress"].clone();
            if !progress.is_null() {
                eprintln!("{progress}");
            }
        }
        let revision = job["revision"]
            .as_u64()
            .ok_or((1, "invalid job revision".into()))?;
        let poll_path = format!("{path}?after={revision}&wait=1000");
        let empty = json!({});
        tokio::select! {
            result = &mut interrupt => {
                result.map_err(|e| (1, format!("could not listen for Ctrl-C: {e}")))?;
                connection.request_url(reqwest::Method::DELETE, &path, &json!({})).await?;
                return Ok(130);
            }
            next = connection.request_url(reqwest::Method::GET, &poll_path, &empty) => job = next?,
        }
    }
}

#[cfg(test)]
mod tests;
