use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(args) = vosu::vosux::arguments(std::env::args_os()) {
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
