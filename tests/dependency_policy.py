#!/usr/bin/env python3
"""Reject GTK3/Tao dependencies, including inactive packages retained in Cargo.lock."""
from pathlib import Path
import tomllib

FORBIDDEN = {
    "gtk", "gtk-sys", "gdk", "gdk-sys", "atk", "atk-sys", "tao",
    "gtk-layer-shell", "gtk-layer-shell-sys", "libappindicator", "libappindicator-sys",
}


def main():
    lock = Path(__file__).resolve().parent.parent / "Cargo.lock"
    packages = tomllib.loads(lock.read_text(encoding="utf-8"))["package"]
    bad = []
    for package in packages:
        name, version = package["name"], package["version"]
        old_glib = name == "glib" and tuple(map(int, version.split(".")[:2])) < (0, 20)
        if name in FORBIDDEN or old_glib:
            bad.append(f"{name} {version}")
    if bad:
        raise SystemExit("GTK3 dependency policy failed: " + ", ".join(bad))
    print("Dependency policy passed: no GTK3, legacy GLib or Tao in Cargo.lock.")


if __name__ == "__main__":
    main()
