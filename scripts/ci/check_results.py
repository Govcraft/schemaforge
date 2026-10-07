#!/usr/bin/env python3
"""Fail closed when required or selected GitHub Actions jobs do not pass."""

import argparse
import json
import os
import sys

from select_checks import SUITES

VALIDATION_JOBS = ("metadata", "tooling", "runtime", "cli", "postgres", "surrealdb", "mssql", "cel", "site")


def check_required(jobs, event="", ref=""):
    failures = []
    if jobs.get("changes", {}).get("result") != "success":
        failures.append(f"changes: {jobs.get('changes', {}).get('result', 'missing')}")
    outputs = jobs.get("changes", {}).get("outputs", {})
    for name in SUITES:
        if outputs.get(name) not in ("true", "false"):
            failures.append(f"{name}: missing or invalid component selection")
    reused = outputs.get("reused", "false")
    if reused not in ("true", "false"):
        failures.append("Missing or invalid reuse decision")
    expected = "success"
    if reused == "true":
        if event != "push" or ref != "refs/heads/main":
            failures.append("Only a main push can reuse PR validation")
        run = outputs.get("reuse_run", "")
        if not isinstance(run, str) or not run.isdecimal() or int(run) <= 0:
            failures.append("Reused validation requires a successful source run")
        expected = "skipped"
    result = jobs.get("validation", {}).get("result")
    if result != expected:
        failures.append(f"validation: expected {expected}, got {result or 'missing'}")
    return failures


def check_validation(jobs, inputs, manual=False):
    failures = []
    if not manual:
        for name in SUITES:
            if not isinstance(inputs.get(name), bool):
                failures.append(f"{name}: missing or invalid boolean input")
    for name in VALIDATION_JOBS:
        selected = manual or name == "metadata" or inputs.get(name) is True
        result = jobs.get(name, {}).get("result")
        if selected and result != "success":
            failures.append(f"{name}: selected job {result or 'missing'}")
        elif not selected and result not in ("success", "skipped"):
            failures.append(f"{name}: unselected job {result or 'missing'}")
    unexpected = jobs.keys() - set(VALIDATION_JOBS)
    if unexpected:
        failures.append("Unexpected validation jobs: " + ", ".join(sorted(unexpected)))
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gate", choices=("required", "validation"))
    args = parser.parse_args()
    try:
        jobs = json.loads(os.environ["NEEDS_JSON"])
        if not isinstance(jobs, dict) or any(not isinstance(job, dict) for job in jobs.values()):
            raise ValueError("NEEDS_JSON must contain job objects")
        if args.gate == "required":
            failures = check_required(jobs, os.environ.get("GITHUB_EVENT_NAME", ""),
                                      os.environ.get("GITHUB_REF", ""))
        else:
            inputs = json.loads(os.environ.get("INPUTS_JSON", "{}"))
            manual = os.environ.get("MANUAL", "false")
            if not isinstance(inputs, dict) or manual not in ("true", "false"):
                raise ValueError("Invalid validation inputs or MANUAL value")
            failures = check_validation(jobs, inputs, manual == "true")
    except (KeyError, ValueError, TypeError, AttributeError) as error:
        sys.exit(f"Invalid CI result data: {error}")
    if failures:
        sys.exit("CI gate failed: " + "; ".join(failures))
    print("Required checks passed.")


if __name__ == "__main__":
    main()
