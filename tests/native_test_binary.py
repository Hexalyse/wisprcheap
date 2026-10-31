"""Locate Cargo's native library-test executable for isolated desktop integration checks."""
import json
import shutil
import subprocess


def build_test_binary(repo, release=False):
    command = [shutil.which("cargo") or "cargo", "test", "--locked", "-p", "wisprcheap",
               "--lib", "--no-run", "--message-format=json-render-diagnostics"]
    if release:
        command.append("--release")
    build = subprocess.run(command, cwd=repo, stdout=subprocess.PIPE, text=True, check=True)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
    return next(item["executable"] for item in artifacts
                if item["reason"] == "compiler-artifact" and item.get("executable")
                and item["target"]["kind"] == ["lib"])
