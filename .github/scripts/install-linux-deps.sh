#!/usr/bin/env bash
# Build/runtime baseline: Ubuntu 24.04 (GLib >= 2.74), GTK4, and GTK4 Layer Shell >= 1.0.
set -euo pipefail

as_root=()
if [ "$(id -u)" -ne 0 ]; then as_root=(sudo); fi
"${as_root[@]}" apt-get update
"${as_root[@]}" apt-get install -y --no-install-recommends \
  build-essential git pkg-config meson ninja-build libgtk-4-dev libasound2-dev \
  libx11-dev libxtst-dev libxi-dev libdbus-1-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev wayland-protocols sway grim xvfb xauth openbox xcompmgr xdotool dbus-x11

# Ubuntu 24.04 does not ship GTK4 Layer Shell. Use a verified upstream revision,
# rather than silently falling back to the unrelated GTK3 layer-shell package.
if [ "$(pkg-config --modversion gtk4-layer-shell-0 2>/dev/null || true)" != "1.3.0" ] || \
   [ ! -f /usr/local/share/licenses/gtk4-layer-shell/LICENSE ]; then
  layer_commit=1c963c51514581c41b9bdae08cdf69171265cdda # upstream v1.3.0
  build_root=$(mktemp -d)
  trap 'rm -rf "$build_root"' EXIT
  git clone --depth 1 --branch v1.3.0 https://github.com/wmww/gtk4-layer-shell.git "$build_root/source"
  test "$(git -C "$build_root/source" rev-parse HEAD)" = "$layer_commit"
  meson setup "$build_root/build" "$build_root/source" --prefix=/usr/local \
    -Dexamples=false -Ddocs=false -Dtests=false -Dintrospection=false -Dvapi=false
  meson compile -C "$build_root/build"
  "${as_root[@]}" meson install -C "$build_root/build"
  "${as_root[@]}" install -D -m644 "$build_root/source/LICENSE" /usr/local/share/licenses/gtk4-layer-shell/LICENSE
  "${as_root[@]}" ldconfig
fi
