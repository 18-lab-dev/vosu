use std::process::ExitCode;

fn main() -> ExitCode {
    // `vosu plugin-host` first: before `vosux`, before any runtime or other
    // thread, and before any daemon service, listener, updater, discovery,
    // ownership lock or log file (`docs/v2/plugin-abi.md` §7.6 "Launch").
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(status) = vosu::plugin_host::dispatch(&args) {
        std::process::exit(status);
    }
    if let Some(args) = vosu::vosux::arguments(args) {
        std::process::exit(vosu::vosux::run(args));
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("vosu runtime initialization failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(vosu::runtime::run_cli());
    // Tokio cannot abort an already-running `spawn_blocking` calculation. Do
    // not let one expensive beatmap calculation hold process shutdown forever.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vosu startup failed: {error}");
            if vosu::runtime::is_startup_owner_conflict(&error) {
                ExitCode::from(75)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}
