#!/usr/bin/env python3
"""Delete only caches scoped to pull requests GitHub confirms are closed."""

import argparse
import json
import os
import re
import subprocess


class GitHub:
    def request(self, path, method="GET", paginate=False):
        command = [os.environ.get("GH_BIN", "gh"), "api", "--method", method, path]
        if paginate:
            command.extend(["--paginate", "--slurp"])
        result = subprocess.run(command, capture_output=True, text=True, timeout=60, check=False)
        if result.returncode:
            # A concurrent close-event cleanup can have deleted the same ID.
            if method == "DELETE" and "HTTP 404" in result.stderr:
                return None
            raise RuntimeError(f"GitHub cache API failed for {method} {path}: {result.stderr.strip()}")
        return json.loads(result.stdout) if result.stdout.strip() else None


def pull_number(cache):
    ref = cache.get("ref")
    match = re.fullmatch(r"refs/pull/([1-9][0-9]*)/merge", ref) if isinstance(ref, str) else None
    return int(match[1]) if match else None


def closed_caches(caches, states):
    return [cache for cache in caches
            if pull_number(cache) is not None and states.get(pull_number(cache)) == "closed"
            and type(cache.get("id")) is int and cache["id"] > 0]


def cleanup(api, repository, number=None):
    if re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) is None:
        raise ValueError("Invalid repository")
    prefix = f"repos/{repository}"
    pages = api.request(f"{prefix}/actions/caches?per_page=100", paginate=True)
    caches = [cache for page in pages for cache in page["actions_caches"]]
    numbers = {value for cache in caches if (value := pull_number(cache)) is not None
               and (number is None or value == number)}
    # Resolve every PR state before any deletion; payloads are not authority.
    states = {value: api.request(f"{prefix}/pulls/{value}")["state"] for value in sorted(numbers)}
    candidates = closed_caches(caches, states)
    for cache in candidates:
        api.request(f"{prefix}/actions/caches/{cache['id']}", method="DELETE")
    size = sum(cache.get("size_in_bytes", 0) for cache in candidates)
    print(f"Removed {len(candidates)} closed-PR caches ({size / 1_000_000_000:.2f} GB).")
    return candidates


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pr", type=int)
    args = parser.parse_args()
    if args.pr is not None and args.pr <= 0:
        parser.error("PR number must be positive")
    cleanup(GitHub(), os.environ["GITHUB_REPOSITORY"], args.pr)


if __name__ == "__main__":
    main()
