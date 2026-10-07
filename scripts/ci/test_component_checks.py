"""Check portable feature boundaries through the component command interface."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class ComponentCommandTests(unittest.TestCase):
    def run_check(self, component, mode):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            cargo = directory / "cargo"
            cargo.write_text("#!/usr/bin/env python3\nimport json, os, sys\n"
                             "from pathlib import Path\n"
                             "Path(os.environ['COMMAND_OUTPUT']).write_text(json.dumps(sys.argv[1:]))\n")
            cargo.chmod(0o755)
            output = directory / "command.json"
            result = subprocess.run(["bash", str(Path(__file__).with_name("check_component.sh")), mode],
                                    env={**os.environ, "PATH": f"{directory}:{os.environ['PATH']}",
                                         "COMMAND_OUTPUT": str(output), "COMPONENT": component},
                                    capture_output=True, text=True, check=False)
            return result, json.loads(output.read_text()) if output.exists() else None

    def graph(self, args):
        return [args[i + 1] for i, arg in enumerate(args) if arg in ("-p", "--features")]

    def test_cli_tests_and_lints_share_server_extensions_without_database_features(self):
        _, tests = self.run_check("cli", "tests")
        _, lints = self.run_check("cli", "lint")
        self.assertEqual(self.graph(tests), ["schema-forge-cli", "server,oauth,sse"])
        self.assertEqual(self.graph(tests), self.graph(lints))
        self.assertEqual(tests[:2], ["nextest", "run"])
        self.assertEqual(lints[0], "clippy")
        self.assertIn("--no-default-features", tests)
        self.assertIn("--no-default-features", lints)
        self.assertIn("--all-targets", lints)
        self.assertEqual(lints[-3:], ["--", "-D", "warnings"])

    def test_tooling_tests_lints_and_doctests_share_the_portable_packages(self):
        graphs = []
        for mode in ("tests", "lint", "doctests"):
            result, args = self.run_check("tooling", mode)
            self.assertEqual(result.returncode, 0)
            self.assertIn("--no-default-features", args)
            self.assertNotIn("--features", args)
            graphs.append(self.graph(args))
        self.assertEqual(graphs[0], graphs[1])
        self.assertEqual(graphs[0], graphs[2])
        self.assertEqual(len(graphs[0]), 7)

    def test_runtime_graph_stays_portable_and_unknown_commands_fail_before_cargo(self):
        _, args = self.run_check("runtime", "tests")
        self.assertEqual(self.graph(args), ["schema-forge-acton", "schema-forge-backend"])
        self.assertIn("--no-default-features", args)
        for component, mode in (("unknown", "tests"), ("cli", "unknown"), ("cli", "doctests")):
            result, args = self.run_check(component, mode)
            self.assertNotEqual(result.returncode, 0)
            self.assertIsNone(args)


if __name__ == "__main__":
    unittest.main()
