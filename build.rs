fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TEST_HARNESS");
    println!("cargo:rerun-if-env-changed=PROFILE");

    if std::env::var_os("CARGO_FEATURE_TEST_HARNESS").is_some()
        && std::env::var("PROFILE").as_deref() == Ok("release")
    {
        panic!("the test-harness feature is forbidden in release builds");
    }

    #[cfg(windows)]
    embed_windows_resource();
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
