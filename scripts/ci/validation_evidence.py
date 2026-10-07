#!/usr/bin/env python3
"""Record successful PR validation and reuse it for identical main file trees."""

import argparse
import io
import json
import os
from pathlib import Path
import re
import subprocess
import time
import zipfile

from select_checks import SUITES, git

FORMAT = 1
WORKFLOW = ".github/workflows/ci.yml"
RECORD_FILE = "validation-evidence.json"
MAX_ARCHIVE = 128 * 1024


def artifact_name(attempt):
    return f"ci-validation-v{FORMAT}-{attempt}"


def selected_components(outputs):
    if any(outputs.get(name) not in ("true", "false") for name in SUITES):
        raise ValueError("Missing or invalid component selection")
    return {name: outputs[name] == "true" for name in SUITES}


def record(selection, event, repository, run_id, attempt, commit, tree):
    if event.get("pull_request", {}).get("head", {}).get("sha") is None:
        raise ValueError("Validation evidence requires a pull request event")
    return {"format": FORMAT, "repository": repository, "run_id": run_id,
            "attempt": attempt, "pr": event["number"],
            "pr_head": event["pull_request"]["head"]["sha"],
            "base": selection["base"], "commit": commit, "tree": tree,
            "components": selected_components(selection), "metadata": True}


def compatible(evidence, run, pr, commit, repository, tree, selected):
    """Require the successful run, merge provenance, exact tree, and coverage."""
    try:
        components = evidence["components"]
        return (type(evidence["format"]) is int and evidence["format"] == FORMAT
                and evidence["repository"] == repository
                and type(evidence["run_id"]) is int and evidence["run_id"] == run["id"]
                and type(evidence["attempt"]) is int and evidence["attempt"] == run["run_attempt"]
                and type(evidence["pr"]) is int and evidence["pr"] == pr["number"]
                and evidence["pr_head"] == run["head_sha"] == pr["head"]["sha"]
                and run["repository"]["full_name"] == repository
                and run["event"] == "pull_request" and run["path"] == WORKFLOW
                and run["status"] == "completed" and run["conclusion"] == "success"
                and evidence["commit"] == commit["sha"]
                and re.fullmatch(r"[0-9a-f]{40}", evidence["base"]) is not None
                and [parent["sha"] for parent in commit["parents"]]
                == [evidence["base"], evidence["pr_head"]]
                and evidence["tree"] == commit["tree"]["sha"] == tree
                and evidence["metadata"] is True
                and isinstance(components, dict) and set(components) == set(SUITES)
                and all(type(value) is bool for value in components.values())
                and all(not required or components[name] for name, required in selected.items()))
    except (KeyError, TypeError, AttributeError):
        return False


def decode_archive(data):
    if len(data) > MAX_ARCHIVE:
        raise ValueError("Validation evidence archive is too large")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        if archive.namelist() != [RECORD_FILE] or archive.getinfo(RECORD_FILE).file_size > MAX_ARCHIVE:
            raise ValueError("Unexpected validation evidence archive contents")
        evidence = json.loads(archive.read(RECORD_FILE))
    if not isinstance(evidence, dict):
        raise ValueError("Validation evidence must be an object")
    return evidence


class GitHub:
    """Use the runner's authenticated gh client without exposing its token."""

    def __init__(self):
        self.deadline = time.monotonic() + 30

    def raw(self, path):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("PR evidence lookup exceeded its time budget")
        result = subprocess.run([os.environ.get("GH_BIN", "gh"), "api", path],
                                capture_output=True, timeout=min(10, remaining), check=True)
        if len(result.stdout) > 8 * 1024 * 1024:
            raise ValueError("GitHub API response is too large")
        return result.stdout

    def json(self, path):
        return json.loads(self.raw(path))


def find_reusable(api, repository, head, tree, selected):
    """Return a successful PR run only with complete, matching evidence."""
    prefix = f"repos/{repository}"
    pulls = api.json(f"{prefix}/commits/{head}/pulls?per_page=100")
    for pr in pulls:
        if not (pr.get("state") == "closed" and pr.get("merged_at")
                and pr.get("merge_commit_sha") == head and pr.get("base", {}).get("ref") == "main"):
            continue
        runs = api.json(f"{prefix}/actions/workflows/ci.yml/runs?event=pull_request"
                        f"&head_sha={pr['head']['sha']}&status=success&per_page=10")["workflow_runs"]
        for run in runs:
            if (run.get("event") != "pull_request" or run.get("path") != WORKFLOW
                    or run.get("status") != "completed" or run.get("conclusion") != "success"):
                continue
            artifacts = api.json(f"{prefix}/actions/runs/{run['id']}/artifacts?per_page=100")["artifacts"]
            matching = [artifact for artifact in artifacts
                        if artifact.get("name") == artifact_name(run["run_attempt"])
                        and artifact.get("expired") is False
                        and type(artifact.get("size_in_bytes")) is int
                        and 0 < artifact["size_in_bytes"] <= MAX_ARCHIVE
                        and artifact.get("workflow_run", {}).get("id") == run["id"]]
            if len(matching) != 1:
                continue
            evidence = decode_archive(api.raw(f"{prefix}/actions/artifacts/{matching[0]['id']}/zip"))
            if not isinstance(evidence.get("commit"), str) or not re.fullmatch(r"[0-9a-f]{40}", evidence["commit"]):
                continue
            commit = api.json(f"{prefix}/git/commits/{evidence['commit']}")
            if not compatible(evidence, run, pr, commit, repository, tree, selected):
                continue
            jobs = api.json(f"{prefix}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs?per_page=100")["jobs"]
            gates = [job for job in jobs if job.get("name") == "Required CI"]
            if len(gates) == 1 and gates[0].get("conclusion") == "success":
                return run
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("record", "find"))
    args = parser.parse_args()
    selection = json.loads(os.environ["SELECTION_JSON"])
    commit = git("rev-parse", "HEAD").decode().strip()
    tree = git("rev-parse", "HEAD^{tree}").decode().strip()
    repository = os.environ["GITHUB_REPOSITORY"]
    if args.mode == "record":
        if os.environ["GITHUB_EVENT_NAME"] != "pull_request":
            raise ValueError("Only successful PR CI produces reusable evidence")
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        evidence = record(selection, event, repository, int(os.environ["GITHUB_RUN_ID"]),
                          int(os.environ["GITHUB_RUN_ATTEMPT"]), commit, tree)
        Path(RECORD_FILE).write_text(json.dumps(evidence, sort_keys=True) + "\n")
        return

    run = None
    try:
        selected = selected_components(selection)
        if (os.environ["GITHUB_EVENT_NAME"] == "push" and os.environ["GITHUB_REF"] == "refs/heads/main"
                and selection.get("base")):
            run = find_reusable(GitHub(), repository, commit, tree, selected)
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError,
            AttributeError, zipfile.BadZipFile, RuntimeError) as error:
        print(f"PR validation evidence unavailable ({type(error).__name__}); running component validation.")
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"reused={str(run is not None).lower()}\n")
        if run:
            output.write(f"reuse_run={run['id']}\n")
    if run:
        message = f"Reusing successful PR CI run {run['id']} for identical tree {tree}."
        print(message)
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            summary.write(f"{message}\n\n[Validated PR run]({run['html_url']})\n")
    else:
        print("No matching successful PR evidence; running component validation.")


if __name__ == "__main__":
    main()
