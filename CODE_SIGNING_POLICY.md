# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

**Status:** signing is not active yet. Until SignPath Foundation approves the project and the repository's SignPath settings exist, releases ship the same Windows executable unsigned; each release's notes say whether it is signed.

## Team roles

The vosu team is the owners of the [18-LAB](https://github.com/18-lab-dev) GitHub organization, which owns the [source repository](https://github.com/18-lab-dev/vosu) and the SignPath project.

- **Committers and reviewers:** [K4zoku](https://github.com/K4zoku), [qt715](https://github.com/qt715) ([organization members](https://github.com/orgs/18-lab-dev/people))
- **Approvers:** [K4zoku](https://github.com/K4zoku), [qt715](https://github.com/qt715)

Committers may change the source without additional review. A change proposed by anyone else, such as a pull request from a contributor, is merged only after a committer reviews it. Reviews pay particular attention to build scripts, `.github/workflows/`, and dependency changes, because they decide what a signed binary contains.

Every team member must use multi-factor authentication for GitHub and SignPath.

## What is signed

Only `vosu.exe`, the Windows executable inside `vosu-windows-<tag>.zip` on the [GitHub releases](https://github.com/18-lab-dev/vosu/releases) page, is signed with the SignPath Foundation certificate. It is built from this repository's own source.

`vosu.exe` embeds the lazer offset-generator payload. The release workflow builds that payload from `tools/lazer-offset-generator` and `tools/lazer-offset-target` together with the self-contained Microsoft .NET runtime and ClrMD packages listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Those upstream files are packaged inside the executable and are not signed separately.

The following are not signed with this certificate: Linux archives and packages, the unsigned development ZIPs from the **Unsigned version builds** workflow, local builds, and community overlays that the dashboard installs from the tosu catalog.

## Release and signing process

1. A committer pushes a `v<version>` tag. The **Release daemon** workflow (`.github/workflows/release.yml`) runs on GitHub-hosted runners and rejects a tag that does not match the Cargo package version.
2. The workflow checks the release `LICENSE` and embedded-asset digests, builds the offset-generator payload with locked NuGet restore, and builds `vosu.exe` with the pinned Rust toolchain and `--locked` dependencies. Release jobs restore no build caches, so every release binary is compiled from scratch from the tagged source. Third-party actions are pinned to commit SHAs.
3. The unsigned `vosu.exe` is uploaded as a workflow artifact and submitted to the SignPath project `vosu` under the `release-signing` signing policy. The workflow submits signing requests only for `v*` tags.
4. An approver must review and manually approve the request in SignPath before anything is signed. Before approving, they confirm that it comes from the expected tag, commit, and workflow run. The `release-signing` policy in SignPath must require that approval; the workflow only names the policy and cannot enforce it.
5. The workflow packages the signed executable, smoke-tests the extracted archive, and verifies with `scripts/verify-windows-release.ps1` that the Authenticode signature is valid, issued to SignPath Foundation, and timestamped, and that the version resources name `vosu` and the tagged version.
6. The release is published only after the Linux, Windows, and Linux package-installation jobs all succeed.

## Privacy

See the [privacy policy](PRIVACY.md). It lists every network request vosu makes, what those requests send, and what the third-party services involved can see. The [security and installation disclosure](docs/security.md) lists what vosu installs, the privileges it requests, and what it changes on the system.

## Reporting

Report a signed file that you believe violates this policy by opening an issue at <https://github.com/18-lab-dev/vosu/issues>, or report it to SignPath Foundation at <support@signpath.io>.
