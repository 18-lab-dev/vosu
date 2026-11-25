//! D15 fixture description written by `scripts/test-catalog-forge.py`.
//!
//! The runner generates a throwaway internal CA and exact-hostname leaves,
//! provisions Forgejo and installs the CA into the *operating system* trust
//! store of the environment that runs these tests. Tests read the CA file only
//! to assert that precondition; no client ever receives it as a root.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use serde_json::Value;

pub const FIXTURE_ENV: &str = "VOSU_X1_FIXTURE";
const RUNNER: &str = "python scripts/test-catalog-forge.py";

/// Shared contract with `scripts/test-catalog-forge.py` provisioning.
pub const OWNER: &str = "vosu-x1-owner";
pub const PRIVATE_REPO: &str = "private-catalog";
pub const PRIVATE_TAG: &str = "v1.0.0";
pub const SAME_ORIGIN_REPO: &str = "same-origin-redirect";
pub const SAME_ORIGIN_TAG: &str = "v2.0.0";
pub const CROSS_PORT_REPO: &str = "cross-port-redirect";
pub const CROSS_PORT_TAG: &str = "v3.0.0";
pub const FOREIGN_REDIRECT_REPO: &str = "foreign-redirect";
pub const FOREIGN_REDIRECT_TAG: &str = "v4.0.0";
pub const FOREIGN_ASSET_REPO: &str = "foreign-asset";
pub const PACKAGE_ASSET: &str = "x1-package.zip";
pub const ICON_ASSET: &str = "x1-icon.png";
pub const TOOL_ID: &str = "x1-private-tool";
pub const TOOL_VERSION: &str = "3.1.4";

pub struct Leaf {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Fixture-only secrets. Intentionally no Debug: assertion output must never
/// print them, and the runner scans every captured log for them.
pub struct Secrets {
    pub token: String,
    pub username: String,
    pub password: String,
}

pub struct ForgeFixture {
    pub upstream: String,
    pub https_port: u16,
    pub cross_port: u16,
    pub version: String,
    pub secrets: Secrets,
    pub package: Vec<u8>,
}

pub struct Fixture {
    pub forge_host: String,
    pub mirror_host: String,
    pub wrong_host: String,
    pub ca_cert: PathBuf,
    pub forge_leaf: Leaf,
    pub mirror_leaf: Leaf,
    forge: Option<ForgeFixture>,
}

fn prerequisite(message: impl std::fmt::Display) -> ! {
    panic!(
        "D15 forge prerequisite missing: {message}. These ignored tests need the real \
         Forgejo/internal-CA fixture with the CA in the OS trust store; run `{RUNNER}` \
         (or `{RUNNER} --windows-vm` for the native Windows store) instead of invoking \
         them directly."
    )
}

fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| prerequisite(format_args!("fixture field {pointer} is missing")))
        .to_owned()
}

fn port(value: &Value, pointer: &str) -> u16 {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or_else(|| prerequisite(format_args!("fixture field {pointer} is not a port")))
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| {
        prerequisite(format_args!("cannot read {}: {error}", path.display()))
    })
}

impl Fixture {
    pub fn load() -> Self {
        let path = std::env::var_os(FIXTURE_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| prerequisite(format_args!("{FIXTURE_ENV} is not set")));
        let base = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| prerequisite("fixture path has no directory"));
        let json: Value = serde_json::from_slice(&read(&path))
            .unwrap_or_else(|error| prerequisite(format_args!("invalid fixture JSON: {error}")));
        let file = |pointer: &str| base.join(text(&json, pointer));
        let forge = json.get("forge").filter(|value| !value.is_null()).map(|_| {
            let secrets: Value = serde_json::from_slice(&read(&file("/forge/secrets")))
                .unwrap_or_else(|_| prerequisite("invalid fixture secrets"));
            ForgeFixture {
                upstream: text(&json, "/forge/upstream"),
                https_port: port(&json, "/forge/https_port"),
                cross_port: port(&json, "/forge/cross_port"),
                version: text(&json, "/forge/version"),
                secrets: Secrets {
                    token: text(&secrets, "/token"),
                    username: text(&secrets, "/username"),
                    password: text(&secrets, "/password"),
                },
                package: read(&file("/forge/package")),
            }
        });
        Self {
            forge_host: text(&json, "/hosts/forge"),
            mirror_host: text(&json, "/hosts/mirror"),
            wrong_host: text(&json, "/hosts/wrong"),
            ca_cert: file("/ca_cert"),
            forge_leaf: Leaf {
                cert: file("/forge_leaf/cert"),
                key: file("/forge_leaf/key"),
            },
            mirror_leaf: Leaf {
                cert: file("/mirror_leaf/cert"),
                key: file("/mirror_leaf/key"),
            },
            forge,
        }
    }

    pub fn forge(&self) -> &ForgeFixture {
        self.forge.as_ref().unwrap_or_else(|| {
            prerequisite(
                "this fixture has no Forgejo section (the Windows bundle is os_store-only)",
            )
        })
    }
}

/// Each test owns its recorder listeners on the fixture's fixed ports, so the
/// fixture tests run one at a time regardless of the harness thread count.
static SERIAL: Mutex<()> = Mutex::new(());

pub fn run<F, Fut>(test: F)
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = ()>,
{
    let fixture = Fixture::load();
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("fixture runtime")
        .block_on(test(fixture));
}

/// Asserts how the environment's OS trust store was provisioned, so a runner
/// that forgot to install (or to omit) the CA cannot produce a false result.
/// Client code never reads this file; only the platform verifier consults the
/// store.
pub fn assert_os_store(fixture: &Fixture, installed: bool) {
    for variable in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
        assert!(
            std::env::var_os(variable).is_none(),
            "{variable} would replace the OS trust store with a custom root"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let compact = |bytes: Vec<u8>| -> String {
            String::from_utf8(bytes)
                .expect("PEM is ASCII")
                .lines()
                .filter(|line| !line.starts_with("-----"))
                .collect::<String>()
        };
        let ca = compact(read(&fixture.ca_cert));
        let bundle = std::fs::read("/etc/ssl/certs/ca-certificates.crt")
            .unwrap_or_else(|error| prerequisite(format_args!("no OS CA bundle: {error}")));
        let roots = bundle
            .windows(b"BEGIN CERTIFICATE".len())
            .filter(|window| *window == b"BEGIN CERTIFICATE")
            .count();
        assert_eq!(
            compact(bundle).contains(&ca),
            installed,
            "fixture CA presence in the OS trust store must be {installed}"
        );
        assert!(roots > 1, "the OS store must retain its public roots");
    }
    // Native Windows: the runner verifies `Cert:\LocalMachine\Root` membership
    // for the CA file before each phase; the bundle check above is Linux-only.
    #[cfg(not(target_os = "linux"))]
    let _ = (&fixture.ca_cert, installed);
}
