//! `vosu-package-check`: SDK validation of a whole publisher catalog and its
//! already-extracted packages, for publishers without a vosu checkout
//! (AF.10, `docs/v2/catalog-v2.md` "Package checker").
//!
//! ```text
//! vosu-package-check --catalog <absolute catalog-v2.json>
//!     --packages-root <absolute directory> --host-version <semver>
//!     --source <exact source identity> --registry-context <absolute JSON>
//! ```
//!
//! It is a thin driver over the SDK's own validators, never a second
//! validator: `Catalog::parse` (intrinsic), `Catalog::validate_context`
//! (explicit registrations and official ids), `CatalogEntry::support` and
//! `PackageManifest::validate_context` (host support),
//! `manifest::schema::compile` over each package's actual schema bytes and
//! `catalog::compare_package` (asset-backed mirror equality). Each
//! catalog entry's package for target `T` must already be extracted at
//! `<packages-root>/<plugins|tools|overlays>/<id>/<T>/`; download, archive
//! safety and extraction stay with the publisher's own scripts.
//!
//! Officialness comes only from the registry-context file's
//! `official_sources`, matched exactly against `--source`; a package can
//! never claim it. A pass grants no trust.
#![forbid(unsafe_code)]

mod assets;
mod context;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use semver::Version;
use vosu_plugin_api::catalog::{Catalog, CatalogEntry, compare_package};
use vosu_plugin_api::manifest::schema::{self, CompiledSchema, SchemaProvenance};
use vosu_plugin_api::manifest::{
    HostSupport, PackageKind, PackageManifest, Registration, ValidationContext,
};

use crate::assets::{confined, read_asset};
use crate::context::RegistryContext;

