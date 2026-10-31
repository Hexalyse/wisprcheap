#!/usr/bin/env python3
"""Exercise the GTK4/X11 overlay in an isolated Xvfb + Openbox + compositor session."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import time
from native_test_binary import build_test_binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--inside", metavar="TEST_BINARY")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    output = repo / "target" / "x11-overlay"
    output.mkdir(parents=True, exist_ok=True)
    if not args.inside:
        binary = build_test_binary(repo, args.release)
        env = os.environ.copy()
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "GDK_SCALE", "GDK_DPI_SCALE"):
            env.pop(name, None)
        subprocess.run(["dbus-run-session", "--", "xvfb-run", "-a", "-s", "-screen 0 1280x800x24",
                        sys.executable, str(Path(__file__).resolve()), "--inside", binary], env=env, check=True)
        return
    with (output / "desktop.log").open("w") as log:
        manager = subprocess.Popen(["openbox", "--sm-disable"], stdout=log, stderr=log)
        compositor = subprocess.Popen(["xcompmgr", "-a"], stdout=log, stderr=log)
        try:
            time.sleep(1)
            if manager.poll() is not None or compositor.poll() is not None:
                raise RuntimeError("isolated X11 desktop failed to start; inspect " + str(output))
            env = dict(os.environ, GDK_BACKEND="x11", GSK_RENDERER="cairo")
            for scale in (1, 2):
                env["GDK_SCALE"] = str(scale)
                for case in ("x11_overlay_backend", "gtk4_log_window_backend"):
                    subprocess.run([args.inside, case, "--ignored", "--nocapture",
                                    "--test-threads=1"], env=env, check=True, timeout=30)
            print("X11 overlay and log-window integration passed at scales 1 and 2.")
        finally:
            for process in (compositor, manager):
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    main()
