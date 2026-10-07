#!/usr/bin/env python3
"""Keep unused hosted-runner toolchains out of Rust cache identities."""

import os
from pathlib import Path
import subprocess
import tomllib


def unused_toolchains(installed, active):
    names = [line.split()[0] for line in installed.splitlines() if line.strip()]
    if active not in names:
        raise ValueError("Active Rust toolchain is not in the installed toolchain list")
    return [name for name in names if name != active]


def normalize(environment, run=subprocess.run):
    if environment.get("GITHUB_ACTIONS") != "true" or environment.get("RUNNER_ENVIRONMENT") != "github-hosted":
        print("Leaving toolchains unchanged outside disposable GitHub-hosted runners.")
        return
    channel = tomllib.loads(Path("rust-toolchain.toml").read_text())["toolchain"]["channel"]

    def output(*command):
        return run(list(command), check=True, capture_output=True, text=True).stdout.strip()

    version = output("rustc", "--version")
    if version.split()[1] != channel:
        raise ValueError(f"Expected pinned Rust {channel}, found {version}")
    active = output("rustup", "show", "active-toolchain").split()[0]
    for toolchain in unused_toolchains(output("rustup", "toolchain", "list", "--quiet"), active):
        run(["rustup", "toolchain", "uninstall", toolchain], check=True)
    print(f"Cache toolchain: {active} ({version})")


if __name__ == "__main__":
    normalize(os.environ)
