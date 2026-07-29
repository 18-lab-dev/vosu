const LINUX_PROCESS_EVENTS_FEATURE: &str = "CARGO_FEATURE_LINUX_PROCESS_EVENTS";
const TARGET_OS: &str = "CARGO_CFG_TARGET_OS";

fn main() {
    println!("cargo:rerun-if-env-changed={LINUX_PROCESS_EVENTS_FEATURE}");
    println!("cargo:rerun-if-env-changed={TARGET_OS}");

    if std::env::var_os("CARGO_FEATURE_TEST_HARNESS").is_some()
        && std::env::var("PROFILE").as_deref() == Ok("release")
    {
        panic!("the test-harness feature is forbidden in release builds");
    }

    #[cfg(windows)]
    embed_windows_resource();

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
    let source = "src/instance/events/bpf/process_events.bpf.c";
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

#[cfg(windows)]
fn embed_windows_resource() {
    use winresource::{VersionInfo, WindowsResource};

    let mut resource = WindowsResource::new();
    resource
        .set_icon("assets/icon.ico")
        .set_language(0x0409)
        .set("ProductName", "vosu")
        .set("CompanyName", "KotRik")
        .set("LegalCopyright", "© KotRik. All rights reserved.")
        .set("FileDescription", "vosu Rust daemon")
        .set("FileVersion", "0.2.0.0")
        .set("ProductVersion", "0.2.0.0")
        .set_version_info(VersionInfo::FILEVERSION, version_quad(0, 2, 0, 0))
        .set_version_info(VersionInfo::PRODUCTVERSION, version_quad(0, 2, 0, 0));
    resource
        .compile()
        .expect("failed to compile Windows resources");
}

#[cfg(windows)]
const fn version_quad(major: u64, minor: u64, patch: u64, build: u64) -> u64 {
    (major << 48) | (minor << 32) | (patch << 16) | build
}
