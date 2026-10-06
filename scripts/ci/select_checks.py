#!/usr/bin/env python3
"""Select affected CI consumers. Unknown or unparseable changes fail broad."""

import argparse
import copy
import fnmatch
import json
import os
from pathlib import PurePosixPath
import re
import subprocess
import tomllib

SUITES = ("tooling", "runtime", "cli", "postgres", "surrealdb", "mssql", "cel", "site")
DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def git(*args, repo="."):
    return subprocess.check_output(["git", "-C", str(repo), *args])


def changed_paths(base, head, repo="."):
    ancestor = git("merge-base", base, head, repo=repo).decode().strip()
    paths = git("diff", "--no-renames", "--name-only", "-z", ancestor, head, repo=repo)
    return ancestor, [p.decode() for p in paths.split(b"\0") if p]


def snapshot(revision, repo="."):
    paths = git("ls-tree", "-r", "--name-only", "-z", revision, repo=repo)
    relevant = [p.decode() for p in paths.split(b"\0") if p and
                (p.endswith(b"Cargo.toml") or p in (b"Cargo.lock", b"CHANGELOG.md"))]
    return {p: git("show", f"{revision}:{p}", repo=repo).decode() for p in relevant}


def dependency_tables(manifest):
    for table in DEPENDENCY_TABLES:
        yield manifest.get(table, {})
    for target in manifest.get("target", {}).values():
        for table in DEPENDENCY_TABLES:
            yield target.get(table, {})


def workspace_packages(files):
    root = tomllib.loads(files["Cargo.toml"])
    members = root["workspace"]["members"]
    exclusions = root["workspace"].get("exclude", [])
    manifest_paths = {str(PurePosixPath(path).parent) for path in files if path.endswith("/Cargo.toml")}
    for member in members:
        if not any(fnmatch.fnmatchcase(directory, member) for directory in manifest_paths):
            raise ValueError(f"Workspace member has no manifest: {member}")
    packages = {}
    for path, text in files.items():
        directory = str(PurePosixPath(path).parent)
        if path == "Cargo.toml" or not path.endswith("/Cargo.toml"):
            continue
        if not any(fnmatch.fnmatchcase(directory, member) for member in members):
            continue
        if any(fnmatch.fnmatchcase(directory, member) for member in exclusions):
            continue
        manifest = tomllib.loads(text)
        package = manifest["package"]
        version = package["version"]
        if isinstance(version, dict):
            version = root["workspace"]["package"]["version"]
        if package["name"] in packages:
            raise ValueError(f"Duplicate workspace package: {package['name']}")
        packages[package["name"]] = (path, version, manifest)
    return packages


def version_tuple(version):
    # Prerelease and build-metadata constraints need Cargo's full resolver.
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError(f"Unsupported version: {version}")
    return tuple(int(part) for part in version.split("."))


def satisfies(version, requirement):
    """Conservative support for Cargo's common stable version requirements."""
    actual = version_tuple(version)
    for constraint in requirement.split(","):
        constraint = constraint.strip()
        match = re.fullmatch(r"(\^|~|=|>=|<=|>|<)?\s*(\d+)(?:\.(\d+|\*))?(?:\.(\d+|\*))?", constraint)
        if constraint == "*":
            continue
        if not match:
            return False
        operator, *parts = match.groups()
        specified = [int(p) for p in parts if p is not None and p != "*"]
        expected = tuple(specified + [0] * (3 - len(specified)))
        if "*" in parts:
            if operator or actual[:len(specified)] != tuple(specified):
                return False
        elif operator in ("=", ">=", "<=", ">", "<"):
            comparisons = {"=": actual == expected, ">=": actual >= expected,
                           "<=": actual <= expected, ">": actual > expected, "<": actual < expected}
            if not comparisons[operator]:
                return False
        else:
            if operator == "~":
                index = 0 if len(specified) == 1 else 1
            else:
                index = next((i for i, n in enumerate(specified) if n), len(specified) - 1)
            upper = list(expected)
            upper[index] += 1
            upper[index + 1:] = [0] * (2 - index)
            if not expected <= actual < tuple(upper):
                return False
    return True


def local_dependencies_valid(packages):
    directories = {str(PurePosixPath(path).parent): (name, version)
                   for name, (path, version, _) in packages.items()}
    for _, (path, _, manifest) in packages.items():
        for table in dependency_tables(manifest):
            for alias, dependency in table.items():
                if not isinstance(dependency, dict) or "path" not in dependency:
                    continue
                directory = os.path.normpath(str(PurePosixPath(path).parent / dependency["path"]))
                if directory not in directories:
                    return False
                name, version = directories[directory]
                if dependency.get("package", alias) != name:
                    return False
                if "version" in dependency and not satisfies(version, dependency["version"]):
                    return False
    return True


def normalized_manifest(path, manifest, packages):
    normalized = copy.deepcopy(manifest)
    normalized["package"].pop("version")
    directories = {str(PurePosixPath(p).parent) for p, _, _ in packages.values()}
    for table in dependency_tables(normalized):
        for dependency in table.values():
            if isinstance(dependency, dict) and "path" in dependency:
                directory = os.path.normpath(str(PurePosixPath(path).parent / dependency["path"]))
                if directory in directories:
                    dependency.pop("version", None)
    return normalized


