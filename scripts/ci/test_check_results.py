"""Negative coverage for the stable merge gate and reusable validation gate."""

import json
import os
from pathlib import Path
import subprocess
import unittest

from check_results import VALIDATION_JOBS, check_required, check_validation
from select_checks import SUITES


class GateTests(unittest.TestCase):
    def inputs(self, **selected):
        return {name: selected.get(name, False) for name in SUITES}

    def results(self, **overrides):
        return {name: {"result": overrides.get(name, "success" if name == "metadata" else "skipped")}
                for name in VALIDATION_JOBS}

    def required_results(self):
        return {"changes": {"result": "success", "outputs": dict.fromkeys(SUITES, "false")},
                "validation": {"result": "success"}}

    def test_docs_only_validation_allows_skipped_consumers(self):
        self.assertEqual(check_validation(self.results(), self.inputs()), [])

    def test_selected_success_and_unselected_success_pass(self):
        jobs = self.results(tooling="success", site="success", runtime="success")
        self.assertEqual(check_validation(jobs, self.inputs(tooling=True, site=True)), [])

    def test_selected_failed_skipped_or_cancelled_jobs_fail(self):
        for result in ("failure", "skipped", "cancelled"):
            with self.subTest(result=result):
                self.assertTrue(check_validation(self.results(tooling=result), self.inputs(tooling=True)))

    def test_unselected_failed_cancelled_or_missing_jobs_fail(self):
        for result in ("failure", "cancelled", None):
            with self.subTest(result=result):
                self.assertTrue(check_validation(self.results(tooling=result), self.inputs()))

    def test_cli_selection_requires_portable_cli_but_allows_surrealdb_skip(self):
        self.assertTrue(check_validation(self.results(), self.inputs(cli=True)))
        self.assertEqual(check_validation(self.results(cli="success"), self.inputs(cli=True)), [])
        self.assertTrue(check_validation(self.results(surrealdb="success"), self.inputs(cli=True)))

    def test_selected_cli_failed_skipped_or_cancelled_jobs_fail(self):
        for result in ("failure", "skipped", "cancelled"):
            with self.subTest(result=result):
                self.assertTrue(check_validation(self.results(cli=result), self.inputs(cli=True)))

    def test_full_backend_selection_still_requires_surrealdb(self):
        inputs = self.inputs(cli=True, surrealdb=True)
        self.assertTrue(check_validation(self.results(cli="success"), inputs))
        self.assertEqual(check_validation(self.results(cli="success", surrealdb="success"), inputs), [])

    def test_metadata_always_required(self):
        for result in ("failure", "skipped", "cancelled"):
            with self.subTest(result=result):
                self.assertTrue(check_validation(self.results(metadata=result), self.inputs()))

    def test_full_manual_run_requires_all_jobs_without_inputs(self):
        jobs = {name: {"result": "success"} for name in VALIDATION_JOBS}
        self.assertEqual(check_validation(jobs, {}, manual=True), [])
        jobs["mssql"]["result"] = "skipped"
        self.assertTrue(check_validation(jobs, {}, manual=True))

    def test_missing_jobs_and_missing_or_nonboolean_inputs_fail(self):
        jobs = self.results()
        del jobs["site"]
        self.assertTrue(check_validation(jobs, self.inputs()))
        for invalid in (None, "false", "true", 0, 1):
            with self.subTest(invalid=invalid):
                self.assertTrue(check_validation(self.results(), self.inputs(site=invalid)))
        self.assertTrue(check_validation(self.results(), {}))

    def test_unexpected_job_cannot_silently_bypass_gate(self):
        jobs = self.results()
        jobs["new-suite"] = {"result": "skipped"}
        self.assertTrue(check_validation(jobs, self.inputs()))

    def test_required_gate_passes_successful_selection_and_validation(self):
        self.assertEqual(check_required(self.required_results()), [])

    def test_required_gate_rejects_unsuccessful_or_missing_upstream_jobs(self):
        for name in ("changes", "validation"):
            for result in ("failure", "skipped", "cancelled", None):
                with self.subTest(name=name, result=result):
                    jobs = self.required_results()
                    jobs[name]["result"] = result
                    self.assertTrue(check_required(jobs))
            jobs = self.required_results()
            del jobs[name]
            self.assertTrue(check_required(jobs))

    def test_required_gate_rejects_invalid_or_missing_selector_outputs(self):
        for invalid in (None, "", "TRUE", True, False):
            with self.subTest(invalid=invalid):
                jobs = self.required_results()
                jobs["changes"]["outputs"]["runtime"] = invalid
                self.assertTrue(check_required(jobs))
        jobs = self.required_results()
        del jobs["changes"]["outputs"]["site"]
        self.assertTrue(check_required(jobs))

    def test_only_verified_main_reuse_allows_skipped_validation(self):
        jobs = self.required_results()
        jobs["changes"]["outputs"].update(reused="true", reuse_run="123")
        jobs["validation"]["result"] = "skipped"
        self.assertEqual(check_required(jobs, "push", "refs/heads/main"), [])
        for event, ref in (("pull_request", "refs/heads/main"), ("schedule", "refs/heads/main"),
                           ("workflow_dispatch", "refs/heads/main"), ("push", "refs/tags/v1.0.0"),
                           ("push", "refs/heads/other")):
            self.assertTrue(check_required(jobs, event, ref))
        for value in (None, "", "false", "0", "abc"):
            jobs["changes"]["outputs"]["reuse_run"] = value
            self.assertTrue(check_required(jobs, "push", "refs/heads/main"))

    def test_failed_lookup_and_invalid_reuse_decisions_cannot_allow_skipped_validation(self):
        jobs = self.required_results()
        jobs["validation"]["result"] = "skipped"
        for value in ("false", "", "TRUE", True, None):
            jobs["changes"]["outputs"]["reused"] = value
            self.assertTrue(check_required(jobs, "push", "refs/heads/main"))

    def test_cli_rejects_malformed_job_data(self):
        script = Path(__file__).with_name("check_results.py")
        for payload in ("not-json", "[]", '{"metadata": false}'):
            with self.subTest(payload=payload):
                result = subprocess.run(["python3", str(script), "validation"],
                                        env={**os.environ, "NEEDS_JSON": payload},
                                        capture_output=True, text=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Invalid CI result data", result.stderr)

    def test_cli_required_success(self):
        script = Path(__file__).with_name("check_results.py")
        result = subprocess.run(["python3", str(script), "required"],
                                env={**os.environ, "NEEDS_JSON": json.dumps(self.required_results())},
                                capture_output=True, text=True, check=True)
        self.assertIn("passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
