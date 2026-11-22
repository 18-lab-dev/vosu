use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::TempDir;

use super::*;

fn fixture() -> TempDir {
    tempfile::tempdir().expect("temporary proc fixture")
}

fn add_wine_process(root: &Path, pid: u32, cmdline: &[u8]) {
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).expect("process directory");
    symlink("/usr/bin/wine-preloader", dir.join("exe")).expect("wine exe link");
    symlink(root, dir.join("cwd")).expect("cwd symlink");
    fs::write(dir.join("cmdline"), cmdline).expect("cmdline");
}

#[test]
fn resolves_override_wine_config_and_command_line_paths() {
    let fixture = fixture();
    add_wine_process(
        fixture.path(),
        30,
        b"wine-preloader\0Z:\\\\games\\\\osu!\\\\osu!.exe\0",
    );

    let override_env = [(
        OsString::from("VOSU_OSU_PATH"),
        OsString::from("/override/osu"),
    )];
    assert_eq!(
        resolve_osu_path_at(fixture.path(), 30, &override_env).expect("override"),
        Path::new("/override/osu"),
    );

    let data = fixture.path().join("data");
    fs::create_dir_all(data.join("osuconfig")).expect("osuconfig");
    fs::write(data.join("osuconfig/osupath"), b" /configured/osu \n").expect("osupath");
    let config_env = [(OsString::from("XDG_DATA_HOME"), data.into_os_string())];
    assert_eq!(
        resolve_osu_path_at(fixture.path(), 30, &config_env).expect("wine config"),
        Path::new("/configured/osu"),
    );
    assert_eq!(
        resolve_osu_path_at(fixture.path(), 30, &[]).expect("wine command line"),
        Path::new("/games/osu!"),
    );
}

#[test]
fn non_wine_executable_falls_back_to_cwd() {
    let fixture = fixture();
    let dir = fixture.path().join("31");
    fs::create_dir_all(&dir).expect("process directory");
    let executable = fixture.path().join("osu-native");
    fs::write(&executable, b"native").expect("native executable");
    symlink(&executable, dir.join("exe")).expect("exe symlink");
    symlink(fixture.path(), dir.join("cwd")).expect("cwd symlink");
    fs::write(dir.join("cmdline"), b"osu!\0").expect("cmdline");

    assert_eq!(
        resolve_osu_path_at(fixture.path(), 31, &[]).expect("cwd fallback"),
        fixture.path(),
    );
}
