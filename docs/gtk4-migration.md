# GTK4 desktop migration

Implemented on `feat/gtk4-desktop`. The preceding security hardening is preserved as a separate
commit. This migration changes desktop integration; settings/history formats and the sync protocol
remain compatible. The Android repository is unchanged by this migration.

## Implementation

- Linux uses GTK4 0.11 / GLib 0.22 for the native log window, startup dialogs and recording overlay.
  The settings/history companion continues using Iced 0.14 with Tiny Skia.
- The tray uses tray-icon 0.26's KSNI backend with default GTK/AppIndicator features disabled.
  Menus are published over D-Bus, including live pause/copy/translation updates. Tray registration
  survives a desktop tray host appearing after the application starts.
- Tao is removed completely, including from Windows: merely making Tao Windows-only still
  retains GTK3 in Cargo.lock. Windows now uses Winit 0.30 to pump the existing Win32 tray,
  log window and overlay messages. Linux awaits the UI channel on GLib's main context.
  This dispatch has no periodic polling timer while idle.
- The overlay reuses the existing software scene/animation renderer. On Wayland, GTK4 Layer Shell
  handles bottom anchoring, nonexclusive placement and no keyboard focus. On X11, a small x11rb
  adapter handles monitor-relative placement and native window hints. The overlay is
  override-redirect, non-focusable, topmost and click-through; work areas account for panels.
- Log windows retain buffered lines, a read-only text view, the 3,000-line cap, close-to-hide,
  Escape-to-hide and reopening. Startup dialogs await GTK4 responses through the main context.

GTK3, the old gtk-layer-shell bindings, AppIndicator, Tao and GLib 0.18 are absent from the entire
lockfile. The vendored GLib backport and workspace patch were removed. The original security
report remains a historical record; its backport-specific workaround has been superseded.

## Build and installation consequences

- Rust 1.92+ is now the declared minimum for both desktop packages.
- Linux release binaries target Ubuntu 24.04 / glibc 2.39+. The previous Ubuntu 22.04 binary
  baseline is no longer supported by these release archives. Older systems can build from
  source only if they provide compatible GTK4 and GLib development libraries.
- System GTK4, ALSA, X11/XTest and xkbcommon libraries replace the GTK3/AppIndicator requirements.
- GTK4 Layer Shell is a different library from GTK3 Layer Shell. The CI/development installer
  builds upstream v1.3.0 at verified commit `1c963c51514581c41b9bdae08cdf69171265cdda` on Ubuntu 24.04.
- Linux archives include `lib/libgtk4-layer-shell.so.0` and its MIT license. Both executables
  have `$ORIGIN/lib` in RUNPATH. Keep the `lib/` directory beside the executables when moving them;
  system installations can alternatively provide the library through the normal loader path.
- GNOME still requires a StatusNotifierItem extension for the tray. Compositors without Layer
  Shell, including GNOME on Wayland, still omit the overlay while dictation continues. Existing
  Wayland input-device permissions are unchanged.

Use `bash .github/scripts/install-linux-deps.sh` for the Ubuntu 24.04 build/test dependencies.
The README includes the runtime-only package command for release users.

## Validation

The migration was compiled and tested on native Windows and Ubuntu 24.04 in WSL2. Linux GUI
checks run on isolated displays/session buses, independently of the user's desktop:

```sh
cargo test --locked --workspace
cargo test --locked --release --workspace
python3 tests/ksni_tray.py --release
python3 tests/x11_overlay.py --release
python3 tests/wayland_overlay.py --release
python3 tests/dependency_policy.py
cargo audit
```

- Windows workspace: 121 passed, one preview test ignored. Includes queued Winit UI events and exit.
- Linux optimized workspace: 127 passed, six environment-specific/preview tests ignored by the normal suite.
- The separate native checks execute the tray test, X11 overlay and log window at both 1x and 2x,
  and the Wayland overlay with a live output scale change. They verify actual D-Bus menu activation,
  delayed host registration, tray shutdown, pointer pass-through, focus retention, placement,
  visibility, buffer sizing and log-window close/Escape/reopen behavior.
- The GTK4 Wayland recording screenshot was inspected: the pill and transparency are preserved.
- The production Linux binaries were built in release mode. A temporary archive layout verified
  both executable-relative RUNPATHs, resolved all shared libraries and started the main executable
  using the bundled GTK4 Layer Shell library.
- Cargo dependency policy rejects GTK3/Tao and obsolete GLib even in inactive lockfile entries.
- Cargo Audit reports zero vulnerability-category entries. Existing optional `lru` unsoundness and
  unrelated unmaintained-dependency warnings remain; this migration does not dismiss them.

Desktop CI runs on Windows and Ubuntu 24.04 pull requests. Release CI runs the native checks with
optimizations and bundles GTK4 Layer Shell. The server Docker build no longer needs vendored GLib.

Physical multi-monitor desktops, GNOME extensions and real provider/microphone calls have not been
exercised by these isolated tests. No release or deployment was published as part of the migration.
