"""Read-only build plans and real temporary ad hoc resource-seal checks for macOS packaging."""
from __future__ import annotations

import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
BASH = shutil.which("bash")


class MacPackagingPlanTests(unittest.TestCase):
    def test_platform_defaults_enable_maintained_signing_before_dmg(self):
        config = json.loads((ROOT / "apps/desktop/src-tauri/tauri.macos.conf.json").read_text())
        self.assertEqual(config["bundle"]["macOS"]["signingIdentity"], "-")
        self.assertIn("app", config["bundle"]["targets"])
        self.assertIn("dmg", config["bundle"]["targets"])
        self.assertNotIn("signingRequirements", config["bundle"]["macOS"])
        bundle = json.loads((ROOT / "apps/desktop/src-tauri/tauri.macos.bundle.conf.json").read_text())
        self.assertEqual(bundle["bundle"]["externalBin"], ["binaries/ai-remote", "binaries/ai-remote-hostw"])

    @unittest.skipUnless(BASH, "bash unavailable")
    def test_dry_run_is_portable_and_preserves_debug_bundle_arguments(self):
        env = {**os.environ}
        env.pop("APPLE_SIGNING_IDENTITY", None)
        env["APPLE_CERTIFICATE_PASSWORD"] = "fixture-password-must-not-be-printed"
        # Compilers and bundlers are absent from PATH: dry-run must not invoke them.
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            dirname = shutil.which("dirname")
            if not dirname:
                self.skipTest("dirname unavailable")
            copied_dirname = path / Path(dirname).name
            shutil.copyfile(dirname, copied_dirname)
            copied_dirname.chmod(0o755)
            env["PATH"] = str(path)
            result = subprocess.run([BASH, str(ROOT / "tools/macos.sh"), "build", "--dry-run", "--debug", "--bundles", "app"], env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("(debug)", result.stdout)
        self.assertIn("identity: -", result.stdout)
        self.assertIn("--debug --bundles app", result.stdout)
        self.assertIn("codesign --verify --deep --strict", result.stdout)
        self.assertNotIn("--dry-run ", result.stdout)
        self.assertNotIn(env["APPLE_CERTIFICATE_PASSWORD"], result.stdout + result.stderr)

    @unittest.skipUnless(BASH, "bash unavailable")
    def test_empty_tauri_arguments_work_with_macos_bash(self):
        env = {**os.environ}
        env.pop("APPLE_SIGNING_IDENTITY", None)
        result = subprocess.run([BASH, str(ROOT / "tools/macos.sh"), "dev", "--dry-run"], env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("npm run tauri -- dev", result.stdout)

    @unittest.skipUnless(BASH, "bash unavailable")
    def test_certificate_identity_override_is_retained_and_empty_override_refused(self):
        identity = "Developer ID Application: Fixture Example (ABCDEFGHIJ)"
        result = subprocess.run([BASH, str(ROOT / "tools/macos.sh"), "build", "--dry-run"], env={**os.environ, "APPLE_SIGNING_IDENTITY": identity}, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(identity, result.stdout)
        invalid = subprocess.run([BASH, str(ROOT / "tools/macos.sh"), "build", "--dry-run"], env={**os.environ, "APPLE_SIGNING_IDENTITY": ""}, text=True, capture_output=True)
        self.assertEqual(invalid.returncode, 2)
        self.assertIn("is empty", invalid.stderr)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS codesign")
class MacResourceSealTests(unittest.TestCase):
    def test_complete_ad_hoc_bundle_verifies_and_binds_info_plist(self):
        # Native signing is exercised only in a temporary fixture; no installed app,
        # login agent, privacy setting, or certificate/keychain state is changed.
        with tempfile.TemporaryDirectory() as temporary:
            app = Path(temporary) / "Fixture.app"
            binaries = app / "Contents/MacOS"
            resources = app / "Contents/Resources"
            binaries.mkdir(parents=True)
            resources.mkdir()
            (resources / "fixture.txt").write_text("sealed resource")
            info = {"CFBundleIdentifier": "io.ai-remote.packaging-fixture", "CFBundlePackageType": "APPL", "CFBundleExecutable": "fixture", "CFBundleVersion": "1"}
            info_path = app / "Contents/Info.plist"
            info_path.write_bytes(plistlib.dumps(info))
            for name in ["fixture", "ai-remote", "ai-remote-hostw"]:
                shutil.copyfile("/usr/bin/true", binaries / name)
                (binaries / name).chmod(0o755)
            for name in ["ai-remote", "ai-remote-hostw"]:
                subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(binaries / name)], check=True, capture_output=True)
            subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(app)], check=True, capture_output=True)
            subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], check=True, capture_output=True)
            identity = subprocess.run(["/usr/bin/codesign", "--display", "--verbose=4", str(app)], check=True, capture_output=True, text=True)
            self.assertIn("Signature=adhoc", identity.stderr)
            self.assertIn("Identifier=io.ai-remote.packaging-fixture", identity.stderr)
            info["CFBundleVersion"] = "2"
            info_path.write_bytes(plistlib.dumps(info))
            changed = subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], capture_output=True, text=True)
            self.assertNotEqual(changed.returncode, 0, "Info.plist must be bound by the bundle's complete signature")


if __name__ == "__main__":
    unittest.main()
