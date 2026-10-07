#!/usr/bin/env python3
"""Populate main's missing dependency graphs without executing validation."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib


GRAPHS = {
    "tooling": {"key": "tooling-portable", "packages": ["core", "cel", "dsl", "config", "codegen", "signing", "cli"], "portable": True},
    "runtime": {"key": "runtime-portable", "packages": ["acton", "backend"], "portable": True},
    "cli": {"key": "cli-portable", "packages": ["cli"], "portable": True, "features": "server,oauth,sse"},
    "postgres": {"key": "postgres-extensions", "packages": ["postgres", "acton", "cli"], "portable": True,
                 "features": "schema-forge-cli/postgres,schema-forge-cli/oauth,schema-forge-cli/sse", "feature_env": True},
    "postgres-disabled": {"key": "postgres-no-extensions", "packages": ["cli", "acton"], "portable": True,
                          "features": "schema-forge-cli/postgres", "feature_env": True},
    "surrealdb": {"key": "surrealdb-extensions-runner", "packages": ["cli", "acton", "surrealdb"],
                  "features": "schema-forge-cli/oauth,schema-forge-cli/sse,schema-forge-cli/test-surrealdb,schema-forge-acton/test-surrealdb,schema-forge-acton/graphql",
                  "auxiliary": [{"packages": ["test-runner"]}]},
    "site": {"key": "site-v2-candidate", "packages": ["cli"], "bin": "schemaforge"},
    "site-tooling": {"key": "site-v2-tooling", "packages": ["cli"], "portable": True, "bin": "schemaforge"},
    "mssql": {"key": "sql-server", "packages": ["mssql"]},
    "windows": {"key": "windows-mssql", "packages": ["cli"], "portable": True, "features": "mssql", "bin": "schemaforge"},
}

LABELS = {
    "tooling": "Portable tooling", "runtime": "Portable runtime", "cli": "Server CLI",
    "postgres": "PostgreSQL and extensions", "postgres-disabled": "PostgreSQL without extensions",
    "surrealdb": "SurrealDB and GraphQL", "site": "Candidate site server",
    "site-tooling": "Portable site generator", "mssql": "SQL Server", "windows": "Windows MSSQL",
}


def matrix():
    return {"include": [{"graph": name, "label": LABELS[name], "cache_key": graph["key"],
                         "runner": "windows-2025" if name == "windows" else "ubuntu-24.04",
                         "cargo_features": graph.get("features", "") if graph.get("feature_env") else ""}
                        for name, graph in GRAPHS.items()]}


def build_arguments(graph):
    args = ["--locked"]
    for package in graph["packages"]:
        args.extend(["-p", f"schema-forge-{package}"])
    if graph.get("portable"):
        args.append("--no-default-features")
    if graph.get("features"):
        args.extend(["--features", graph["features"]])
    args.extend(["--bin", graph["bin"]] if graph.get("bin") else ["--all-targets"])
    return args


def commands(name, recipe):
    graph = GRAPHS[name]
    args = build_arguments(graph)
    # cargo-chef has no prebuilt Windows asset. The Windows suite only checks
    # the binary, so populate its dependency metadata with that same command.
    if name == "windows":
        return [["cargo", "check", *args]]
    result = [["cargo", "chef", "prepare", "--recipe-path", str(recipe)],
              ["cargo", "chef", "cook", "--recipe-path", str(recipe), *args]]
    if not graph.get("bin"):
        result.append(["cargo", "chef", "cook", "--check", "--recipe-path", str(recipe), *args])
    # The container runner builds in its own Cargo invocation. Preserve its
    # dependency features instead of unifying them with the application graph.
    for auxiliary in graph.get("auxiliary", []):
        auxiliary_args = build_arguments(auxiliary)
        result.extend([
            ["cargo", "chef", "cook", "--recipe-path", str(recipe), *auxiliary_args],
            ["cargo", "chef", "cook", "--check", "--recipe-path", str(recipe), *auxiliary_args],
        ])
    return result


def strip_redundant_target_editions(contents):
    """Remove deprecated target editions that chef copied from the package."""
    edition = tomllib.loads(contents).get("package", {}).get("edition")
    section = ""
    lines = []
    for line in contents.splitlines(keepends=True):
        heading = re.fullmatch(r"\s*\[+(.*?)\]+\s*", line.strip())
        if heading:
            section = heading[1]
        if section in ("lib", "bin", "test", "bench", "example") and re.match(r"\s*edition\s*=", line):
            if tomllib.loads(line)["edition"] == edition:
                continue
        lines.append(line)
    return "".join(lines)


def clean_recipe(recipe):
    data = json.loads(recipe.read_text())
    for manifest in data["skeleton"]["manifests"]:
        manifest["contents"] = strip_redundant_target_editions(manifest["contents"])
    recipe.write_text(json.dumps(data))


def warm(name, environment, run=subprocess.run):
    # chef overwrites workspace sources with stubs; never run it in a local or
    # self-hosted checkout. Only these disposable, trusted main jobs may cook.
    if not (environment.get("GITHUB_ACTIONS") == "true"
            and environment.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and environment.get("GITHUB_REF") == "refs/heads/main"):
        raise ValueError("Cache cooking requires a disposable GitHub-hosted main checkout")
    recipe = Path(environment["RUNNER_TEMP"]) / "dependency-cache-recipe.json"
    for command in commands(name, recipe):
        run(command, check=True)
        if command[1:3] == ["chef", "prepare"]:
            clean_recipe(recipe)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("matrix", "warm"))
    parser.add_argument("--graph", choices=GRAPHS)
    args = parser.parse_args()
    if args.mode == "matrix":
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write("matrix=" + json.dumps(matrix(), separators=(",", ":")) + "\n")
    elif args.graph:
        warm(args.graph, os.environ)
    else:
        parser.error("warm requires --graph")


if __name__ == "__main__":
    main()
