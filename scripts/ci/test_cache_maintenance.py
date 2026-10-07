"""Test destructive-cache boundaries and dependency-warming feature fidelity."""

from pathlib import Path
import subprocess
import unittest
from unittest.mock import Mock, patch

from cleanup_caches import GitHub, cleanup, closed_caches, pull_number
from normalize_toolchains import normalize, unused_toolchains
import test_component_checks
from warm_caches import GRAPHS, build_arguments, commands, matrix, strip_redundant_target_editions, warm


ROOT = Path(__file__).resolve().parents[2]
HOSTED = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}


class ToolchainTests(unittest.TestCase):
    def test_local_and_self_hosted_toolchains_are_never_removed(self):
        for env in ({}, {**HOSTED, "RUNNER_ENVIRONMENT": "self-hosted"}):
            run = Mock()
            normalize(env, run)
            run.assert_not_called()

    def test_unused_installed_versions_are_removed_but_active_pinned_compiler_is_kept(self):
        active = "1.97.1-x86_64-unknown-linux-gnu"
        values = ["rustc 1.97.1 (8bab26f4f 2026-07-14)", f"{active} (overridden)",
                  f"stable-x86_64-unknown-linux-gnu (default)\n{active}\nnightly-x86_64-unknown-linux-gnu\n"]
        run = Mock(side_effect=[*[subprocess.CompletedProcess([], 0, stdout=value) for value in values], None, None])
        with patch("normalize_toolchains.Path.read_text", return_value='[toolchain]\nchannel = "1.97.1"'):
            normalize(HOSTED, run)
        removed = [call.args[0] for call in run.call_args_list if call.args[0][:3] == ["rustup", "toolchain", "uninstall"]]
        self.assertEqual(removed, [["rustup", "toolchain", "uninstall", "stable-x86_64-unknown-linux-gnu"],
                                   ["rustup", "toolchain", "uninstall", "nightly-x86_64-unknown-linux-gnu"]])

    def test_unexpected_compiler_or_missing_active_toolchain_fails_before_uninstall(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, stdout="rustc 1.99.0 (hash date)"))
        with patch("normalize_toolchains.Path.read_text", return_value='[toolchain]\nchannel = "1.97.1"'):
            with self.assertRaises(ValueError):
                normalize(HOSTED, run)
        self.assertEqual(run.call_count, 1)
        with self.assertRaises(ValueError):
            unused_toolchains("stable-x86_64-pc-windows-msvc", "1.97.1-x86_64-pc-windows-msvc")

    def test_windows_toolchain_names_and_already_normalized_runners(self):
        active = "1.97.1-x86_64-pc-windows-msvc"
        self.assertEqual(unused_toolchains(f"stable-x86_64-pc-windows-msvc\n{active} (default)", active),
                         ["stable-x86_64-pc-windows-msvc"])
        self.assertEqual(unused_toolchains(active, active), [])


