use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};
use vosu_sys::platform::{PathResolver, Paths};

pub(super) struct RuntimeTestPathResolver {
    pub(super) executable: PathBuf,
    pub(super) launch: PathBuf,
    pub(super) root: PathBuf,
}

impl PathResolver for RuntimeTestPathResolver {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }

    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.launch.clone())
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        let directory = match name {
            "HOME" => "home",
            "XDG_CONFIG_HOME" => "config",
            "XDG_DATA_HOME" => "data",
            "XDG_CACHE_HOME" => "cache",
            _ => return None,
        };
        Some(self.root.join(directory).into_os_string())
    }
}

pub(super) fn runtime_test_paths(root: &Path) -> Paths {
    let program = root.join("program");
    let launch = root.join("launch");
    fs::create_dir_all(&program).unwrap();
    fs::create_dir_all(&launch).unwrap();
    let paths = Paths::resolve(
        &RuntimeTestPathResolver {
            executable: program.join("vosu"),
            launch,
            root: root.to_owned(),
        },
        vosu_sys::platform::BuildChannel::Release,
    )
    .unwrap();
    paths.initialize().unwrap();
    paths
}
