"""Exercise prerequisite setup's host boundary and mirror failure behavior."""

from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock

from install_prerequisites import install, missing_packages, ubuntu_mirror


HOSTED = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}


class PrerequisiteTests(unittest.TestCase):
    def test_persistent_and_non_ubuntu_hosts_are_rejected_before_any_command(self):
        run = Mock()
        for environment, release in (({}, {"ID": "ubuntu"}),
                                     ({**HOSTED, "RUNNER_ENVIRONMENT": "self-hosted"}, {"ID": "ubuntu"}),
                                     (HOSTED, {"ID": "arch"})):
            with self.assertRaises(ValueError):
                install(["protobuf-compiler"], environment, release, run=run)
        run.assert_not_called()

    def test_only_fully_installed_packages_are_skipped(self):
        run = Mock(side_effect=[subprocess.CompletedProcess([], 0, "install ok installed"),
                                subprocess.CompletedProcess([], 0, "deinstall ok config-files"),
                                subprocess.CompletedProcess([], 1, "")])
        self.assertEqual(missing_packages(["protobuf-compiler", "protobuf-compiler", "libkrb5-dev", "libssl-dev"], run),
                         ["libkrb5-dev", "libssl-dev"])
        for value in ([], ["-y"], ["package;command"], ["pkg*"]):
            with self.assertRaises(ValueError):
                missing_packages(value, Mock())

    def runner(self, installed, fail_update=False):
        def command(args, **kwargs):
            if args[:2] == ["dpkg", "--print-architecture"]:
                return subprocess.CompletedProcess(args, 0, "amd64\n")
            if args[0] == "dpkg-query":
                return subprocess.CompletedProcess(args, 0, "install ok installed" if installed[0] else "")
            if "update" in args and fail_update:
                raise subprocess.CalledProcessError(124, args)
            if "install" in args:
                installed[0] = True
            return subprocess.CompletedProcess(args, 0)
        return Mock(side_effect=command)

    def test_existing_packages_avoid_network_but_later_browser_setup_uses_bounded_mirrors(self):
        run = self.runner([True])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "apt-mirrors.txt").write_text("http://azure.archive.ubuntu.com/ubuntu/\n")
            install(["protobuf-compiler"], HOSTED, {"ID": "ubuntu"}, root, run)
        self.assertFalse(any("apt-get" in call.args[0] for call in run.call_args_list))
        writes = {call.args[0][-1]: call.kwargs["input"] for call in run.call_args_list if "tee" in call.args[0]}
        self.assertEqual(writes[str(root / "apt-mirrors.txt")], "https://archive.ubuntu.com/ubuntu/\n")
        self.assertIn('Acquire::https::Timeout "15"', writes[str(root / "apt.conf.d/99schemaforge-ci")])

    def test_missing_packages_are_installed_and_a_timeout_never_continues_to_build(self):
        for failure in (False, True):
            run = self.runner([False], fail_update=failure)
            with tempfile.TemporaryDirectory() as directory:
                if failure:
                    with self.assertRaises(subprocess.CalledProcessError):
                        install(["protobuf-compiler"], HOSTED, {"ID": "ubuntu"}, Path(directory), run)
                else:
                    install(["protobuf-compiler"], HOSTED, {"ID": "ubuntu"}, Path(directory), run)
            apt = [call.args[0] for call in run.call_args_list if "apt-get" in call.args[0]]
            self.assertEqual(len(apt), 1 if failure else 2)
            self.assertIn("120s", apt[0])
            self.assertIn("--error-on=any", apt[0])
            if not failure:
                self.assertIn("180s", apt[1])
                self.assertEqual(apt[1][-1], "protobuf-compiler")

    def test_arm_releases_use_the_ports_archive(self):
        self.assertEqual(ubuntu_mirror("arm64"), "https://ports.ubuntu.com/ubuntu-ports/")
        with self.assertRaises(ValueError):
            ubuntu_mirror("unknown")


if __name__ == "__main__":
    unittest.main()
