#!/usr/bin/env python3
"""Structural IPA packaging tests. Synthetic Mach-O fixtures are not installable apps."""
import importlib.util
import json
from pathlib import Path
import plistlib
import stat
import struct
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("package_ios_ipa", Path(__file__).with_name("package-ios-ipa.py"))
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


def image(platform=2, cpu=0x0100000C, filetype=2, subtype=0):
    header = struct.pack("<8I", 0xFEEDFACF, cpu, subtype, filetype, 1, 24, 0, 0)
    return header + struct.pack("<6I", 0x32, 24, platform, 17 << 16, 18 << 16, 0)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.app = self.root / "Fixture.app"
        self.app.mkdir()
        (self.app / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleSupportedPlatforms": ["iPhoneOS"],
            "CFBundleExecutable": "Fixture",
            "MinimumOSVersion": "17.0",
        }))
        self.main = self.app / "Fixture"
        self.main.write_bytes(image())
        self.main.chmod(0o755)

    def tearDown(self):
        self.temporary.cleanup()

    def test_device_payload_layout_and_framework_symlinks(self):
        framework = self.app / "Frameworks/Fixture.framework"
        version = framework / "Versions/A"
        version.mkdir(parents=True)
        (version / "Fixture").write_bytes(image(filetype=6))
        (framework / "Versions/Current").symlink_to("A", target_is_directory=True)
        (framework / "Fixture").symlink_to("Versions/Current/Fixture")
        (self.app / "asset.txt").write_text("fixture")
        output = self.root / "Fixture-unsigned.ipa"
        metadata = packager.package(self.app, output)
        self.assertNotIn("unsigned", metadata)  # No unverified cryptographic signature assertion.
        self.assertEqual(metadata["signing"], "local-signing-required")
        self.assertFalse(metadata["embedded_provisioning"])
        self.assertTrue(metadata["requires_local_signing"])
        self.assertEqual(metadata["mach_o_images"], 2)
        with zipfile.ZipFile(output) as zipped:
            self.assertIn("Payload/Fixture.app/Info.plist", zipped.namelist())
            self.assertEqual(zipped.read("Payload/Fixture.app/asset.txt"), b"fixture")
            link = zipped.getinfo("Payload/Fixture.app/Frameworks/Fixture.framework/Versions/Current")
            self.assertTrue(stat.S_ISLNK(link.external_attr >> 16))
            self.assertEqual(zipped.read(link), b"A")
            self.assertTrue(zipped.getinfo("Payload/Fixture.app/Fixture").external_attr >> 16 & 0o111)

    def test_simulator_macho_is_refused_even_with_device_plist(self):
        self.main.write_bytes(image(platform=7))
        with self.assertRaisesRegex(packager.PackageError, "not a verified iOS device"):
            packager.package(self.app, self.root / "bad.ipa")
        self.assertFalse((self.root / "bad.ipa").exists())

    def test_simulator_plist_is_refused(self):
        (self.app / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleSupportedPlatforms": ["iPhoneSimulator"], "CFBundleExecutable": "Fixture"
        }))
        with self.assertRaisesRegex(packager.PackageError, "simulator"):
            packager.package(self.app, self.root / "bad.ipa")

    def test_embedded_simulator_and_catalyst_images_are_refused(self):
        frameworks = self.app / "Frameworks"
        frameworks.mkdir()
        for platform in [7, 6]:
            (frameworks / "bad.dylib").write_bytes(image(platform=platform, filetype=6))
            with self.assertRaisesRegex(packager.PackageError, "not a verified iOS device"):
                packager.package(self.app, self.root / "bad.ipa")

    def test_non_arm64_main_is_refused(self):
        self.main.write_bytes(image(cpu=0x01000007))
        with self.assertRaisesRegex(packager.PackageError, "non-arm64"):
            packager.package(self.app, self.root / "bad.ipa")

    def test_arm64e_is_not_reported_as_arm64(self):
        self.main.write_bytes(image(subtype=2))
        with self.assertRaisesRegex(packager.PackageError, "arm64e"):
            packager.package(self.app, self.root / "bad.ipa")

    def test_escaping_and_broken_links_are_refused(self):
        outside = self.root / "outside"
        outside.write_text("not app content")
        link = self.app / "escape"
        link.symlink_to("../outside")
        with self.assertRaisesRegex(packager.PackageError, "escaping"):
            packager.package(self.app, self.root / "bad.ipa")
        link.unlink()
        link.symlink_to("missing")
        with self.assertRaisesRegex(packager.PackageError, "broken"):
            packager.package(self.app, self.root / "bad.ipa")

    def test_private_provisioning_profile_is_refused(self):
        (self.app / "embedded.mobileprovision").write_bytes(b"fixture")
        with self.assertRaisesRegex(packager.PackageError, "embedded.mobileprovision"):
            packager.package(self.app, self.root / "bad.ipa")

    def test_main_filetype_and_malformed_commands_are_refused(self):
        self.main.write_bytes(image(filetype=6))
        with self.assertRaisesRegex(packager.PackageError, "must be a Mach-O executable"):
            packager.package(self.app, self.root / "bad.ipa")
        self.main.write_bytes(image()[:-1])
        with self.assertRaisesRegex(packager.PackageError, "load-command bounds"):
            packager.package(self.app, self.root / "bad.ipa")


if __name__ == "__main__":
    unittest.main()
