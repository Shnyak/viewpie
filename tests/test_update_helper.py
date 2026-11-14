"""Regression tests for the privileged self-update helper."""
import ast
import importlib.util
import inspect
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "viewpie_update_helper", ROOT / "debian" / "viewpie-update-helper.py"
)
helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helper)


class UpdateHelperTests(unittest.TestCase):
    def test_run_logged_streams_output_and_returns_success(self):
        lines = []
        with patch.object(helper, "event"), patch.object(helper, "log", side_effect=lines.append):
            result = helper.run_logged(
                [sys.executable, "-c", "print('installer output')"],
                timeout_seconds=5,
                phase="test",
            )
        self.assertEqual(result, 0)
        self.assertIn("installer output", lines)

    def test_run_logged_enforces_deadline_while_process_is_running(self):
        started = time.monotonic()
        with patch.object(helper, "event"), patch.object(helper, "log"):
            with self.assertRaises(TimeoutError):
                helper.run_logged(
                    [sys.executable, "-c", "import time; time.sleep(5)"],
                    timeout_seconds=0.2,
                    phase="test-timeout",
                )
        self.assertLess(time.monotonic() - started, 3)

    def test_run_logged_uses_noninteractive_package_environment(self):
        original = helper.subprocess.Popen
        captured = {}

        def capture(command, **kwargs):
            captured.update(kwargs.get("env", {}))
            return original(command, **kwargs)

        with patch.object(helper, "event"), patch.object(helper, "log"), patch.object(helper.subprocess, "Popen", side_effect=capture):
            helper.run_logged(
                [sys.executable, "-c", "pass"],
                timeout_seconds=5,
                phase="test-environment",
            )
        self.assertEqual(captured["DEBIAN_FRONTEND"], "noninteractive")
        self.assertEqual(captured["APT_LISTCHANGES_FRONTEND"], "none")

    def test_local_package_install_pins_ffmpeg_but_allows_viewpie_upgrade(self):
        command = helper.package_install_command(
            Path("/tmp/viewpie-new.deb"), "7:5.1.8-0+deb12u1+rpt1"
        )
        self.assertIn("/tmp/viewpie-new.deb", command)
        self.assertEqual(command[-1], "ffmpeg=7:5.1.8-0+deb12u1+rpt1")
        self.assertNotIn("--no-upgrade", command)

    def test_install_latest_release_does_not_shadow_package_version_helper(self):
        tree = ast.parse(inspect.getsource(helper.install_latest_release))
        assigned_names = {
            node.id
            for node in ast.walk(tree)
            if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store)
        }
        helper_names = {
            node.name for node in ast.walk(ast.parse(inspect.getsource(helper)))
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        }
        self.assertTrue(helper_names.isdisjoint(assigned_names),
                        "install_latest_release shadows a module-level helper")

    def test_only_missing_runtime_packages_are_requested(self):
        with patch.object(helper, "is_package_installed", side_effect=lambda name: name == "ffmpeg"):
            self.assertEqual(helper.missing_runtime_packages(), ["fonts-dejavu-core"])


if __name__ == "__main__":
    unittest.main()
