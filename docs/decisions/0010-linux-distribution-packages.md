# Ship Linux distribution packages alongside portable archives

- Status: Accepted
- Date: 2026-10-09

## Context

Portable ZIPs support self-updates but do not integrate with Linux package
managers or desktop launchers. A release built against a newer glibc also cannot
run on otherwise supported older distributions. Installation and runtime
privilege must remain separate: only capability setup runs as root.

## Decision

1. Publish Debian/Ubuntu `.deb`, Fedora `.rpm`, and Arch `.pkg.tar.zst` assets
   alongside the unchanged Linux and Windows ZIPs. ZIP roots still contain
   exactly the executable and `LICENSE`, preserving the updater contract.
   Build Linux on Ubuntu 22.04 and assert required glibc symbols are at most
   2.35. Pin nfpm's release archive and SHA-256 to upstream release checksums.
2. Package install and upgrade hooks run `/usr/bin/vosu --setup` as root.
   Capability failure never aborts a transaction; print a one-line hint to run
   `/usr/bin/vosu --setup` as the desktop user, which requests polkit
   authentication itself. Do not run the telemetry daemon as root.
3. One executable serves portable and packaged installs. On Linux and Windows,
   a program directory not writable by the current user means externally
   managed installation: skip startup update checks, reject explicit update
   requests using existing updater-error conventions, and disable Automatic
   updates with package-manager guidance in the dashboard.
4. Commit an AUR `vosu-bin` template and render release-specific versions and
   SHA-256 values into a release tarball with `.SRCINFO` and install hooks.
   The template downloads the unchanged Linux ZIP plus desktop/icon resources
   from the matching source tag. A maintainer publishes these files to AUR;
   GitHub release publication does not impersonate that separate operation.
   **Superseded 2026-10-12:** the AUR maintainer publishes only `vosu-git`,
   built from `main` with `packaging/aur/PKGBUILD`; releases no longer carry an
   AUR bundle.
5. Install `/usr/bin/vosu`, a desktop launcher (`Exec=vosu`, `Icon=vosu`,
   `Terminal=false`), scalable and 256px icons, and license text in packages.
   The runtime tray is always present when a tray host exists and offers Open
   dashboard and Quit; it uses the installed icon name with embedded fallback.
   Linux Start at login is an optional dashboard setting using a systemd user
   service. Packages neither ship nor enable the service. A second launch that
   finds the configured listener answering opens that instance's dashboard and
   exits successfully rather than attempting to bind again.

## Consequences

Distribution dependencies include glibc, libelf, zlib, the GCC unwinder, and
setcap. Debian's libelf alternative accommodates the t64 package rename.
Clean distro installation smoke gates protect dependency and hook behavior;
portable archive checks remain independent. System-wide installs update through
their package manager; writable portable installs retain verified self-update.
Desktop integration does not imply a display or tray host must be available.

## References

- [Install instructions](../../README.md#linux-packages)
- [Release runbook](../operations/runbook.md#linux-distribution-releases)
- [Transactional self-update](0006-transactional-self-update.md)
