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
                if std::env::var("PROFILE").as_deref() == Ok("release") {
                    // The checksum only proves the zip is intact. A bundle
                    // prepared from older generator sources still passes it
                    // while failing on templates that need newer generator
                    // code, so release builds also require matching sources.
                    let marker = zip.with_file_name(format!(
                        "{}.inputs.sha256",
                        zip.file_stem().unwrap().to_string_lossy()
                    ));
                    println!("cargo:rerun-if-changed={}", marker.display());
                    let recorded = std::fs::read_to_string(&marker).unwrap_or_default();
                    let current = generator_source_fingerprint();
                    if recorded.trim() != current {
                        panic!(
                            "offset generator bundle {} was prepared from different generator sources; rerun scripts/prepare-offset-generator.py",
                            zip.display()
                        );
                    }
                }
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

/// Mirrors `fingerprint()` in scripts/prepare-offset-generator.py.
fn generator_source_fingerprint() -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut inputs: Vec<PathBuf> = [
        "scripts/prepare-offset-generator.py",
        "tools/lazer-offset-generator/lazer-offset-generator.csproj",
        "tools/lazer-offset-generator/packages.lock.json",
        "tools/lazer-offset-target/lazer-offset-target.csproj",
        "tools/lazer-offset-target/packages.lock.json",
        "LICENSE",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "THIRD_PARTY_NOTICES.md",
    ]
    .iter()
    .map(|relative| root.join(relative))
    .collect();
    for directory in ["tools/lazer-offset-generator", "tools/lazer-offset-target"] {
        let mut sources: Vec<PathBuf> = std::fs::read_dir(root.join(directory))
            .expect("read offset generator sources")
            .map(|entry| entry.expect("offset generator source entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "cs"))
            .collect();
        sources.sort();
        inputs.extend(sources);
    }
    let mut digest = Sha256::new();
    for path in &inputs {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path.strip_prefix(&root).unwrap_or(Path::new(""));
        digest.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        digest.update(std::fs::read(path).expect("read offset generator input"));
    }
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
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

    // Issue #6: use the approved first-party copyright notice.
    // CompanyName remains empty; a copyright notice does not establish a company.
    let mut resource = WindowsResource::new();
    resource
        .set_icon("assets/icon.ico")
        .set_language(0x0409)
        .set("ProductName", "vosu")
        .set("CompanyName", "")
        .set("LegalCopyright", "Copyright (c) 2026 18-LAB")
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
