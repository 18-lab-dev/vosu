# Run graphical launches without a console and inspect logs from files

- Status: Accepted (vosu 2.0)
- Date: 2026-11-24

## Context

The daemon's UI is its system tray and browser dashboard, not a persistent
terminal. The same executable must nevertheless remain a normal CLI: shells
must wait, receive exit codes, display output, and deliver Ctrl-C to daemon and
plugin actions. The updater requires one root executable.

## Decision

Keep the Windows console subsystem. Merge the Windows 11 24H2/Server 2025
`consoleAllocationPolicy=detached` setting into winresource's embedded manifest.
Graphical launches allocate no console; terminal launches inherit theirs. On
older Windows, daemon startup detaches only a console whose sole attached
process is vosu. CLI actions never take that fallback path.

Add `vosu logs [--follow] [-n N]`, reading the same channel-aware log directory
as the daemon. Tail and follow handle truncation and log rotation. Files remain
useful after a crash or when the daemon is unresponsive; their OS permissions
protect private log data without a new authenticated HTTP/WebSocket surface.

Add tray **Show logs** on Windows and Linux. Windows creates a new console
process and passes internal `logs --window`; that action dynamically loads
`AllocConsoleWithOptions` with `ALLOC_CONSOLE_MODE_NEW_WINDOW` on new Windows,
overriding the detached allocation policy. Older Windows uses the process
creation flag. Linux tries `xdg-terminal-exec`, `$TERMINAL`, then a short list of
common terminals, falling back to the log file's default handler with a warning.

## Consequences

There is no GUI-subsystem switch, launcher executable, new runtime dependency,
or network log endpoint. On older Windows the fallback may briefly flash a
console. Headless Linux remains supported without a terminal or tray. The
normal terminal invocation keeps its existing foreground lifecycle; closing a
separate log viewer does not stop the daemon.

## References

- [Application manifests](https://learn.microsoft.com/en-us/windows/win32/sbscs/application-manifests)
- [AllocConsoleWithOptions](https://learn.microsoft.com/en-us/windows/console/allocconsolewithoptions)
