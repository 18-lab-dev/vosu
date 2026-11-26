# Allow approved process plugins to create Linux user namespaces

- Status: Accepted
- Date: 2026-11-26
- Amends: [ADR 0019](0019-plugin-runtime-and-trust.md), A5.1; see [Plugin ABI §7.3](../v2/plugin-abi.md#73-linux-privilege-drop-and-confinement-before-loading-plugin-code)

## Context

The vosu-tools replay renderer renders untrusted overlays, skins and replays. It must keep Chromium's sandbox enabled. Chromium starts on its own on Linux, but not under the process-plugin filter: `no_new_privs` prevents using its setuid helper, and our filter refuses the user namespaces it uses instead. The Windows job object does not impose this restriction.

## Decision

A process plugin can request `user_namespaces = true` in `[runtime.linux]` with `mode = "process"` in `[runtime]`. The table form is necessary because TOML cannot also give `runtime` a scalar value. Existing scalar runtime declarations remain valid; in-process plugins cannot use the Linux table.

Show the request in the catalog install plan and in native approval, with fixed wording: “On Linux, this plugin can create user namespaces, which Chromium-style sandboxes need.” On Linux, opting in requires explicit approval even for trusted and official sources. Approval stays bound to the exact artifact: native approval uses the manifest and library SHA-256 digests; install confirmation pins the package hash and its mirrored declaration. Changing the declaration requires new approval.

Only an approved Linux process host selects the opt-in filter. It permits `unshare`, `clone` and `clone3` to create user namespaces. It still refuses all `setns` with `EPERM` and kills on a foreign audit architecture or an x32 syscall number. `no_new_privs`, the capability drop, parent-death handling, and fail-closed startup remain mandatory. Readback records the selected filter and the opt-in probe checks that `setns` is denied without changing the host namespace. The default filter and Windows job object do not change.

## Alternatives considered

- **A core-launched sandboxed browser helper.** This preserves the original plugin filter but adds a launch capability and a browser-specific IPC and lifetime contract to core. The renderer already owns its browser process; the declared opt-in avoids transferring that responsibility to core.
- **Windows-only replay rendering.** This avoids changing Linux policy but removes a feature on a supported OS when Linux already has a working browser sandbox.
- **Run Chromium without its sandbox.** Rejected: replay rendering processes untrusted content.

## Consequences

An opted-in plugin and its descendants can gain capabilities scoped to a new user namespace, which broadens their access to kernel interfaces. They do not regain setup-granted daemon capabilities in the original namespace. An OS policy can still prohibit user namespaces independently of vosu. Neither filter is a general sandbox for the native plugin: it still has the user's filesystem and network access. No setup privilege, ABI opcode, Windows launch behavior or default Linux policy changes.
