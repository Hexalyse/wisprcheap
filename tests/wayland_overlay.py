#!/usr/bin/env python3
"""Run the GTK overlay integration test in an isolated software-rendered Sway.

Requires Sway, grim, Xvfb, xauth, and the normal Linux build dependencies. No input-device
permissions, microphone, provider keys, or running desktop are needed.
"""

import argparse
import json
import os
from pathlib import Path
import pwd
import shutil
import signal
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    output = repo / "target" / "wayland-overlay"
    output.mkdir(parents=True, exist_ok=True)
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("cargo not found")
    command = [cargo, "test", "--locked", "-p", "wisprcheap", "--lib", "--no-run", "--message-format=json-render-diagnostics"]
    if args.release:
        command.append("--release")
    build = subprocess.run(command, cwd=repo, stdout=subprocess.PIPE, text=True, check=True)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
    binary = next(item["executable"] for item in artifacts
                  if item["reason"] == "compiler-artifact" and item.get("executable")
                  and item["target"]["kind"] == ["lib"])

    # Sway refuses to run as root. This also lets WSL/root callers test safely.
    account = pwd.getpwnam("nobody") if os.getuid() == 0 else pwd.getpwuid(os.getuid())
    runtime = Path(tempfile.mkdtemp(prefix="wisprcheap-wayland-"))
    test_binary = runtime / "overlay-tests"
    shutil.copy2(binary, test_binary)
    config = runtime / "sway.conf"
    config.write_text("xwayland disable\noutput * mode 1280x800\nseat seat0 fallback true\n", encoding="utf-8")
    if os.getuid() == 0:
        for path in (runtime, test_binary, config, output):
            os.chown(path, account.pw_uid, account.pw_gid)
    env = os.environ.copy()
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "SWAYSOCK", "DBUS_SESSION_BUS_ADDRESS"):
        env.pop(name, None)
    # Xvfb supplies virtual keyboard/pointer devices to Sway. The GTK client still uses native Wayland.
    env.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS="x11", WLR_RENDERER="pixman",
               WLR_LIBINPUT_NO_DEVICES="1", GDK_BACKEND="wayland")
    prefix = ["runuser", "-u", account.pw_name, "--"] if os.getuid() == 0 else []
    compositor = None
    try:
        with (output / "sway.log").open("w") as log:
            compositor = subprocess.Popen(prefix + ["xvfb-run", "-a", "-s", "-screen 0 1280x800x24",
                                                   "sway", "-c", str(config)], env=env,
                                          stdout=log, stderr=log, start_new_session=True)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                displays = [path for path in runtime.glob("wayland-*") if path.is_socket()]
                controls = list(runtime.glob("sway-ipc.*.sock"))
                if displays and controls:
                    break
                if compositor.poll() is not None:
                    raise RuntimeError((output / "sway.log").read_text())
                time.sleep(0.1)
            else:
                raise RuntimeError("Sway did not create its sockets")
            env.update(WAYLAND_DISPLAY=displays[0].name, SWAYSOCK=str(controls[0]),
                       WISPRCHEAP_OVERLAY_TEST_DIR=str(output), WAYLAND_DEBUG="client")
            env.pop("WISPRCHEAP_OVERLAY_EXPECT_UNSUPPORTED", None)
            with (output / "protocol.log").open("w") as protocol:
                subprocess.run(prefix + [str(test_binary), "wayland_overlay_backend", "--ignored",
                                         "--nocapture", "--test-threads=1"], env=env,
                               stderr=protocol, check=True, timeout=30)
            print(f"Wayland overlay integration passed; screenshots and logs: {output}")
    except Exception:
        protocol = output / "protocol.log"
        if protocol.exists():
            print("\n".join(protocol.read_text().splitlines()[-40:]))
        raise
    finally:
        if compositor is not None and compositor.poll() is None:
            os.killpg(compositor.pid, signal.SIGTERM)
            try:
                compositor.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(compositor.pid, signal.SIGKILL)
                compositor.wait()
        shutil.rmtree(runtime)


if __name__ == "__main__":
    main()
