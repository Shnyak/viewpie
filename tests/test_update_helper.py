"""Regression tests for the privileged self-update helper."""
import importlib.util
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
        self.assertTrue(any("installer output" in line for line in lines))

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

    def test_only_missing_runtime_packages_are_requested(self):
        with patch.object(helper, "package_is_installed", side_effect=lambda name: name == "ffmpeg"):
            self.assertEqual(helper.missing_runtime_packages(), ["fonts-dejavu-core"])


if __name__ == "__main__":
    unittest.main()
