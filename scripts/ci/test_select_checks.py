"""Regression tests for affected-consumer selection and version-only releases."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from select_checks import SUITES, changed_paths, classify, git, satisfies, select_event, snapshot
from validate_metadata import validate


def fixture(version="0.4.0", requirement="0.4", external="1.0.0"):
    return {
        "Cargo.toml": '[workspace]\nmembers = ["crates/schema-forge-cli", "crates/local"]\n',
        "crates/schema-forge-cli/Cargo.toml": f'''[package]
name = "schema-forge-cli"
version = "{version}"
[dependencies]
local = {{ path = "../local", version = "{requirement}" }}
external = "1"
''',
        "crates/local/Cargo.toml": f'[package]\nname = "local"\nversion = "{version}"\n',
        "Cargo.lock": f'''version = 4
[[package]]
name = "schema-forge-cli"
version = "{version}"
dependencies = ["local {version}", "external"]
[[package]]
name = "local"
version = "{version}"
[[package]]
name = "external"
version = "{external}"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "test"
''',
        "CHANGELOG.md": f"# Changelog\n\n## [{version}] - 2026-10-06\n\n### Changed\n\n- Improved tooling.\n",
    }


class ConsumerTests(unittest.TestCase):
    def selected(self, paths, **kwargs):
        return {key for key, value in classify(paths, **kwargs).items() if value}

    def test_docs_only_and_empty_diff(self):
        self.assertEqual(self.selected(["README.md", "SECURITY.md", "CONTRIBUTING.md", "crates/schema-forge-core/README.md", "crates/schema-forge-cli/tests/site_e2e/README.md", "docs/guide.md", "skills/forge/SKILL.md", "docs/assets/demo.png"]), set())
        self.assertEqual(self.selected([]), set())

    def test_source_in_docs_is_not_documentation(self):
        self.assertEqual(self.selected(["docs/run.py"]), set(SUITES))

    def test_generator_selects_tooling_and_site(self):
        self.assertEqual(self.selected(["crates/schema-forge-codegen/src/site.rs"]), {"tooling", "site"})
        for path in ("site.rs", "codegen.rs"):
            self.assertEqual(self.selected(["crates/schema-forge-cli/src/commands/" + path]), {"tooling", "site"})

    def test_cli_small_command(self):
        self.assertEqual(self.selected(["crates/schema-forge-cli/src/commands/policies.rs"]), {"tooling", "cli"})

    def test_cli_server_entry_point_and_manifest(self):
        for path in ("src/main.rs", "src/cli.rs", "src/config_tooling.rs", "src/commands/backend.rs",
                     "src/commands/serve.rs", "src/commands/apply.rs", "Cargo.toml"):
            with self.subTest(path=path):
                self.assertEqual(self.selected(["crates/schema-forge-cli/" + path]), set(SUITES))

    def test_runtime_routes_and_shared_authorization(self):
        self.assertEqual(self.selected(["crates/schema-forge-acton/src/routes/health.rs"]), {"runtime", "postgres", "surrealdb", "site"})
        for path in ("shared_auth.rs", "authz/engine.rs", "tenancy_config.rs", "routes/creator_membership.rs"):
            with self.subTest(path=path):
                self.assertEqual(self.selected(["crates/schema-forge-acton/src/" + path]), set(SUITES))

    def test_backends_and_union(self):
        self.assertEqual(self.selected(["crates/schema-forge-postgres/src/backend.rs"]), {"postgres", "runtime"})
        self.assertEqual(self.selected(["crates/schema-forge-surrealdb/src/backend.rs"]), {"surrealdb", "site"})
        self.assertEqual(self.selected(["crates/schema-forge-mssql/src/lib.rs"]), {"mssql"})
        self.assertEqual(self.selected(["crates/schema-forge-mssql/src/lib.rs", "crates/schema-forge-codegen/src/lib.rs"]), {"mssql", "tooling", "site"})

    def test_cel_selects_consumers(self):
        self.assertEqual(self.selected(["crates/schema-forge-cel/src/lib.rs"]), {"cel", "tooling", "runtime", "postgres", "surrealdb", "site"})

    def test_shared_and_unknown_changes_are_broad(self):
        for path in ("crates/schema-forge-core/src/lib.rs", "crates/schema-forge-config/src/lib.rs", "rust-toolchain.toml", "scripts/tool.sh", ".github/workflows/ci.yml", "new-file"):
            with self.subTest(path=path):
                self.assertEqual(self.selected([path]), set(SUITES))

    def test_full_mode_even_for_docs(self):
        self.assertEqual(self.selected(["README.md"], full=True), set(SUITES))


class VersionTests(unittest.TestCase):
    def test_workspace_version_and_local_requirements_only(self):
        before = fixture()
        after = fixture("0.5.0", "0.5")
        paths = [path for path in after if before[path] != after[path]]
        self.assertFalse(any(classify(paths, before, after).values()))
        self.assertEqual(validate(after, before), [])

    def test_caret_constraints_accept_patch_updates(self):
        for requirement in ("0.4", "0.4.0", "^0.4.0", ">=0.4.0, <0.5.0", "~0.4.0", "0.4.*"):
            with self.subTest(requirement=requirement):
                self.assertTrue(satisfies("0.4.7", requirement))
                self.assertFalse(satisfies("0.5.0", requirement))

    def test_zero_version_caret(self):
        self.assertTrue(satisfies("0.0.4", "0.0"))
        self.assertFalse(satisfies("0.0.4", "0.0.3"))
        self.assertTrue(satisfies("1.7.0", "1"))

    def test_external_lock_only_change_runs_full(self):
        self.assertTrue(all(classify(["Cargo.lock"], fixture(), fixture(external="1.1.0")).values()))

    def test_external_dependency_and_features_run_full(self):
        for change in ('external = "2"', 'external = {version = "1", features = ["extra"]}'):
            with self.subTest(change=change):
                before = fixture()
                after = fixture()
                path = "crates/schema-forge-cli/Cargo.toml"
                after[path] = after[path].replace('external = "1"', change)
                self.assertTrue(all(classify([path], before, after).values()))

    def test_local_requirement_must_still_resolve(self):
        before = fixture()
        after = fixture("0.5.0", "0.4")
        paths = [path for path in after if before[path] != after[path]]
        self.assertTrue(all(classify(paths, before, after).values()))
        self.assertTrue(validate(after))

    def test_root_manifest_always_runs_full(self):
        before = fixture()
        after = fixture()
        after["Cargo.toml"] += 'resolver = "2"\n'
        self.assertTrue(all(classify(["Cargo.toml"], before, after).values()))

    def test_malformed_manifest_runs_full_and_fails_metadata(self):
        before = fixture()
        after = fixture()
        after["crates/local/Cargo.toml"] = "[broken"
        self.assertTrue(all(classify(["crates/local/Cargo.toml"], before, after).values()))
        self.assertTrue(validate(after))

    def test_deleted_manifest_runs_full(self):
        before = fixture()
        after = fixture()
        del after["crates/local/Cargo.toml"]
        self.assertTrue(all(classify(["crates/local/Cargo.toml"], before, after).values()))
        self.assertTrue(validate(after))

    def test_ambiguous_registry_package_reference_cannot_use_fast_path(self):
        before = fixture()
        after = fixture("0.4.1")
        collision = '''
[[package]]
name = "local"
version = "0.4.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "registry-local"
'''
        before["Cargo.lock"] += collision
        after["Cargo.lock"] += collision
        paths = [path for path in after if before[path] != after[path]]
        self.assertTrue(all(classify(paths, before, after).values()))

    def test_invalid_package_version_cannot_skip_checks(self):
        before = fixture()
        after = fixture()
        path = "crates/schema-forge-cli/Cargo.toml"
        after[path] = after[path].replace('version = "0.4.0"', 'version = "garbage"')
        self.assertTrue(all(classify([path], before, after).values()))
        self.assertTrue(validate(after))

    def test_stale_lock_and_missing_changelog_fail_metadata(self):
        before = fixture()
        after = fixture("0.5.0", "0.5")
        after["Cargo.lock"] = before["Cargo.lock"]
        after["CHANGELOG.md"] = before["CHANGELOG.md"]
        errors = validate(after, before)
        self.assertTrue(any("Cargo.lock" in error for error in errors))
        self.assertTrue(any("CHANGELOG" in error for error in errors))

    def test_release_tag_matches_cli_and_nonempty_changelog(self):
        files = fixture()
        self.assertEqual(validate(files, release_tag="v0.4.0"), [])
        self.assertTrue(validate(files, release_tag="v0.5.0"))
        files["CHANGELOG.md"] = "## [0.4.0]\n\n### Changed\n"
        self.assertTrue(validate(files, release_tag="v0.4.0"))


class GitDiffTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        git("init", "-q", repo=self.repo)
        git("config", "user.name", "CI test", repo=self.repo)
        git("config", "user.email", "ci@example.test", repo=self.repo)
        git("config", "commit.gpgsign", "false", repo=self.repo)
        self.write(fixture())
        self.commit()
        self.base = git("rev-parse", "HEAD", repo=self.repo).decode().strip()

    def write(self, files):
        for path, content in files.items():
            target = self.repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content)

    def commit(self):
        git("add", "-A", repo=self.repo)
        git("commit", "-qm", "test: update fixture", repo=self.repo)

    def test_delete_and_rename_select_both_consumers(self):
        old = "crates/schema-forge-codegen/src/old.rs"
        new = "crates/schema-forge-acton/src/routes/auth.rs"
        self.write({old: "fn sample() {}\n"})
        self.commit()
        base = git("rev-parse", "HEAD", repo=self.repo).decode().strip()
        (self.repo / old).unlink()
        self.write({new: "fn sample() {}\n"})
        self.commit()
        _, paths = changed_paths(base, "HEAD", self.repo)
        self.assertEqual(set(paths), {old, new})
        self.assertTrue(all(classify(paths).values()))

    def test_merge_base_ignores_changes_only_on_base_branch(self):
        git("checkout", "-qb", "topic", repo=self.repo)
        self.write({"README.md": "documentation\n"})
        self.commit()
        head = git("rev-parse", "HEAD", repo=self.repo).decode().strip()
        git("checkout", "-q", "--detach", self.base, repo=self.repo)
        self.write({"crates/schema-forge-core/src/lib.rs": "fn unrelated() {}\n"})
        self.commit()
        ancestor, paths = changed_paths("HEAD", head, self.repo)
        self.assertEqual(ancestor, self.base)
        self.assertEqual(paths, ["README.md"])
        checks, comparison_base = select_event("pull_request", "HEAD", head, self.repo)
        self.assertFalse(any(checks.values()))
        self.assertEqual(comparison_base, self.base)

    def test_main_push_documentation_is_fast(self):
        self.write({"README.md": "documentation\n"})
        self.commit()
        checks, base = select_event("push", self.base, "HEAD", self.repo)
        self.assertFalse(any(checks.values()))
        self.assertEqual(base, self.base)

    def test_main_push_covers_every_commit(self):
        self.write({"crates/schema-forge-mssql/src/lib.rs": "fn storage() {}\n"})
        self.commit()
        self.write({"README.md": "documentation\n"})
        self.commit()
        checks, base = select_event("push", self.base, "HEAD", self.repo)
        self.assertEqual({name for name, selected in checks.items() if selected}, {"mssql"})
        self.assertEqual(base, self.base)

    def test_main_push_version_only_is_fast_and_checks_changelog(self):
        self.write(fixture("0.4.1"))
        self.commit()
        checks, base = select_event("push", self.base, "HEAD", self.repo)
        self.assertFalse(any(checks.values()))
        before, after = snapshot(base, self.repo), snapshot("HEAD", self.repo)
        self.assertEqual(validate(after, before), [])
        after["CHANGELOG.md"] = before["CHANGELOG.md"]
        self.assertTrue(any("CHANGELOG" in error for error in validate(after, before)))

    def test_main_push_missing_or_invalid_base_falls_back_to_full(self):
        for base in (None, "", "0" * 40, "f" * 40, "HEAD^", "--unsafe"):
            with self.subTest(base=base):
                checks, comparison_base = select_event("push", base, "HEAD", self.repo)
                self.assertTrue(all(checks.values()))
                self.assertEqual(comparison_base, "")

    def test_main_push_rollback_falls_back_to_full(self):
        self.write({"crates/schema-forge-core/src/lib.rs": "fn shared() {}\n"})
        self.commit()
        before = git("rev-parse", "HEAD", repo=self.repo).decode().strip()
        checks, base = select_event("push", before, self.base, self.repo)
        self.assertTrue(all(checks.values()))
        self.assertEqual(base, "")

    def test_main_push_divergent_history_falls_back_to_full(self):
        git("checkout", "-qb", "topic", repo=self.repo)
        self.write({"README.md": "documentation\n"})
        self.commit()
        head = git("rev-parse", "HEAD", repo=self.repo).decode().strip()
        git("checkout", "-q", "--detach", self.base, repo=self.repo)
        self.write({"crates/schema-forge-core/src/lib.rs": "fn old_shared() {}\n"})
        self.commit()
        before = git("rev-parse", "HEAD", repo=self.repo).decode().strip()
        checks, base = select_event("push", before, head, self.repo)
        self.assertTrue(all(checks.values()))
        self.assertEqual(base, "")

    def test_nightly_and_manual_events_always_validate_fully(self):
        for event in ("schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                checks, base = select_event(event, None, "unknown", "not-a-repository")
                self.assertTrue(all(checks.values()))
                self.assertEqual(base, "")

    def test_push_cli_outputs_verified_base_for_metadata(self):
        self.write({"README.md": "documentation\n"})
        self.commit()
        script = Path(__file__).with_name("select_checks.py").resolve()
        output = self.repo / "outputs"
        result = subprocess.run(["python3", str(script), "--event", "push", "--base", self.base],
                                cwd=self.repo, env={**os.environ, "GITHUB_OUTPUT": str(output)},
                                check=True, capture_output=True, text=True)
        self.assertFalse(any(json.loads(result.stdout).values()))
        self.assertIn(f"base={self.base}\n", output.read_text())

    def test_push_cli_fallback_clears_metadata_base(self):
        script = Path(__file__).with_name("select_checks.py").resolve()
        output = self.repo / "outputs"
        result = subprocess.run(["python3", str(script), "--event", "push", "--base", "0" * 40],
                                cwd=self.repo, env={**os.environ, "GITHUB_OUTPUT": str(output)},
                                check=True, capture_output=True, text=True)
        self.assertTrue(all(json.loads(result.stdout).values()))
        self.assertIn("base=\n", output.read_text())
        self.assertNotIn(f"base={'0' * 40}\n", output.read_text())

    def test_pull_request_cli_requires_base(self):
        script = Path(__file__).with_name("select_checks.py").resolve()
        result = subprocess.run(["python3", str(script), "--event", "pull_request"],
                                cwd=self.repo, check=False, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires --base", result.stderr)

    def test_cli_emits_boolean_json_and_github_outputs(self):
        self.write({"crates/schema-forge-codegen/src/lib.rs": "fn render() {}\n"})
        self.commit()
        script = Path(__file__).with_name("select_checks.py").resolve()
        output = self.repo / "outputs"
        result = subprocess.run(["python3", str(script), "--base", self.base], cwd=self.repo,
                                env={**os.environ, "GITHUB_OUTPUT": str(output)}, check=True,
                                capture_output=True, text=True)
        checks = json.loads(result.stdout)
        self.assertTrue(checks["tooling"])
        self.assertFalse(checks["runtime"])
        self.assertIn("site=true\n", output.read_text())
        self.assertIn("mssql=false\n", output.read_text())

    def test_full_cli_does_not_require_base_or_git_repository(self):
        script = Path(__file__).with_name("select_checks.py").resolve()
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(["python3", str(script), "--head", "unknown-revision", "--full"],
                                    cwd=directory, check=True, capture_output=True, text=True)
        self.assertTrue(all(json.loads(result.stdout).values()))

    def test_cli_requires_base_for_component_selection(self):
        script = Path(__file__).with_name("select_checks.py").resolve()
        result = subprocess.run(["python3", str(script)], cwd=self.repo,
                                check=False, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--base is required", result.stderr)

    def test_snapshots_compare_real_version_only_commit(self):
        self.write(fixture("0.4.1"))
        self.commit()
        base, paths = changed_paths(self.base, "HEAD", self.repo)
        self.assertFalse(any(classify(paths, snapshot(base, self.repo), snapshot("HEAD", self.repo)).values()))


if __name__ == "__main__":
    unittest.main()
