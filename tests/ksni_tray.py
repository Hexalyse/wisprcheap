#!/usr/bin/env python3
"""Check GTK-free tray startup, dynamic menus, activation and shutdown on an isolated D-Bus."""
import argparse
import os
from pathlib import Path
import subprocess
from native_test_binary import build_test_binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true")
    args = parser.parse_args()
    binary = build_test_binary(Path(__file__).resolve().parent.parent, args.release)
    env = os.environ.copy()
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"):
        env.pop(name, None)
    subprocess.run(["dbus-run-session", "--", binary, "ksni_tray_backend", "--ignored",
                    "--nocapture", "--test-threads=1"], env=env, check=True, timeout=30)


if __name__ == "__main__":
    main()
