use super::BuildChannel;
use crate::platform::{PathResolver, Paths};
use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf};

struct Resolver {
    executable: PathBuf,
    env: BTreeMap<String, OsString>,
}
impl PathResolver for Resolver {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }
    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.executable.parent().unwrap().join("launch"))
    }
    fn env_var(&self, name: &str) -> Option<OsString> {
        self.env.get(name).cloned()
    }
}
fn resolver(root: &std::path::Path) -> Resolver {
    Resolver {
        executable: root.join("program/vosu.exe"),
        env: [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "LOCALAPPDATA",
        ]
        .into_iter()
        .map(|key| (key.to_owned(), root.join(key).into_os_string()))
        .collect(),
    }
}

#[test]
fn executable_semver_selects_channel_not_build_profile() {
    for version in ["1.2.5", "2.0.0", "2.0.0+alpha.7"] {
        assert_eq!(
            BuildChannel::from_version(version).unwrap(),
            BuildChannel::Release
        );
    }
    for version in ["2.0.0-alpha.1", "2.0.0-beta.9+build.3", "2.0.0-rc.1"] {
        assert_eq!(
            BuildChannel::from_version(version).unwrap(),
            BuildChannel::Prerelease
        );
    }
    assert!(BuildChannel::from_version("not-a-version").is_err());
}

#[test]
fn channel_roots_and_broker_are_pure_and_disjoint() {
    let temp = tempfile::tempdir().unwrap();
    let input = resolver(temp.path());
    let release = Paths::resolve(&input, BuildChannel::Release).unwrap();
    let alpha = Paths::resolve(&input, BuildChannel::Prerelease).unwrap();
    for (normal, isolated) in [
        (release.config(), alpha.config()),
        (release.data(), alpha.data()),
        (release.cache(), alpha.cache()),
    ] {
        assert_eq!(isolated, normal.join("prerelease"));
        assert!(!normal.exists(), "resolution must not create directories");
    }
    let normal_broker = release.broker_path_with(&input).unwrap();
    let alpha_broker = alpha.broker_path_with(&input).unwrap();
    assert_eq!(
        alpha_broker,
        normal_broker
            .parent()
            .unwrap()
            .join("prerelease/broker.json")
    );
    assert!(!normal_broker.parent().unwrap().exists());
    alpha.initialize().unwrap();
    assert!(alpha.config().is_dir());
    assert!(alpha.data().is_dir());
    assert!(alpha.cache().is_dir());
    assert!(!release.config().join("vosu.toml").exists());
    assert!(!release.cache().join("offsets").exists());
    assert_eq!(alpha.static_path("").unwrap(), alpha.data().join("static"));
}

#[cfg(target_os = "linux")]
#[test]
fn prerelease_never_migrates_legacy_roots_and_broker_fallback_is_isolated() {
    let temp = tempfile::tempdir().unwrap();
    let mut input = resolver(temp.path());
    input.env.remove("XDG_RUNTIME_DIR");
    // Legacy pre-rename state must remain untouched by alpha initialization.
    let legacy = temp.path().join("XDG_CONFIG_HOME/tosu");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("sentinel"), b"legacy-original").unwrap();
    let alpha = Paths::resolve(&input, BuildChannel::Prerelease).unwrap();
    assert!(!alpha.config().exists());
    alpha.initialize().unwrap();
    assert_eq!(
        std::fs::read(legacy.join("sentinel")).unwrap(),
        b"legacy-original"
    );
    assert!(!alpha.config().join("sentinel").exists());
    assert!(!alpha.config().parent().unwrap().join("sentinel").exists());
    assert_eq!(
        alpha.broker_path_with(&input).unwrap(),
        alpha.config().join("broker.json")
    );
}

#[cfg(windows)]
#[test]
fn windows_portable_roots_and_localappdata_broker_follow_channel() {
    let temp = tempfile::tempdir().unwrap();
    let mut input = resolver(temp.path());
    let alpha = Paths::resolve(&input, BuildChannel::Prerelease).unwrap();
    assert_eq!(alpha.config(), temp.path().join("program/prerelease"));
    assert_eq!(alpha.data(), temp.path().join("program/prerelease"));
    assert_eq!(alpha.cache(), temp.path().join("program/.cache/prerelease"));
    assert_eq!(
        alpha.broker_path_with(&input).unwrap(),
        temp.path().join("LOCALAPPDATA/vosu/prerelease/broker.json")
    );
    for value in [OsString::new(), OsString::from("relative")] {
        input.env.insert("LOCALAPPDATA".into(), value);
        assert!(alpha.broker_path_with(&input).is_err());
    }
    input.env.remove("LOCALAPPDATA");
    assert!(alpha.broker_path_with(&input).is_err());
}
