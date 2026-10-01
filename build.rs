const LINUX_PROCESS_EVENTS_FEATURE: &str = "CARGO_FEATURE_LINUX_PROCESS_EVENTS";
const TARGET_OS: &str = "CARGO_CFG_TARGET_OS";

fn main() {
    println!("cargo:rerun-if-env-changed={LINUX_PROCESS_EVENTS_FEATURE}");
    println!("cargo:rerun-if-env-changed={TARGET_OS}");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=PROFILE");

    if std::env::var_os("CARGO_FEATURE_TEST_HARNESS").is_some()
        && std::env::var("PROFILE").as_deref() == Ok("release")
    {
        panic!("the test-harness feature is forbidden in release builds");
    }

    embed_generator_bundle();

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

fn embed_generator_bundle() {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let output = out_dir.join("daemon_generator_bundle.rs");
    let target = match (
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        std::env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
    ) {
        (Ok("linux"), Ok("x86_64")) => Some("linux-x64"),
        (Ok("windows"), Ok("x86_64")) => Some("win-x64"),
        _ => None,
    };
    let (zip, checksum) = target.as_ref().map_or((None, None), |rid| {
        let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("target/offset-generator")
            .join(rid);
        let zip = root.join(format!("offset-generator-v1-{rid}.zip"));
        let checksum = root.join(format!("offset-generator-v1-{rid}.zip.sha256"));
        println!("cargo:rerun-if-changed={}", zip.display());
        println!("cargo:rerun-if-changed={}", checksum.display());
        (Some(zip), Some(checksum))
    });

    let valid = match (zip.as_ref(), checksum.as_ref()) {
        (Some(zip), Some(checksum)) if zip.is_file() && checksum.is_file() => {
            let expected = std::fs::read_to_string(checksum)
                .unwrap_or_default()
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let bytes = std::fs::read(zip).expect("failed to read offset generator bundle");
            let mut actual = String::with_capacity(64);
            for byte in Sha256::digest(&bytes) {
                write!(actual, "{byte:02x}").unwrap();
            }
            if expected.len() != 64 || expected != actual {
                if std::env::var("PROFILE").as_deref() == Ok("release") {
                    panic!(
                        "offset generator bundle checksum mismatch: {}",
                        zip.display()
                    );
                }
                false
            } else {
                true
            }
        }
        _ => {
            if std::env::var("PROFILE").as_deref() == Ok("release") && target.is_some() {
                panic!("missing offset generator bundle for release target");
            }
            false
        }
    };

    let mut source = String::from("impl DaemonAssets {\n");
    if valid {
        let zip = zip.unwrap();
        let checksum = checksum.unwrap();
        writeln!(
            source,
            "    pub fn generator_bundle() -> Option<(&'static [u8], &'static str)> {{ Some((include_bytes!({:?}), include_str!({:?}))) }}",
            zip.to_string_lossy().as_ref(),
            checksum.to_string_lossy().as_ref(),
        )
        .unwrap();
    } else {
        source.push_str(
            "    pub fn generator_bundle() -> Option<(&'static [u8], &'static str)> { None }\n",
        );
    }
    source.push_str("}\n");
    std::fs::write(output, source).expect("failed to write generated daemon asset bindings");
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

    let [major, minor, patch] = [
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH"),
    ]
    .map(|component| {
        component
            .parse::<u16>()
            .expect("Windows resource version components must fit in 16 bits")
    });
    let text_version = format!("{major}.{minor}.{patch}.0");
    let numeric_version = version_quad(major.into(), minor.into(), patch.into(), 0);

    let mut resource = WindowsResource::new();
    resource
        .set_icon("assets/icon.ico")
        .set_language(0x0409)
        .set("ProductName", "vosu")
        .set("CompanyName", "KotRik")
        .set("LegalCopyright", "© KotRik. All rights reserved.")
        .set("FileDescription", "vosu Rust daemon")
        .set("FileVersion", &text_version)
        .set("ProductVersion", &text_version)
        .set_version_info(VersionInfo::FILEVERSION, numeric_version)
        .set_version_info(VersionInfo::PRODUCTVERSION, numeric_version);
    resource
        .compile()
        .expect("failed to compile Windows resources");
}

#[cfg(windows)]
const fn version_quad(major: u64, minor: u64, patch: u64, build: u64) -> u64 {
    (major << 48) | (minor << 32) | (patch << 16) | build
}
