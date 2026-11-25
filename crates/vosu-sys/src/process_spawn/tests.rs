//! The gated wrappers keep the standard library's semantics. The
//! Windows inheritance race they close is tested against real plugin-host
//! spawns in `plugin_process::windows::tests`.

use super::*;

fn shell(script: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/Q", "/C", script]);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
}

#[test]
fn output_captures_both_streams_and_closes_stdin() {
    // Each script reads stdin to its end first: it returns only because
    // `output` gives the child an empty (null) stdin.
    #[cfg(windows)]
    let script = "findstr x & echo out-& echo err 1>&2";
    #[cfg(not(windows))]
    let script = "cat; echo out-; echo err >&2";
    let output = output(&mut shell(script)).unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "out-");
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "err");
}

#[test]
fn status_reports_the_exit_code() {
    assert_eq!(status(&mut shell("exit 7")).unwrap().code(), Some(7));
}

#[test]
fn tokio_output_awaits_outside_the_gate() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let output = runtime.block_on(async {
        let mut command = tokio::process::Command::from(shell("echo async"));
        command.stdout(Stdio::piped());
        let child = spawn_tokio(&mut command).unwrap();
        // Waiting never needs the gate: it can be held meanwhile.
        let _gate = hold();
        child.wait_with_output().await.unwrap()
    });
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "async");
}
