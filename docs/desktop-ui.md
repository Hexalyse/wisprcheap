# Desktop companion

Build with `cargo build --release`, then open `wisprcheap ui`. The tray's **Open WisprCheap**
item (or left-click on Windows) and newly generated desktop shortcuts use the same launcher.
Closing the window exits `wisprcheap-ui`; the background dictation app and native overlay continue.
Quitting dictation from the tray or CLI also requests a close of the companion for that instance.
Unsaved edits retain their confirmation; a save already in progress finishes before the window closes.

The companion uses Iced 0.14 with Tiny Skia, X11/Wayland window support, and no webview or GPU renderer.
It is a separate Cargo package. `cargo tree -p wisprcheap -e normal` contains no Iced/Winit dependencies.
The optional UI polls runtime status every two seconds while open. History, config and log reads run
on blocking workers, outside the dictation actor. The history view returns at most 200 matching entries;
statistics cover all entries in the selected period. Refresh reloads activity and logs.

Settings is one continuous form with section headings and a persistent menu beside it.
Menu items scroll to their section; scrolling the form updates the highlighted menu item.
Anchors use measured layout heights so wrapped text, microphone lists and resizing stay accurate.
Unsaved edits are retained while navigating between sections.
Settings are patches to the existing YAML rather than a serialized replacement. Unedited comments,
unknown settings and `${VAR}` references are retained. Saves validate against the normal config loader
and use an atomic file replacement. A revision of the config and `.env` files prevents a stale UI from
overwriting changes from an editor or sync. Dictionary shortcuts, sync and companion saves share an
in-process edit lock. Missing provider keys are allowed during setup; the app still requires them to start.
Keys entered in the UI are stored in the local YAML file; existing `.env` keys remain there.

Both processes have separate single-instance sockets. The companion focuses an existing window when
opened again. It talks to the app over the same per-user named pipe/Unix socket as the CLI, using
`ui-v1` plus a JSON request and JSON reply. A connected but unresponsive/incompatible app produces an
error; it never falls back to an offline write. With no app running, the companion can configure settings
and read local history directly. If the app is running, it supplies its own active config path.

Statistics show recorded words, audio duration, estimated STT/LLM costs, daily activity, monthly totals,
models, failures and raw-text fallbacks. Latency is STT + LLM processing time; it excludes queue and paste.
Unknown prices are flagged as partial totals. Price overrides apply to future recordings, as in the CLI.

Overview and History share side-by-side **Current device / All devices** filter buttons, matching
Android's filled selected chip and outlined alternative. Current device is the default
and includes untagged history recorded before pairing. All devices reads encrypted account history on
demand, decrypts it in memory and merges local unsynced recordings using the sync engine's record IDs.
This works with upload-only history sync and never writes remote history or advances the sync cursor.
Network/key errors are shown instead of presenting local totals as complete account statistics.
Without a connected sync account, All devices explicitly labels its totals as locally available history.
Costs and sync entries have been removed from the tray; their controls remain in Overview.

Current scope: sync status and **Sync now** are available, while account/passphrase setup remains in
`wisprcheap sync`. Retry uses the app's last failed recording from the current session. The history
view displays saved audio paths but does not replay or retry arbitrary historical recordings.

For render checks, use an isolated config and `WISPRCHEAP_INSTANCE`, then launch the companion with
`--screenshots <directory>`. It renders every page and settings section, exports PNGs using Iced's own
window screenshot API and exits, including both device scopes in Overview and History.
Add `--screenshots-small` to check the minimum 850 × 600 window size.
Use synthetic history and test keys for this check.

## Implementation validation

Checked on Windows/MSVC and Ubuntu 24.04 in the isolated `WisprCheapBuild` WSL distribution:

- Desktop workspace builds on both platforms; Windows release binaries are in `target/release`.
- Unit tests, sync integration, companion IPC integration and Clippy with warnings denied pass on both platforms.
- Seven pages render and export screenshots on Windows, Linux/X11 and headless Weston/Wayland.
- Real Windows daemon/companion smoke test verifies status, pause, validated saves, stale-edit rejection,
  malformed requests, translation selection, reopening the same window and process exit on window close.
  Dictation remains responsive after the companion exits. Fixtures use test keys and disable keyboard injection.
- The optimized companion used about 23 MiB of working set with empty history and no measurable CPU over
  a four-second idle sample. This is a local smoke measurement, not a benchmark with a large history.

WSLg's own Wayland compositor disconnected during the software-renderer smoke test in this environment.
X11 and a separate headless Wayland compositor passed. To run the GUI under WSLg here, use
`env -u WAYLAND_DISPLAY /root/wisprcheap-target/debug/wisprcheap-ui` (with your config arguments).
Physical Linux desktop behavior has not been tested. Live provider requests were not made during these checks.
