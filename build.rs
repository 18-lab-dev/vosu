fn main() {
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

    let mut resource = WindowsResource::new();
    resource
        .set_icon("assets/icon.ico")
        .set_language(0x0409)
        .set("ProductName", "vosu")
        .set("CompanyName", "KotRik")
        .set("LegalCopyright", "© KotRik. All rights reserved.")
        .set("FileDescription", "vosu Rust daemon")
        .set("FileVersion", "0.1.0.0")
        .set("ProductVersion", "0.1.0.0")
        .set_version_info(VersionInfo::FILEVERSION, version_quad(0, 1, 0, 0))
        .set_version_info(VersionInfo::PRODUCTVERSION, version_quad(0, 1, 0, 0));
    resource
        .compile()
        .expect("failed to compile Windows resources");
}

#[cfg(windows)]
const fn version_quad(major: u64, minor: u64, patch: u64, build: u64) -> u64 {
    (major << 48) | (minor << 32) | (patch << 16) | build
}
