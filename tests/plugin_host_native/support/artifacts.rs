//! The prepared fixture libraries and per-test package staging.
//!
//! `scripts/plugin-host-fixtures.py` (or the gate step running it) builds
//! both example targets, takes each library path from Cargo's JSON
//! `compiler-artifact` record, and exports it. An unset variable or a
//! missing file fails the test: absent native evidence is never a skip.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// One prepared fixture library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    /// `crates/vosu-plugin-testkit/examples/testkit_process_plugin.rs`.
    Testkit,
    /// `crates/vosu-osu/examples/osu_process_plugin.rs`.
    Osu,
}

/// One package staged for one test.
#[derive(Debug)]
pub struct Staged {
    pub dir: PathBuf,
    pub library: PathBuf,
    pub sha256: String,
}

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

impl Artifact {
    /// The environment variable naming the prepared library.
    pub fn variable(self) -> &'static str {
        match self {
            Self::Testkit => "VOSU_PLUGIN_HOST_TESTKIT_LIBRARY",
            Self::Osu => "VOSU_PLUGIN_HOST_OSU_LIBRARY",
        }
    }

    /// The Cargo example target the library was built from.
    pub fn name(self) -> &'static str {
        match self {
            Self::Testkit => "testkit_process_plugin",
            Self::Osu => "osu_process_plugin",
        }
    }

    /// The fixture manifest's directory.
    fn fixture_dir(self) -> PathBuf {
        let leaf = match self {
            Self::Testkit => "testkit",
            Self::Osu => "osu",
        };
        checkout().join("tests/fixtures/plugin_host").join(leaf)
    }

    /// The package assets as (staged relative path, source). The osu!
    /// fixture stages the bundled plugin's own schema, settings and action
    /// files, so its package cannot drift from the in-process one.
    fn assets(self) -> Vec<(&'static str, PathBuf)> {
        match self {
            Self::Testkit => ["namespace.json", "args.json", "result.json"]
                .into_iter()
                .map(|name| (name, self.fixture_dir().join(name)))
                .collect(),
            Self::Osu => {
                let osu = checkout().join("crates/vosu-osu");
                vec![
                    ("settings.schema.json", osu.join("settings.schema.json")),
                    (
                        "schemas/vosu.osu.schema.json",
                        checkout().join("docs/v2/schemas/vosu.osu.schema.json"),
                    ),
                    (
                        "schemas/beatmap-import.args.json",
                        osu.join("schemas/beatmap-import.args.json"),
                    ),
                    (
                        "schemas/beatmap-import.result.json",
                        osu.join("schemas/beatmap-import.result.json"),
                    ),
                ]
            }
        }
    }

    /// The prepared library. Panics when the variable is unset or names
    /// anything but an absolute path to an existing file.
    pub fn library(self) -> PathBuf {
        let variable = self.variable();
        let path = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                panic!(
                    "{variable} is unset: prepare the fixtures first, e.g. \
                 `python scripts/plugin-host-fixtures.py -- cargo test --locked \
                 --features test-harness --test plugin_host_native`"
                )
            });
        assert!(
            path.is_absolute(),
            "{variable}={} is not absolute",
            path.display()
        );
        assert!(
            path.is_file(),
            "{variable}={} is not a file",
            path.display()
        );
        path
    }

    /// The fixture manifest text, with `substitutions` applied in order.
    pub fn manifest(self, substitutions: &[(&str, &str)]) -> String {
        let mut text = std::fs::read_to_string(self.fixture_dir().join("manifest.toml")).unwrap();
        for (from, to) in substitutions {
            assert!(
                text.contains(from),
                "{from:?} is not in the {self:?} manifest"
            );
            text = text.replace(from, to);
        }
        text
    }

    /// Stages a fresh package in `dir` (which must not exist yet): the
    /// manifest and its assets with `substitutions` applied, and the library
    /// under `lib/`, exactly as named by the manifest for this target.
    /// Prints the `RECEIPT` line.
    pub fn stage(self, dir: &Path, substitutions: &[(&str, &str)]) -> Staged {
        assert!(!dir.exists(), "{} is reused", dir.display());
        let source = self.library();
        let file_name = source.file_name().unwrap().to_str().unwrap().to_owned();
        let manifest = self.manifest(substitutions);
        let declared = [
            format!("library = 'lib/{file_name}'"),
            format!("library = \"lib/{file_name}\""),
        ];
        assert!(
            declared.iter().any(|line| manifest.contains(line.as_str())),
            "the {self:?} manifest does not declare lib/{file_name}"
        );
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("manifest.toml"), manifest).unwrap();
        for (relative, source) in self.assets() {
            let target = dir.join(
                substitutions
                    .iter()
                    .fold(relative.to_owned(), |path, (from, to)| {
                        path.replace(from, to)
                    }),
            );
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            let mut text = std::fs::read_to_string(&source)
                .unwrap_or_else(|error| panic!("staging {}: {error}", source.display()));
            for (from, to) in substitutions {
                text = text.replace(from, to);
            }
            std::fs::write(&target, text).unwrap();
        }
        let library = dir.join("lib").join(&file_name);
        std::fs::copy(&source, &library).unwrap();
        let sha256 = sha256(&library);
        assert_eq!(sha256, self::sha256(&source), "the staged copy differs");
        println!(
            "RECEIPT artifact={} sha256={sha256} path={}",
            self.name(),
            library.display()
        );
        Staged {
            dir: dir.to_owned(),
            library,
            sha256,
        }
    }
}

pub fn sha256(path: &Path) -> String {
    let digest = Sha256::digest(std::fs::read(path).unwrap());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
