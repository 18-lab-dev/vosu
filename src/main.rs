use std::process::ExitCode;

fn main() -> ExitCode {
    let worker_threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(4));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
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