/// Upper bound on a catalog document (vosu's own catalog fetch limit).
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound on a root `manifest.toml` (vosu's own install limit).
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Upper bound on an action or settings schema (vosu's action-schema limit).
const MAX_JSON_SCHEMA_BYTES: usize = 256 * 1024;

const USAGE: &str = "usage: vosu-package-check --catalog <absolute catalog-v2.json> \
--packages-root <absolute directory> --host-version <semver> --source <exact source identity> \
--registry-context <absolute registry-context.json>";

/// Parsed command line; every path is absolute.
#[derive(Debug)]
struct Args {
    catalog: PathBuf,
    packages_root: PathBuf,
    host_version: Version,
    source: String,
    registry_context: PathBuf,
}

/// What one successful run checked.
#[derive(Debug, Default, PartialEq, Eq)]
struct Report {
    packages: usize,
    actions: usize,
    schema_assets: usize,
    /// One line per checked package target, in catalog order.
    artifacts: Vec<String>,
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match parse_args(&raw) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("vosu-package-check: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match check(&args) {
        Ok(report) => {
            for artifact in &report.artifacts {
                println!("checked {artifact}");
            }
            println!(
                "vosu-package-check: passed: {} packages, {} actions, {} schema assets",
                report.packages, report.actions, report.schema_assets
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("vosu-package-check: failed: {message}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut catalog = None;
    let mut packages_root = None;
    let mut host_version = None;
    let mut source = None;
    let mut registry_context = None;
    let mut iter = raw.iter();
    while let Some(flag) = iter.next() {
        let slot = match flag.as_str() {
            "--catalog" => &mut catalog,
            "--packages-root" => &mut packages_root,
            "--host-version" => &mut host_version,
            "--source" => &mut source,
            "--registry-context" => &mut registry_context,
            other => return Err(format!("unknown argument {other:?}")),
        };
        let value = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if slot.replace(value.clone()).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let absolute = |flag: &str, value: Option<String>| -> Result<PathBuf, String> {
        let path = PathBuf::from(value.ok_or_else(|| format!("{flag} is required"))?);
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(format!("{flag} must be an absolute path"))
        }
    };
    let host_version = host_version.ok_or("--host-version is required")?;
    let source = source.ok_or("--source is required")?;
    if source.is_empty() || source.trim() != source {
        return Err("--source must be the exact, non-empty source identity".into());
    }
    Ok(Args {
        catalog: absolute("--catalog", catalog)?,
        packages_root: absolute("--packages-root", packages_root)?,
        host_version: Version::parse(&host_version)
            .map_err(|_| format!("--host-version {host_version:?} is not a semver version"))?,
        source,
        registry_context: absolute("--registry-context", registry_context)?,
    })
}

fn array_name(kind: PackageKind) -> &'static str {
    match kind {
        PackageKind::Plugin => "plugins",
        PackageKind::Tool => "tools",
        PackageKind::Overlay => "overlays",
    }
}

/// Runs every SDK validation layer over the catalog and each extracted
/// package target; the first failure is returned with its location.
fn check(args: &Args) -> Result<Report, String> {
    let context = RegistryContext::load(&args.registry_context)?;
    let catalog_text = assets::read_bounded(&args.catalog, MAX_CATALOG_BYTES)
        .map_err(|e| format!("catalog {}: {e}", args.catalog.display()))?;
    let catalog = Catalog::parse(&catalog_text).map_err(|e| format!("catalog: {e}"))?;

    let official = context.is_official(&args.source);
    let official_ids: Vec<&str> = if official {
        catalog
            .entries()
            .map(|(_, entry)| entry.id.as_str())
            .collect()
    } else {
        Vec::new()
    };
    let base_registrations = context.registrations();
    let revisions = context.namespace_revisions()?;
    let validation = ValidationContext {
        application_version: &args.host_version,
        source: &args.source,
        official,
        registrations: &base_registrations,
        namespace_revisions: &revisions,
    };
    catalog
        .validate_context(&validation, &official_ids)
        .map_err(|e| format!("catalog context: {e}"))?;

    // The same complete set `Catalog::validate_context` validates each entry
    // against: the context's registrations plus this whole catalog.
    let mut registrations = base_registrations.clone();
    registrations.extend(catalog.entries().map(|(kind, entry)| Registration {
        kind,
        id: &entry.id,
        source: &args.source,
        official,
        builtin: false,
        replaces: &entry.replaces,
    }));
    let package_context = ValidationContext {
        registrations: &registrations,
        ..validation
    };
    let provenance = if official {
        SchemaProvenance::TrustedOfficial
    } else {
        SchemaProvenance::Untrusted
    };
    let packages = Packages {
        root: &args.packages_root,
        context: &package_context,
        provenance,
        common_defs: context.common_defs(),
    };

    let mut report = Report::default();
    for (kind, entry) in catalog.entries() {
        for target in entry.packages.keys() {
            let support = entry
                .support(kind, &args.host_version, target)
                .map_err(|e| format!("{}/{}: {e}", array_name(kind), entry.id))?;
            if !support.supported() {
                return Err(format!(
                    "{}/{}: host range {:?} does not admit {} (host-support)",
                    array_name(kind),
                    entry.id,
                    entry.host,
                    args.host_version
                ));
            }
            packages.check(kind, entry, target, &mut report)?;
        }
    }
    if report.packages == 0 {
        return Err("the catalog lists no packages; nothing was checked".into());
    }
    Ok(report)
}

/// The fixed inputs every package target is checked against.
struct Packages<'a> {
    root: &'a Path,
    context: &'a ValidationContext<'a>,
    provenance: SchemaProvenance,
    common_defs: &'a serde_json::Value,
}

impl Packages<'_> {
    fn check(
        &self,
        kind: PackageKind,
        entry: &CatalogEntry,
        target: &str,
        report: &mut Report,
    ) -> Result<(), String> {
        let label = format!("{}/{}/{target}", array_name(kind), entry.id);
        let fail = |message: String| format!("{label}: {message}");
        let root = assets::package_root(self.root, &[array_name(kind), &entry.id, target])
            .map_err(fail)?;
        let text = read_asset(&root, "manifest.toml", MAX_MANIFEST_BYTES).map_err(fail)?;
        let manifest = PackageManifest::parse(&text).map_err(|e| fail(e.to_string()))?;
        let fields = manifest.fields();
        // Name a wrong package before deeper checks, as the installer does.
        if manifest.kind() != kind {
            return Err(fail("catalog.mirror at /kind".into()));
        }
        if fields.id != entry.id {
            return Err(fail("catalog.mirror at /id".into()));
        }
        if fields.version != entry.version {
            return Err(fail("catalog.mirror at /version".into()));
        }
        match manifest.validate_context(self.context) {
            Ok(HostSupport::Supported) => {}
            Ok(HostSupport::Unsupported) => {
                return Err(fail("manifest host range excludes --host-version".into()));
            }
            Err(e) => return Err(fail(e.to_string())),
        }

        let mut schema_assets = 0;
        let mut compiled: Vec<CompiledSchema> = Vec::new();
        for provide in fields.provides {
            let Some(path) = &provide.schema else {
                continue;
            };
            let namespace = provide
                .id
                .strip_prefix("field:")
                .ok_or_else(|| fail("catalog.mirror at /provides".into()))?;
            let version = Version::parse(&provide.version)
                .map_err(|_| fail(format!("{path}: provide version is not semver")))?;
            let schema_text = read_asset(&root, path, schema::MAX_BYTES).map_err(fail)?;
            compiled.push(
                schema::compile(
                    namespace,
                    &version,
                    self.provenance,
                    &schema_text,
                    self.common_defs,
                )
                .map_err(|e| fail(format!("{path}: {e}")))?,
            );
            schema_assets += 1;
        }
        compare_package(kind, entry, &manifest, &compiled).map_err(|e| fail(e.to_string()))?;
        for art in [fields.icon, fields.banner].into_iter().flatten() {
            confined(&root, art).map_err(fail)?;
        }

        let mut detail = String::new();
        let mut actions = 0;
        match &manifest {
            PackageManifest::Plugin(plugin) => {
                let library = plugin
                    .targets
                    .get(target)
                    .ok_or_else(|| fail(format!("manifest has no [targets.{target}]")))?;
                confined(&root, &library.library).map_err(fail)?;
                let _ = write!(detail, "library {}", library.library);
                if let Some(path) = &plugin.settings_schema {
                    json_schema(&root, path).map_err(fail)?;
                    schema_assets += 1;
                }
                for action in &plugin.actions {
                    for path in [&action.args_schema, &action.result_schema] {
                        json_schema(&root, path).map_err(fail)?;
                        schema_assets += 1;
                    }
                    actions += 1;
                }
            }
            PackageManifest::Tool(tool) => {
                let entry = format!("{}/{}", tool.ui.root, tool.ui.entry);
                confined(&root, &entry).map_err(fail)?;
                let _ = write!(detail, "ui {entry}");
            }
            PackageManifest::Overlay(_) => {
                confined(&root, "index.html").map_err(fail)?;
                detail.push_str("ui index.html");
            }
        }
        report.packages += 1;
        report.actions += actions;
        report.schema_assets += schema_assets;
        report.artifacts.push(format!(
            "{label}: {detail}, {actions} actions, {schema_assets} schema assets"
        ));
        Ok(())
    }
}

/// An action or settings schema must exist inside the package and be one
/// bounded JSON document. Its bounded action-schema profile is compiled by
/// the host at install time; the SDK carries no second copy of that
/// compiler.
fn json_schema(root: &Path, path: &str) -> Result<(), String> {
    let text = read_asset(root, path, MAX_JSON_SCHEMA_BYTES)?;
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Object(_) | serde_json::Value::Bool(_)) => Ok(()),
        _ => Err(format!("{path}: not a JSON schema document")),
    }
}
