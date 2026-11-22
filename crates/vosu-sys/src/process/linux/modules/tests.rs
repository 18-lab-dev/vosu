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

#[test]
fn enumerate_modules_does_not_merge_different_files_sharing_a_basename() {
    // F4: two distinct files in different directories, both named
    // `game.exe`, must stay two separate modules rather than collapsing
    // into one span spanning both addresses.
    let dir = TempDir::new().unwrap();
    let pid_dir = dir.path().join("1");
    fs::create_dir_all(&pid_dir).unwrap();
    fs::write(
        pid_dir.join("maps"),
        "\
10000-11000 r--p 00000000 08:01 1 /opt/a/game.exe\n\
20000-21000 r--p 00000000 08:01 2 /opt/b/game.exe\n\
",
    )
    .unwrap();

    let modules = enumerate_modules_at(dir.path(), 1).unwrap();
    assert_eq!(modules.len(), 2);
    let a = modules
        .iter()
        .find(|module| module.base_address == 0x10000)
        .unwrap();
    assert_eq!(a.name, "game.exe");
    assert_eq!(a.size, 0x1000);
    let b = modules
        .iter()
        .find(|module| module.base_address == 0x20000)
        .unwrap();
    assert_eq!(b.name, "game.exe");
    assert_eq!(b.size, 0x1000);
}

#[test]
fn enumerate_modules_anchors_base_on_the_file_offset_zero_mapping_not_the_lowest_address() {
    // F4: the file-offset-0 mapping (0x30000) sits between two other
    // mappings of the same file, one lower (0x10000) and one higher
    // (0x40000); the module's base must be the offset-0 mapping's own
    // start, and its size must span from there, not from the lowest
    // address seen.
    let dir = TempDir::new().unwrap();
    let pid_dir = dir.path().join("1");
    fs::create_dir_all(&pid_dir).unwrap();
    fs::write(
        pid_dir.join("maps"),
        "\
30000-31000 r--p 00000000 08:01 1 /opt/a/game.exe\n\
10000-11000 r--p 00002000 08:01 1 /opt/a/game.exe\n\
40000-41000 r--p 00001000 08:01 1 /opt/a/game.exe\n\
",
    )
    .unwrap();

    let modules = enumerate_modules_at(dir.path(), 1).unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].base_address, 0x30000);
    assert_eq!(modules[0].size, 0x41000 - 0x30000);
}

#[test]
fn enumerate_modules_preserves_single_spaces_inside_a_path() {
    // F5: a path containing single spaces must not have those spaces
    // collapsed, and the module's `name` is still the plain file name.
    let dir = TempDir::new().unwrap();
    let pid_dir = dir.path().join("1");
    fs::create_dir_all(&pid_dir).unwrap();
    fs::write(
        pid_dir.join("maps"),
        "50000-51000 r--p 00000000 08:01 5 /home/user/My Game/game.exe\n",
    )
    .unwrap();

    let modules = enumerate_modules_at(dir.path(), 1).unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "game.exe");
    assert_eq!(modules[0].base_address, 0x50000);
}

#[test]
fn enumerate_modules_strips_the_deleted_suffix_so_the_path_still_resolves() {
    // F5: every mapping of a replaced binary keeps the kernel's
    // `" (deleted)"` marker; stripping it lets the module still match by
    // its real name instead of answering `NotReady` forever.
    let dir = TempDir::new().unwrap();
    let pid_dir = dir.path().join("1");
    fs::create_dir_all(&pid_dir).unwrap();
    fs::write(
        pid_dir.join("maps"),
        "\
50000-51000 r--p 00000000 08:01 5 /opt/a/game.exe (deleted)\n\
51000-52000 r-xp 00001000 08:01 5 /opt/a/game.exe (deleted)\n\
",
    )
    .unwrap();

    let modules = enumerate_modules_at(dir.path(), 1).unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "game.exe");
    assert_eq!(modules[0].base_address, 0x50000);
    assert_eq!(modules[0].size, 0x52000 - 0x50000);
}
