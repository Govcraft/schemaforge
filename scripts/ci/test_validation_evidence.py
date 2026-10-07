"""Positive reuse and fail-closed coverage for main-push validation evidence."""

import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from select_checks import SUITES
from validation_evidence import (MAX_ARCHIVE, RECORD_FILE, artifact_name, compatible,
                                 decode_archive, find_reusable, main, record)

HEAD = "a" * 40
BASE = "b" * 40
MERGE = "c" * 40
TREE = "d" * 40
REPO = "Govcraft/schemaforge"


def archive(evidence, filename=RECORD_FILE):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as file:
        file.writestr(filename, json.dumps(evidence))
    return output.getvalue()


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.selection = {**dict.fromkeys(SUITES, "false"), "tooling": "true", "cli": "true", "base": BASE}
        self.selected = {name: self.selection[name] == "true" for name in SUITES}
        self.event = {"number": 227, "pull_request": {"head": {"sha": HEAD}}}
        self.evidence = record(self.selection, self.event, REPO, 123, 2, MERGE, TREE)
        self.run = {"id": 123, "run_attempt": 2, "head_sha": HEAD, "event": "pull_request",
                    "path": ".github/workflows/ci.yml", "status": "completed", "conclusion": "success",
                    "repository": {"full_name": REPO}, "html_url": "https://github.com/Govcraft/schemaforge/actions/runs/123"}
        self.pr = {"number": 227, "head": {"sha": HEAD}, "state": "closed", "merged_at": "now",
                   "merge_commit_sha": "e" * 40, "base": {"ref": "main"}}
        self.commit = {"sha": MERGE, "tree": {"sha": TREE}, "parents": [{"sha": BASE}, {"sha": HEAD}]}

    def accepts(self, evidence=None, run=None, commit=None, selected=None, tree=TREE):
        return compatible(evidence if evidence is not None else self.evidence,
                          run if run is not None else self.run, self.pr,
                          commit if commit is not None else self.commit,
                          REPO, tree, selected if selected is not None else self.selected)

    def test_successful_pr_merge_tree_and_coverage_can_be_reused(self):
        self.assertTrue(self.accepts())
        # The squash commit has a different ID from the provisional merge.
        self.assertNotEqual(self.pr["merge_commit_sha"], self.evidence["commit"])

    def test_changed_tree_and_added_required_components_reject_reuse(self):
        self.assertFalse(self.accepts(tree="f" * 40))
        self.assertFalse(self.accepts(selected=dict.fromkeys(SUITES, True)))

    def test_full_pr_coverage_can_satisfy_a_smaller_main_selection(self):
        self.evidence["components"] = dict.fromkeys(SUITES, True)
        self.assertTrue(self.accepts())

    def test_failed_cancelled_incomplete_wrong_event_or_workflow_runs_reject_reuse(self):
        for key, values in {"conclusion": ["failure", "cancelled", None], "status": ["in_progress", "queued"],
                            "event": ["push", "workflow_dispatch", "pull_request_target"],
                            "path": [".github/workflows/release.yml"], "head_sha": ["f" * 40]}.items():
            for value in values:
                with self.subTest(key=key, value=value):
                    run = {**self.run, key: value}
                    self.assertFalse(self.accepts(run=run))

    def test_record_identity_attempt_repository_metadata_and_schema_are_checked(self):
        for key, values in {"format": [True, 0, 2], "repository": ["another/repo"], "run_id": [True, 124, "123"],
                            "attempt": [1, "2"], "pr": [228, "227"], "pr_head": ["f" * 40],
                            "base": [None, "invalid"], "commit": [HEAD], "metadata": [False, "true"],
                            "tree": ["f" * 40]}.items():
            for value in values:
                with self.subTest(key=key, value=value):
                    self.assertFalse(self.accepts(evidence={**self.evidence, key: value}))
        for invalid in (None, [], {}, {**self.evidence["components"], "cli": "true"},
                        {**self.evidence["components"], "extra": True}):
            self.assertFalse(self.accepts(evidence={**self.evidence, "components": invalid}))
        self.assertFalse(self.accepts(evidence={}))
        self.assertFalse(self.accepts(run={**self.run, "repository": {"full_name": "other/repo"}}))

    def test_provisional_merge_commit_must_have_expected_pr_parents_and_tree(self):
        for change in ({"parents": []}, {"parents": [{"sha": HEAD}, {"sha": BASE}]},
                       {"parents": [{"sha": BASE}, {"sha": "f" * 40}]}, {"tree": {"sha": "f" * 40}}):
            self.assertFalse(self.accepts(commit={**self.commit, **change}))

    def test_archive_rejects_unexpected_files_invalid_json_and_oversized_payloads(self):
        self.assertEqual(decode_archive(archive(self.evidence)), self.evidence)
        for data in (b"invalid", archive(self.evidence, "unexpected.json"), archive([]),
                     archive({"padding": "x" * MAX_ARCHIVE})):
            with self.subTest(size=len(data)), self.assertRaises((ValueError, zipfile.BadZipFile)):
                decode_archive(data)

    def fake_api(self, artifacts=None, gates=None, pulls=None):
        tests = self
        class FakeAPI:
            def json(self, path):
                if "/pulls?" in path:
                    return pulls if pulls is not None else [tests.pr]
                if "/workflows/ci.yml/runs?" in path:
                    return {"workflow_runs": [tests.run]}
                if "/artifacts?" in path:
                    return {"artifacts": artifacts if artifacts is not None else [
                        {"id": 456, "name": artifact_name(2), "expired": False,
                         "size_in_bytes": 1000, "workflow_run": {"id": 123}}]}
                if "/git/commits/" in path:
                    return tests.commit
                if "/attempts/2/jobs?" in path:
                    return {"jobs": gates if gates is not None else [{"name": "Required CI", "conclusion": "success"}]}
                raise AssertionError(f"Unexpected endpoint: {path}")

            def raw(self, path):
                tests.assertTrue(path.endswith("/artifacts/456/zip"))
                return archive(tests.evidence)
        return FakeAPI()

    def find(self, api):
        return find_reusable(api, REPO, self.pr["merge_commit_sha"], TREE, self.selected)

    def test_lookup_requires_associated_merged_pr_and_successful_required_gate(self):
        self.assertEqual(self.find(self.fake_api()), self.run)
        for change in ({"state": "open"}, {"merged_at": None}, {"merge_commit_sha": HEAD}, {"base": {"ref": "other"}}):
            self.assertIsNone(self.find(self.fake_api(pulls=[{**self.pr, **change}])))
        for gates in ([], [{"name": "Required CI", "conclusion": "failure"}],
                      [{"name": "Required CI", "conclusion": "success"}] * 2):
            self.assertIsNone(self.find(self.fake_api(gates=gates)))

    def test_missing_expired_wrong_attempt_oversized_or_ambiguous_artifacts_fall_back(self):
        valid = {"id": 456, "name": artifact_name(2), "expired": False,
                 "size_in_bytes": 1000, "workflow_run": {"id": 123}}
        for artifacts in ([], [valid, valid], [{**valid, "expired": True}],
                          [{**valid, "name": artifact_name(1)}], [{**valid, "size_in_bytes": MAX_ARCHIVE + 1}],
                          [{**valid, "workflow_run": {"id": 124}}]):
            self.assertIsNone(self.find(self.fake_api(artifacts=artifacts)))

    def run_cli(self, event="push", ref="refs/heads/main", api=None):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            summary = Path(directory) / "summary"
            env = {"SELECTION_JSON": json.dumps(self.selection), "GITHUB_REPOSITORY": REPO,
                   "GITHUB_EVENT_NAME": event, "GITHUB_REF": ref, "GITHUB_OUTPUT": str(output),
                   "GITHUB_STEP_SUMMARY": str(summary)}
            with patch.dict(os.environ, env), patch("sys.argv", ["validation_evidence.py", "find"]), \
                    patch("validation_evidence.git", side_effect=[self.pr["merge_commit_sha"].encode(), TREE.encode()]), \
                    patch("validation_evidence.GitHub", return_value=api or self.fake_api()) as client:
                main()
            return output.read_text(), summary.read_text() if summary.exists() else "", client.called

    def test_cli_records_verified_reuse_and_links_the_source_run(self):
        output, summary, _ = self.run_cli()
        self.assertIn("reused=true\nreuse_run=123\n", output)
        self.assertIn(self.run["html_url"], summary)

    def test_pr_manual_scheduled_tags_and_missing_push_history_never_reuse(self):
        for event, ref in (("pull_request", "refs/pull/227/merge"), ("workflow_dispatch", "refs/heads/main"),
                           ("schedule", "refs/heads/main"), ("push", "refs/tags/v0.50.0")):
            output, _, called = self.run_cli(event, ref)
            self.assertEqual(output, "reused=false\n")
            self.assertFalse(called)
        self.selection["base"] = ""
        output, _, called = self.run_cli()
        self.assertEqual(output, "reused=false\n")
        self.assertFalse(called)

    def test_api_permission_network_and_malformed_evidence_failures_run_normal_validation(self):
        for error in (TimeoutError(), subprocess.CalledProcessError(1, "gh"), ValueError("bad JSON")):
            api = self.fake_api()
            with patch.object(api, "json", side_effect=error):
                output, _, _ = self.run_cli(api=api)
            self.assertEqual(output, "reused=false\n")


if __name__ == "__main__":
    unittest.main()
