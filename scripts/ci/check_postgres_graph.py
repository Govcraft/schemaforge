#!/usr/bin/env python3
"""Validate PostgreSQL backend isolation, extensions, and SQLx's TLS provider."""

import argparse
from pathlib import Path
import sys


REQUIRED_PACKAGES = {
    "schema-forge-cli", "schema-forge-acton", "schema-forge-postgres",
    "acton-service", "sqlx", "sqlx-core",
}
EXTENSION_PACKAGES = {"schema-forge-cli", "schema-forge-acton", "acton-service"}


def check_graph(text, extensions):
    """Check cargo tree's `{p}|{f}` output, including repeated dependency entries."""
    packages = {}
    for line in text.splitlines():
        # Cargo includes headings for build and dev edges in this tree format.
        if not line or line in ("[build-dependencies]", "[dev-dependencies]"):
            continue
        if "|" not in line:
            raise ValueError(f"Malformed dependency record: {line}")
        package, features = line.split("|", 1)
        name = package.split()[0]
        features = features.removesuffix(" (*)")
        packages.setdefault(name, set()).update(filter(None, features.split(",")))
    failures = []
    missing = REQUIRED_PACKAGES - packages.keys()
    if missing:
        failures.append("Missing PostgreSQL consumers: " + ", ".join(sorted(missing)))
    for name, features in packages.items():
        if "surrealdb" in name or "surrealdb" in features or "test-surrealdb" in features:
            failures.append(f"Embedded SurrealDB dependency: {name}")
        if name in EXTENSION_PACKAGES:
            if extensions:
                absent = {"oauth", "sse"} - features
                if absent:
                    failures.append(f"{name}: missing extensions {', '.join(sorted(absent))}")
            else:
                unexpected = features & {"oauth", "sse", "graphql"}
                if unexpected:
                    failures.append(f"{name}: extensions enabled {', '.join(sorted(unexpected))}")
    for name in ("schema-forge-cli", "schema-forge-acton"):
        if name in packages and "postgres" not in packages[name]:
            failures.append(f"{name}: PostgreSQL feature absent")
    # SQLx prefers Ring when both providers' features are unified. Check the
    # internal driver too, since another dependency can enable it directly.
    for name, required in (
        ("sqlx", "tls-rustls-aws-lc-rs"),
        ("sqlx-core", "_tls-rustls-aws-lc-rs"),
    ):
        features = packages.get(name, set())
        if required not in features:
            failures.append(f"{name}: AWS-LC TLS feature absent")
        competing = {
            feature for feature in features
            if "tls-rustls-ring" in feature
            or feature in {"tls-rustls", "runtime-tokio-rustls", "runtime-async-std-rustls"}
            or "tls-native-tls" in feature
        }
        if competing:
            failures.append(f"{name}: competing TLS provider features {', '.join(sorted(competing))}")
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("graph", type=Path)
    parser.add_argument("--extensions", required=True, choices=("enabled", "disabled"))
    args = parser.parse_args()
    try:
        failures = check_graph(args.graph.read_text(), args.extensions == "enabled")
    except (OSError, ValueError) as error:
        sys.exit(f"Invalid PostgreSQL dependency graph: {error}")
    if failures:
        sys.exit("PostgreSQL graph check failed: " + "; ".join(failures))
    print(f"PostgreSQL graph is isolated; extensions {args.extensions}.")


if __name__ == "__main__":
    main()
