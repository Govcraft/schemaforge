#!/usr/bin/env python3
"""Check workspace versions, local dependency requirements, and release metadata."""

import argparse
import os
import re
import sys
import tomllib

from select_checks import changed_paths, local_dependencies_valid, snapshot, workspace_packages


def valid_semver(version):
    match = re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?", version)
    if match is None:
        return False
    prerelease, build = match.groups()[3:]
    for identifiers in (prerelease, build):
        if identifiers is not None and any(not part for part in identifiers.split(".")):
            return False
    return not prerelease or all(not (part.isdigit() and len(part) > 1 and part.startswith("0")) for part in prerelease.split("."))


def has_changelog_entry(changelog, version):
    heading = re.search(rf"^## \[{re.escape(version)}\](?:\s.*)?$", changelog, re.MULTILINE)
    if heading is None:
        return False
    section = changelog[heading.end():]
    following = re.search(r"^## ", section, re.MULTILINE)
    if following:
        section = section[:following.start()]
    return any(line.strip() and not line.lstrip().startswith("#") for line in section.splitlines())


def validate(files, before=None, release_tag=None):
    errors = []
    try:
        packages = workspace_packages(files)
        lock = tomllib.loads(files["Cargo.lock"])
        for name, (_, version, _) in packages.items():
            if not valid_semver(version):
                errors.append(f"Invalid package version: {name} {version}")
            entries = [p for p in lock.get("package", []) if p["name"] == name and "source" not in p]
            if len(entries) != 1 or entries[0]["version"] != version:
                errors.append(f"Cargo.lock must contain exactly one local {name} {version}")
        if not local_dependencies_valid(packages):
            errors.append("Local dependency package names, paths, or version requirements are inconsistent")
        cli = packages["schema-forge-cli"][1]
        changelog = files.get("CHANGELOG.md", "")
        if before is not None:
            old = workspace_packages(before)
            if old["schema-forge-cli"][1] != cli and not has_changelog_entry(changelog, cli):
                errors.append(f"CLI version {cli} requires a nonempty CHANGELOG.md section")
        if release_tag:
            if release_tag != f"v{cli}":
                errors.append(f"Release tag {release_tag} must match CLI version v{cli}")
            if not has_changelog_entry(changelog, cli):
                errors.append(f"Release {cli} requires a nonempty CHANGELOG.md section")
    except (KeyError, ValueError, TypeError) as error:
        errors.append(f"Invalid Cargo metadata: {error}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--release-tag", default=os.environ.get("RELEASE_TAG"))
    args = parser.parse_args()
    before = None
    if args.base:
        base, _ = changed_paths(args.base, args.head)
        before = snapshot(base)
    errors = validate(snapshot(args.head), before, args.release_tag)
    if errors:
        sys.exit("\n".join(errors))
    print("Workspace versions, local dependencies, lockfile, and release metadata are consistent.")


if __name__ == "__main__":
    main()
