"""Read-only build plan checks for Windows packaging: the installer carries the command and the
background host next to the app, which installs the host from there (no terminal)."""
from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
import unittest


ROOT = Path(__file__).resolve().parent.parent
PWSH = shutil.which("pwsh")


class WindowsPackagingPlanTests(unittest.TestCase):
    def test_the_bundle_carries_the_command_and_the_host(self):
        bundle = json.loads((ROOT / "apps/desktop/src-tauri/tauri.windows.bundle.conf.json").read_text())
        self.assertEqual(bundle["bundle"]["externalBin"], ["binaries/ai-remote", "binaries/ai-remote-hostw"])

    @unittest.skipUnless(PWSH, "pwsh unavailable")
    def test_dry_run_prints_the_plan_and_runs_nothing(self):
        result = subprocess.run(
            [PWSH, "-NoProfile", "-File", str(ROOT / "tools/windows.ps1"), "build", "-DryRun", "--debug"],
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("(debug)", result.stdout)
        self.assertIn("cargo build -p ai-remote", result.stdout)
        self.assertNotIn("--release", result.stdout)
        self.assertIn("tauri.windows.bundle.conf.json --debug", result.stdout)


if __name__ == "__main__":
    unittest.main()
