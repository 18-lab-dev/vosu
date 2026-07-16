use super::*;

#[derive(Deserialize)]
pub(super) struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
pub(super) struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

/// Validate GitHub's authoritative latest-release response.
///
/// Equality and downgrade both return `Latest`; only a strictly newer stable
/// version is installable.  The historical `-forced` marker suppresses the
/// automatic updater rather than being interpreted as semver.
pub fn parse_latest_release(
    json: &[u8],
    current: &Version,
    platform: &str,
) -> Result<ReleaseDecision, UpdateError> {
    let release: GithubRelease = serde_json::from_slice(json)?;
    if current.to_string().contains("-forced") {
        return Ok(ReleaseDecision::SuppressedForced);
    }
    if release.draft || release.prerelease {
        return Ok(ReleaseDecision::Latest(current.clone()));
    }
    let tag = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    if tag.starts_with('v') {
        return Err(UpdateError::Metadata(
            "tag has more than one leading v".into(),
        ));
    }
    let version = Version::parse(tag)
        .map_err(|error| UpdateError::Metadata(format!("invalid tag_name: {error}")))?;
    if !version.pre.is_empty() {
        return Ok(ReleaseDecision::Latest(current.clone()));
    }
    if version <= *current {
        return Ok(ReleaseDecision::Latest(version));
    }

    let expected = format!("vosu-{platform}-v{version}.zip");
    let mut matches = release.assets.iter().filter(|asset| asset.name == expected);
    let Some(asset) = matches.next() else {
        return Ok(ReleaseDecision::NoFiles);
    };
    if matches.next().is_some() {
        return Err(UpdateError::Metadata(
            "duplicate exact release asset".into(),
        ));
    }
    let digest_text = asset
        .digest
        .as_deref()
        .ok_or_else(|| UpdateError::Metadata("release asset has no SHA-256 digest".into()))?;
    let digest_text = digest_text
        .strip_prefix("sha256:")
        .ok_or_else(|| UpdateError::Metadata("asset digest is not sha256".into()))?;
    let digest = decode_digest(digest_text)?;
    Ok(ReleaseDecision::Update(ValidatedRelease {
        version,
        archive_url: asset.browser_download_url.clone(),
        digest,
        asset_name: expected,
    }))
}

pub(super) fn decode_digest(value: &str) -> Result<[u8; 32], UpdateError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UpdateError::Metadata("malformed SHA-256 digest".into()));
    }
    let mut output = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).expect("ASCII was checked");
        output[index] = u8::from_str_radix(text, 16)
            .map_err(|_| UpdateError::Metadata("malformed SHA-256 digest".into()))?;
    }
    Ok(output)
}
/// Apply the source startup policy without weakening the explicit update route:
/// development/test builds publish the package version without touching the
/// network, while production always checks the authoritative release and only
/// prepares it when the effective `--update`/configuration decision is true.
// This public entry point keeps its explicit startup-policy and update inputs for API stability.
#[allow(clippy::too_many_arguments)]
pub async fn check_startup_and_prepare<'a, P: UpdatePlatform>(
    enabled: bool,
    program_dir: &Path,
    target: &Path,
    current: &Version,
    platform_name: &str,
    release_api: &str,
    platform: &'a P,
    client: &StreamingClient,
    cancel: &CancellationToken,
) -> Result<UpdateCheck<'a, P>, UpdateError> {
    if !startup_uses_remote_release() {
        return Ok(UpdateCheck::Latest(current.clone()));
    }
    check_release_and_prepare(
        enabled,
        program_dir,
        target,
        current,
        platform_name,
        release_api,
        platform,
        client,
        cancel,
    )
    .await
}

/// Explicit update requests always consult the authoritative release endpoint,
/// including in development builds where a local harness may exercise the
/// route. Startup callers should use [`check_startup_and_prepare`] instead.
// This public entry point keeps its explicit update inputs for API stability.
#[allow(clippy::too_many_arguments)]
pub async fn check_latest_and_prepare<'a, P: UpdatePlatform>(
    program_dir: &Path,
    target: &Path,
    current: &Version,
    platform_name: &str,
    release_api: &str,
    platform: &'a P,
    client: &StreamingClient,
    cancel: &CancellationToken,
) -> Result<UpdateCheck<'a, P>, UpdateError> {
    check_release_and_prepare(
        true,
        program_dir,
        target,
        current,
        platform_name,
        release_api,
        platform,
        client,
        cancel,
    )
    .await
}

// Keep policy forwarding explicit and aligned with the two public entry points.
#[allow(clippy::too_many_arguments)]
pub(super) async fn check_release_and_prepare<'a, P: UpdatePlatform>(
    prepare_available: bool,
    program_dir: &Path,
    target: &Path,
    current: &Version,
    platform_name: &str,
    release_api: &str,
    platform: &'a P,
    client: &StreamingClient,
    cancel: &CancellationToken,
) -> Result<UpdateCheck<'a, P>, UpdateError> {
    if is_forced_version(current) {
        return Ok(UpdateCheck::SuppressedForced);
    }
    let guard = TransactionGuard::begin(program_dir, target, platform)?;
    let metadata_path = guard.record.work.join("release.json");
    client
        .download_atomic(release_api, USER_AGENT, &metadata_path, cancel)
        .await?;
    if fs::metadata(&metadata_path)?.len() > 1024 * 1024 {
        return Err(UpdateError::Metadata(
            "release metadata exceeds 1 MiB".into(),
        ));
    }
    let body = fs::read(&metadata_path)?;
    fs::remove_file(&metadata_path)?;
    match parse_latest_release(&body, current, platform_name)? {
        ReleaseDecision::Latest(version) => {
            guard.cancel()?;
            Ok(UpdateCheck::Latest(version))
        }
        ReleaseDecision::NoFiles => {
            guard.cancel()?;
            Ok(UpdateCheck::NoFiles)
        }
        ReleaseDecision::SuppressedForced => {
            guard.cancel()?;
            Ok(UpdateCheck::SuppressedForced)
        }
        ReleaseDecision::Update(release) if prepare_available => Ok(UpdateCheck::Prepared(
            prepare_update(guard, release, client, cancel).await?,
        )),
        ReleaseDecision::Update(release) => {
            let version = release.version;
            guard.cancel()?;
            Ok(UpdateCheck::Available(version))
        }
    }
}

pub(super) fn startup_uses_remote_release() -> bool {
    !cfg!(any(test, debug_assertions))
}

pub(super) fn is_forced_version(current: &Version) -> bool {
    current.to_string().contains("-forced")
}