def normalized_lock(files, packages, allowed_versions):
    lock = tomllib.loads(files["Cargo.lock"])
    registry_names = {package["name"] for package in lock.get("package", []) if "source" in package}
    for package in lock.get("package", []):
        name = package["name"]
        if name in packages and "source" not in package:
            if package["version"] != packages[name][1]:
                raise ValueError(f"Stale lock entry: {name}")
            package.pop("version")
        for i, dependency in enumerate(package.get("dependencies", [])):
            words = dependency.split()
            if (len(words) == 2 and words[0] in allowed_versions and
                    words[0] not in registry_names and words[1] in allowed_versions[words[0]]):
                package["dependencies"][i] = words[0]
        if "dependencies" in package:
            package["dependencies"].sort()
    lock["package"].sort(key=lambda p: (p["name"], p.get("version", ""), p.get("source", "")))
    return lock


def version_only(paths, before, after):
    """Allow only version edits of existing workspace packages and references."""
    try:
        old = workspace_packages(before)
        new = workspace_packages(after)
        if old.keys() != new.keys() or not local_dependencies_valid(new):
            return False
        for packages in (old, new):
            for _, version, _ in packages.values():
                version_tuple(version)
        allowed = {name: {old[name][1], new[name][1]} for name in old}
        manifests = {entry[0] for entry in old.values()}
        for path in paths:
            if path == "Cargo.lock":
                if normalized_lock(before, old, allowed) != normalized_lock(after, new, allowed):
                    return False
            elif path in manifests:
                name = next(name for name, entry in old.items() if entry[0] == path)
                if normalized_manifest(path, old[name][2], old) != normalized_manifest(path, new[name][2], new):
                    return False
            else:
                return False
        return True
    except (KeyError, ValueError, TypeError):
        return False


def is_documentation(path):
    if path in ("README.md", "SECURITY.md", "CONTRIBUTING.md", "CODE_OF_CONDUCT.md", "CHANGELOG.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "AGENTS.md"):
        return True
    parts = PurePosixPath(path).parts
    if len(parts) == 3 and parts[0] == "crates" and parts[-1] == "README.md":
        return True
    if path == "crates/schema-forge-cli/tests/site_e2e/README.md":
        return True
    extension = PurePosixPath(path).suffix.lower()
    if path.startswith(("docs/", "skills/")) and extension == ".md":
        return True
    return path.startswith("docs/assets/") and extension in (".png", ".jpg", ".jpeg", ".webp", ".svg", ".gif")


def classify(paths, before=None, after=None, full=False):
    checks = {name: False for name in SUITES}
    if full:
        return dict.fromkeys(SUITES, True)
    source_paths = [path for path in paths if not is_documentation(path)]
    if not source_paths:
        return checks
    if before is not None and after is not None and version_only(source_paths, before, after):
        return checks
    for path in source_paths:
        suites = None
        if path.endswith("Cargo.toml") or path == "Cargo.lock":
            suites = SUITES
        elif path.startswith("crates/schema-forge-codegen/") or path in (
                "crates/schema-forge-cli/src/commands/site.rs",
                "crates/schema-forge-cli/src/commands/codegen.rs") or path.startswith((
                "crates/schema-forge-cli/templates/", "crates/schema-forge-cli/src/commands/site/",
                "crates/schema-forge-cli/src/commands/codegen/", "crates/schema-forge-cli/tests/site_e2e/")):
            suites = ("tooling", "site")
        elif path.startswith("crates/schema-forge-cli/"):
            relative = path.removeprefix("crates/schema-forge-cli/")
            broad = ("src/main.rs", "src/cli.rs", "src/config.rs", "src/config_tooling.rs",
                     "src/commands/mod.rs", "src/commands/backend.rs", "build.rs")
            server_commands = ("serve", "apply", "migrate", "entity", "bootstrap_admin", "login", "token", "export", "inspect", "schema_update")
            if relative in broad or any(relative.startswith(f"src/commands/{name}") for name in server_commands):
                suites = SUITES
            else:
                suites = ("tooling", "cli")
        elif path.startswith("crates/schema-forge-acton/"):
            # Security and tenant changes exercise every storage implementation.
            if any(word in path.lower() for word in ("auth", "tenan", "cedar", "principal", "membership", "invite", "oauth", "system", "state", "shared", "extension", "creator_", "policy", "config")):
                suites = SUITES
            else:
                suites = ("runtime", "postgres", "surrealdb", "site")
        elif path.startswith("crates/schema-forge-postgres/"):
            suites = ("postgres", "runtime")
        elif path.startswith("crates/schema-forge-surrealdb/"):
            suites = ("surrealdb", "site")
        elif path.startswith("crates/schema-forge-mssql/"):
            suites = ("mssql",)
        elif path.startswith("crates/schema-forge-cel/"):
            suites = ("cel", "tooling", "runtime", "postgres", "surrealdb", "site")
        # Shared crates, infrastructure, and unknown paths conservatively run all.
        for suite in suites or SUITES:
            checks[suite] = True
    return checks


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--full", action="store_true")
    args = parser.parse_args()
    if args.full:
        checks = classify([], full=True)
    elif args.base:
        base, paths = changed_paths(args.base, args.head)
        checks = classify(paths, snapshot(base), snapshot(args.head))
    else:
        parser.error("--base is required unless --full is supplied")
    print(json.dumps(checks, sort_keys=True))
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as file:
            for name, selected in checks.items():
                file.write(f"{name}={str(selected).lower()}\n")


if __name__ == "__main__":
    main()
