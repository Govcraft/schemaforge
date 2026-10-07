#!/usr/bin/env python3
"""Install missing Ubuntu CI prerequisites without unbounded mirror retries."""

import argparse
import os
from pathlib import Path
import platform
import re
import subprocess


APT_LIMITS = '''Acquire::http::Timeout "15";
Acquire::https::Timeout "15";
Acquire::Retries "1";
Acquire::Languages "none";
'''


def ubuntu_mirror(architecture):
    if architecture == "amd64":
        return "https://archive.ubuntu.com/ubuntu/"
    if architecture == "arm64":
        return "https://ports.ubuntu.com/ubuntu-ports/"
    raise ValueError(f"Unsupported Ubuntu runner architecture: {architecture}")


def missing_packages(packages, run=subprocess.run):
    if not packages or any(not re.fullmatch(r"[a-z0-9][a-z0-9+.-]+", name) for name in packages):
        raise ValueError("Specify literal Debian package names")
    missing = []
    for name in dict.fromkeys(packages):
        result = run(["dpkg-query", "-W", "-f=${Status}", name],
                     capture_output=True, text=True, check=False)
        if result.returncode != 0 or result.stdout.strip() != "install ok installed":
            missing.append(name)
    return missing


def install(packages, environment, release, apt_root=Path("/etc/apt"), run=subprocess.run):
    # This changes APT configuration, including later Playwright installation.
    # Never apply it to a developer machine or persistent self-hosted runner.
    if not (environment.get("GITHUB_ACTIONS") == "true"
            and environment.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and release.get("ID") == "ubuntu"):
        raise ValueError("Prerequisite installation requires a disposable GitHub-hosted Ubuntu runner")
    missing = missing_packages(packages, run)
    architecture = run(["dpkg", "--print-architecture"], capture_output=True,
                       text=True, check=True).stdout.strip()
    mirror = ubuntu_mirror(architecture)

    def write(path, contents):
        run(["sudo", "-n", "tee", str(path)], input=contents, text=True,
            stdout=subprocess.DEVNULL, check=True)

    # GitHub's mirror list currently prefers Azure's archive. A stalled mirror
    # can cost minutes per index before falling back to the public archive.
    mirrors = apt_root / "apt-mirrors.txt"
    if mirrors.exists():
        write(mirrors, mirror + "\n")
    for path in (apt_root / "sources.list", apt_root / "sources.list.d/ubuntu.sources"):
        if path.exists():
            contents = path.read_text()
            updated = re.sub(r"https?://azure\.archive\.ubuntu\.com/ubuntu/?", mirror, contents)
            if updated != contents:
                write(path, updated)
    write(apt_root / "apt.conf.d/99schemaforge-ci", APT_LIMITS)
    if not missing:
        print("All system prerequisites are installed; skipping APT", flush=True)
        return
    print("Installing missing system prerequisites: " + ", ".join(missing), flush=True)
    run(["timeout", "--kill-after=10s", "120s", "sudo", "-n", "apt-get",
         "update", "--error-on=any"], check=True)
    run(["timeout", "--kill-after=10s", "180s", "sudo", "-n", "apt-get",
         "install", "-y", "--no-install-recommends", *missing], check=True)
    if missing_packages(packages, run):
        raise RuntimeError("APT completed without installing every requested prerequisite")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("packages", nargs="+")
    args = parser.parse_args()
    install(args.packages, os.environ, platform.freedesktop_os_release())


if __name__ == "__main__":
    main()
