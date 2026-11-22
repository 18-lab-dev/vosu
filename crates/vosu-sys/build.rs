const LINUX_PROCESS_EVENTS_FEATURE: &str = "CARGO_FEATURE_LINUX_PROCESS_EVENTS";
const TARGET_OS: &str = "CARGO_CFG_TARGET_OS";

fn main() {
    println!("cargo:rerun-if-env-changed={LINUX_PROCESS_EVENTS_FEATURE}");
    println!("cargo:rerun-if-env-changed={TARGET_OS}");

    #[cfg(all(feature = "linux-process-events", target_os = "linux"))]
    {
        let feature_enabled = std::env::var_os(LINUX_PROCESS_EVENTS_FEATURE).is_some();
        let target_is_linux = std::env::var(TARGET_OS).as_deref() == Ok("linux");
        if feature_enabled && target_is_linux {
            build_linux_process_events();
        }
    }
}

#[cfg(all(feature = "linux-process-events", target_os = "linux"))]
fn build_linux_process_events() {
    let source = "src/events/bpf/process_events.bpf.c";
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("process_events.bpf.o");

    println!("cargo:rerun-if-changed={source}");
    libbpf_cargo::SkeletonBuilder::new()
        .source(source)
        .obj(output)
        .clang("clang")
        .build()
        .expect("failed to compile the Linux process-event eBPF program");
}
