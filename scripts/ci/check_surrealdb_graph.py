#!/usr/bin/env python3
"""Keep SurrealDB binaries free of embedded engines and the unused parser."""

from pathlib import Path
import sys


def check_graph(contents):
    for line in contents.splitlines():
        package, _, features = line.partition("|")
        name = package.split()[0] if package.split() else ""
        if name in ("surrealdb-engine-local", "surrealdb-kvs"):
            return False
        if name == "surrealdb" and any(
            feature.startswith("kv-") or feature in ("default", "parse")
            for feature in features.split(",")
        ):
            return False
    return any(line.startswith("surrealdb ") for line in contents.splitlines())


if __name__ == "__main__":
    if not check_graph(Path(sys.argv[1]).read_text()):
        sys.exit("SurrealDB must use remote protocols without local engines or default/parser features")
