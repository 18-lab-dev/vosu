#[path = "../build_support/windows_manifest.rs"]
mod windows_manifest;

#[test]
fn embedded_manifest_keeps_cli_subsystem_and_detaches_graphical_launches() {
    let manifest = windows_manifest::WINDOWS_MANIFEST;
    assert!(manifest.contains("<consoleAllocationPolicy xmlns=\"http://schemas.microsoft.com/SMI/2024/WindowsSettings\">detached</consoleAllocationPolicy>"));
    assert!(manifest.contains("requestedExecutionLevel level=\"asInvoker\" uiAccess=\"false\""));
    assert_eq!(manifest.matches("<assembly ").count(), 1);
    assert!(
        include_str!("../build.rs").contains(".set_manifest(windows_manifest::WINDOWS_MANIFEST)")
    );
    assert!(!include_str!("../src/main.rs").contains("windows_subsystem"));
}