class CleanupTests(unittest.TestCase):
    def caches(self):
        return [{"id": 1, "ref": "refs/heads/main", "size_in_bytes": 100},
                {"id": 2, "ref": "refs/pull/227/merge", "size_in_bytes": 200},
                {"id": 3, "ref": "refs/pull/228/merge", "size_in_bytes": 300},
                {"id": 4, "ref": "refs/tags/v0.49.0", "size_in_bytes": 400},
                {"id": 5, "ref": "refs/heads/feature", "size_in_bytes": 500}]

    def test_only_closed_pr_merge_refs_are_candidates(self):
        caches = self.caches() + [{"id": True, "ref": "refs/pull/227/merge"},
                                 {"id": -1, "ref": "refs/pull/227/merge"}, {"id": 6, "ref": None},
                                 {"id": 7, "ref": "refs/pull/227/head"}]
        self.assertEqual([c["id"] for c in closed_caches(caches, {227: "closed", 228: "open"})], [2])
        for ref in (None, "refs/heads/main", "refs/tags/v0.49.0", "refs/pull/0/merge", "refs/pull/227/merge/extra"):
            self.assertIsNone(pull_number({"ref": ref}))

    def api(self, states):
        caches = self.caches()
        def request(path, method="GET", paginate=False):
            if "/actions/caches?" in path:
                self.assertTrue(paginate)
                return [{"actions_caches": caches[:2]}, {"actions_caches": caches[2:]}]
            if "/pulls/" in path:
                return {"state": states[int(path.rsplit("/", 1)[1])]}
            self.assertEqual(method, "DELETE")
            return None
        return Mock(request=Mock(side_effect=request))

    def test_all_pages_and_live_pr_states_are_checked_before_deletion(self):
        api = self.api({227: "closed", 228: "open"})
        self.assertEqual([c["id"] for c in cleanup(api, "Govcraft/schemaforge")], [2])
        self.assertEqual([call.args[0] for call in api.request.call_args_list if call.kwargs.get("method") == "DELETE"],
                         ["repos/Govcraft/schemaforge/actions/caches/2"])
        self.assertEqual(api.request.call_args_list[-1].args[0], "repos/Govcraft/schemaforge/actions/caches/2")

    def test_close_event_is_limited_to_its_pr_and_reopened_pr_is_preserved(self):
        api = self.api({227: "closed", 228: "closed"})
        self.assertEqual([c["id"] for c in cleanup(api, "Govcraft/schemaforge", 228)], [3])
        self.assertFalse(any(call.args[0].endswith("/pulls/227") for call in api.request.call_args_list))
        api = self.api({228: "open"})
        self.assertEqual(cleanup(api, "Govcraft/schemaforge", 228), [])

    def test_api_failure_stops_cleanup_before_any_deletion(self):
        api = Mock()
        api.request.side_effect = [[{"actions_caches": self.caches()}], {"state": "closed"}, RuntimeError("API unavailable")]
        with self.assertRaises(RuntimeError):
            cleanup(api, "Govcraft/schemaforge")
        self.assertFalse(any(call.kwargs.get("method") == "DELETE" for call in api.request.call_args_list))
        with self.assertRaises(ValueError):
            cleanup(api, "Govcraft/schemaforge/other")

    def test_concurrent_deletion_is_idempotent_but_other_api_errors_fail(self):
        api = GitHub()
        with patch("cleanup_caches.subprocess.run", return_value=subprocess.CompletedProcess([], 1, "", "gh: Not Found (HTTP 404)")):
            self.assertIsNone(api.request("cache/1", method="DELETE"))
            with self.assertRaises(RuntimeError):
                api.request("cache/1")
        with patch("cleanup_caches.subprocess.run", return_value=subprocess.CompletedProcess([], 1, "", "gh: Forbidden (HTTP 403)")):
            with self.assertRaises(RuntimeError):
                api.request("cache/1", method="DELETE")


