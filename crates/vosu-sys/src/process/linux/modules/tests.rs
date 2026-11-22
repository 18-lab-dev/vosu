use std::fs;

use tempfile::TempDir;

use super::*;

#[test]
fn enumerate_modules_finds_libc_in_the_test_process_itself() {
    let pid = std::process::id();
    let modules = enumerate_modules(pid).expect("enumerate modules for the running test process");
    assert!(!modules.is_empty());
    let libc = modules
        .iter()
        .find(|module| module.name.starts_with("libc"));
    let Some(libc) = libc else {
        // A fully static test binary maps no shared libc; skip rather
        // than fail an environment-dependent assumption.
        return;
    };
    assert!(libc.base_address > 0);
    assert!(libc.size > 0);
    // Every other named module has a distinct base address from libc's.
    for other in &modules {
        if other.name != libc.name {
            assert_ne!(other.base_address, libc.base_address);
        }
    }
}

#[test]
fn enumerate_modules_groups_every_mapping_of_the_same_file_into_one_base_and_span() {
    let dir = TempDir::new().unwrap();
    let pid_dir = dir.path().join("1");
    fs::create_dir_all(&pid_dir).unwrap();
    fs::write(
        pid_dir.join("maps"),
        "\
55a000-55b000 r--p 00000000 08:01 1 /usr/lib/libfoo.so.1\n\
55b000-55c000 r-xp 00001000 08:01 1 /usr/lib/libfoo.so.1\n\
600000-601000 rw-p 00000000 00:00 0 \n\
700000-701000 r--p 00000000 08:01 2 /usr/lib/libbar.so\n\
",
    )
    .unwrap();

    let modules = enumerate_modules_at(dir.path(), 1).unwrap();
    let foo = modules
        .iter()
        .find(|module| module.name == "libfoo.so.1")
        .unwrap();
    assert_eq!(foo.base_address, 0x55a000);
    assert_eq!(foo.size, 0x55c000 - 0x55a000);
    let bar = modules
        .iter()
        .find(|module| module.name == "libbar.so")
        .unwrap();
    assert_eq!(bar.base_address, 0x700000);
    assert_eq!(bar.size, 0x1000);
    // the anonymous mapping (no backing file) is not reported as a module
    assert_eq!(modules.len(), 2);
}
