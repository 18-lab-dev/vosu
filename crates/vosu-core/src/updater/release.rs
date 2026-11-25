use super::*;

#[derive(Deserialize)]
pub(super) struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
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
/// automatic updater rather than being interpreted as semver. A newer release
/// whose major version exceeds `current`'s is never installable without
/// `major_consent`: without it, it is reported as `MajorAvailable` instead of
/// being validated against its release asset.
pub fn parse_latest_release(
    json: &[u8],
    current: &Version,
    platform: &str,
    major_consent: bool,
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
    if version.major > current.major && !major_consent {
        return Ok(ReleaseDecision::MajorAvailable(MajorRelease {
            version,
            release_url: release.html_url,
        }));
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
#[expect(
    clippy::too_many_arguments,
    reason = "this public entry point keeps its explicit startup-policy and update inputs for API stability"
)]
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
    check_release_and_prepare(ReleaseCheckRequest {
        prepare_available: enabled,
        // Startup never has an interactive consent step; a major release is
        // always just reported here, never installed.
        major_consent: false,
        program_dir,
        target,
        current,
        platform_name,
        release_api,
        platform,
        client,
        cancel,
    })
    .await
}

/// Explicit update requests always consult the authoritative release endpoint,
/// including in development builds where a local harness may exercise the
/// route. Startup callers should use [`check_startup_and_prepare`] instead.
#[expect(
    clippy::too_many_arguments,
    reason = "this public entry point keeps its explicit update inputs for API stability"
)]
pub async fn check_latest_and_prepare<'a, P: UpdatePlatform>(
    program_dir: &Path,
    target: &Path,
    current: &Version,
    platform_name: &str,
    release_api: &str,
    platform: &'a P,
    client: &StreamingClient,
    cancel: &CancellationToken,
    major_consent: bool,
) -> Result<UpdateCheck<'a, P>, UpdateError> {
    check_release_and_prepare(ReleaseCheckRequest {
        prepare_available: true,
        major_consent,
        program_dir,
        target,
        current,
        platform_name,
        release_api,
        platform,
        client,
        cancel,
    })
    .await
}

/// Grouped inputs for [`check_release_and_prepare`]; keeps policy forwarding
/// explicit and aligned with the two public entry points without a long
/// positional parameter list.
pub(super) struct ReleaseCheckRequest<'a, 'b, P: UpdatePlatform> {
    pub(super) prepare_available: bool,
    /// Explicit consent to install a release whose major version exceeds
    /// the running build's; startup never supplies it.
    pub(super) major_consent: bool,
    pub(super) program_dir: &'b Path,
    pub(super) target: &'b Path,
    pub(super) current: &'b Version,
    pub(super) platform_name: &'b str,
    pub(super) release_api: &'b str,
    pub(super) platform: &'a P,
    pub(super) client: &'b StreamingClient,
    pub(super) cancel: &'b CancellationToken,
}

pub(super) async fn check_release_and_prepare<'a, 'b, P: UpdatePlatform>(
    request: ReleaseCheckRequest<'a, 'b, P>,
) -> Result<UpdateCheck<'a, P>, UpdateError> {
    let ReleaseCheckRequest {
        prepare_available,
        major_consent,
        program_dir,
        target,
        current,
        platform_name,
        release_api,
        platform,
        client,
        cancel,
    } = request;
    if is_forced_version(current) {
        return Ok(UpdateCheck::SuppressedForced);
    }
    let guard = TransactionGuard::begin(program_dir, target, platform)?;
    let metadata_path = guard.record.work.join("release.json");
    let user_agent = user_agent(current);
    client
        .download_atomic(release_api, &user_agent, &metadata_path, cancel)
        .await?;
    if fs::metadata(&metadata_path)?.len() > 1024 * 1024 {
        return Err(UpdateError::Metadata(
            "release metadata exceeds 1 MiB".into(),
        ));
    }
    // Reads this task's own just-downloaded `metadata_path` strictly after
    // its own `download_atomic` call returned: no other writer can touch
    // this transaction-scoped path, so this is the same same-task-after-
    // own-publish exclusion as `prepare.rs`'s staging reads (see the
    // exclusion list on `vosu_sys::platform::read_published`'s doc
    // comment); unlike those, this one is small enough to read whole here.
    let body = fs::read(&metadata_path)?;
    fs::remove_file(&metadata_path)?;
    match parse_latest_release(&body, current, platform_name, major_consent)? {
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
        ReleaseDecision::MajorAvailable(release) => {
            guard.cancel()?;
            Ok(UpdateCheck::MajorAvailable(release))
        }
        ReleaseDecision::Update(release) if prepare_available => Ok(UpdateCheck::Prepared(
            prepare_update(guard, release, client, &user_agent, cancel).await?,
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