class WarmingTests(unittest.TestCase):
    def selectors(self, args):
        return ([args[i + 1] for i, arg in enumerate(args) if arg == "-p"],
                [args[i + 1] for i, arg in enumerate(args) if arg == "--features"],
                "--no-default-features" in args)

    def test_portable_warming_matches_actual_validation_graphs(self):
        interface = test_component_checks.ComponentCommandTests()
        for name in ("tooling", "runtime", "cli"):
            result, args = interface.run_check(name, "tests")
            self.assertEqual(result.returncode, 0)
            self.assertEqual(self.selectors(args), self.selectors(build_arguments(GRAPHS[name])))

    def test_backend_warming_matches_workflow_feature_environments_and_cache_keys(self):
        postgres = (ROOT / ".github/workflows/postgres-conditional.yml").read_text()
        surrealdb = (ROOT / ".github/workflows/surrealdb-runtime.yml").read_text()
        for name in ("postgres", "postgres-disabled"):
            graph = GRAPHS[name]
            self.assertIn(f"CARGO_FEATURES: {graph['features']}", postgres)
            self.assertIn(f"cache-key: {graph['key']}", postgres)
        self.assertIn(f"features={GRAPHS['surrealdb']['features']}", surrealdb)
        self.assertIn(f"cache-key: {GRAPHS['surrealdb']['key']}", surrealdb)
        mssql = (ROOT / ".github/workflows/mssql-integration.yml").read_text()
        for name in ("mssql", "windows"):
            self.assertIn(f"cache-key: {GRAPHS[name]['key']}", mssql)

    def test_matrix_preserves_platform_features_and_separate_dependency_graphs(self):
        entries = matrix()["include"]
        self.assertEqual(len(entries), len({entry["cache_key"] for entry in entries}))
        self.assertEqual([entry["graph"] for entry in entries if entry["runner"] == "windows-2025"], ["windows"])
        for entry in entries:
            expected = GRAPHS[entry["graph"]].get("features", "") if entry["graph"].startswith("postgres") else ""
            self.assertEqual(entry["cargo_features"], expected)

    def test_warming_never_executes_tests_and_windows_only_checks_its_binary(self):
        for name in GRAPHS:
            for command in commands(name, Path("recipe.json")):
                self.assertNotIn("nextest", command)
                self.assertNotIn("test", command)
                self.assertNotIn("clippy", command)
                if command[2:3] == ["cook"]:
                    self.assertIn("--locked", command)
        self.assertEqual(commands("windows", Path("recipe.json"))[0][:2], ["cargo", "check"])
        self.assertEqual(len(commands("site", Path("recipe.json"))), 2)
        self.assertEqual(len(commands("runtime", Path("recipe.json"))), 3)

    def test_source_overwriting_is_forbidden_outside_disposable_main_jobs(self):
        run = Mock()
        for env in ({}, HOSTED, {**HOSTED, "GITHUB_REF": "refs/pull/229/merge"},
                    {**HOSTED, "GITHUB_REF": "refs/heads/main", "RUNNER_ENVIRONMENT": "self-hosted"}):
            with self.assertRaises(ValueError):
                warm("runtime", env, run)
        run.assert_not_called()
        with patch("warm_caches.clean_recipe") as clean:
            warm("runtime", {**HOSTED, "GITHUB_REF": "refs/heads/main", "RUNNER_TEMP": "/tmp/fixture"}, run)
            clean.assert_called_once_with(Path("/tmp/fixture/dependency-cache-recipe.json"))
        self.assertEqual([call.args[0] for call in run.call_args_list], commands("runtime", Path("/tmp/fixture/dependency-cache-recipe.json")))

    def test_recipe_cleaning_keeps_package_edition_and_explicit_different_target_editions(self):
        contents = '[package]\nname = "fixture"\nedition = "2021"\n[lib]\nedition = "2021"\n[[test]]\nedition = "2024"\n'
        cleaned = strip_redundant_target_editions(contents)
        self.assertIn('[package]\nname = "fixture"\nedition = "2021"', cleaned)
        self.assertNotIn('[lib]\nedition', cleaned)
        self.assertIn('[[test]]\nedition = "2024"', cleaned)
        self.assertEqual(strip_redundant_target_editions('[workspace]\nmembers = []\n'), '[workspace]\nmembers = []\n')

    def test_shared_cache_policy_and_close_event_do_not_execute_pr_code(self):
        action = (ROOT / ".github/actions/rust-cache/action.yml").read_text()
        self.assertIn("github.ref == 'refs/heads/main'", action)
        self.assertIn("inputs.lookup-only != 'true'", action)
        self.assertNotIn("add-rust-environment-hash-key: false", action)
        workflow = (ROOT / ".github/workflows/cache-maintenance.yml").read_text()
        self.assertIn("ref: main", workflow)
        self.assertNotIn("github.event.pull_request.head", workflow)
        self.assertNotIn("needs: required", workflow)


if __name__ == "__main__":
    unittest.main()
