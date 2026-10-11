"""Minimal iOS privacy manifest and actual vendored target-gate boundary checks."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor/n0-dns-resolver"
RUSTC = shutil.which("rustc") or str(Path.home() / ".cargo/bin/rustc")


class IOSPrivacyTests(unittest.TestCase):
    def test_manifest_declares_only_proven_container_file_metadata(self):
        with (ROOT / "apps/ios/App/PrivacyInfo.xcprivacy").open("rb") as file:
            manifest = plistlib.load(file)
        self.assertEqual(manifest, {"NSPrivacyAccessedAPITypes": [{
            "NSPrivacyAccessedAPIType": "NSPrivacyAccessedAPICategoryFileTimestamp",
            "NSPrivacyAccessedAPITypeReasons": ["C617.1"],
        }]})

    def test_manifest_is_explicitly_in_the_application_resource_phase(self):
        project = (ROOT / "apps/ios/project.yml").read_text()
        self.assertRegex(project, r"path: App\n\s+excludes: \[PrivacyInfo\.xcprivacy\]")
        self.assertRegex(project, r"path: App/PrivacyInfo\.xcprivacy\n\s+buildPhase: resources")

    def test_upstream_identity_and_licenses_are_retained(self):
        provenance = json.loads((VENDOR / ".cargo_vcs_info.json").read_text())
        self.assertEqual(provenance["git"]["sha1"], "16a791ce8cfd6456518970872ba3251b0756112d")
        package = (VENDOR / "Cargo.toml").read_text()
        self.assertIn('name = "n0-dns-resolver"', package)
        self.assertIn('version = "0.1.0"', package)
        self.assertIn('license = "MIT OR Apache-2.0"', package)
        self.assertTrue((VENDOR / "LICENSE-MIT").is_file())
        self.assertTrue((VENDOR / "LICENSE-APACHE").is_file())
        self.assertIn("535c99c9a786bee714155d2ee4fc2477405c2c43536e1bc6a51de09a0d1448f7", (VENDOR / "README.ai-remote.md").read_text())

    def test_apple_dns_and_other_platform_readers_are_exact_upstream_sources(self):
        expected = {
            "apple.rs": "3f8cb2d12e4bca23fb2e972eb900902f89ff726dae8ce31469a50f13e8343566",
            "windows.rs": "68bd41bb9c54eaa63ca588bf91b48593c43070f3b341ccc83b53ed5f8b097523",
            "unix.rs": "9e596e4d8894af41d22823395418894106c2e4234fcbed018f9d50258b1c6349",
        }
        for name, digest in expected.items():
            self.assertEqual(hashlib.sha256((VENDOR / "src/system_config" / name).read_bytes()).hexdigest(), digest, name)
        self.assertEqual(hashlib.sha256((VENDOR / "src/system_config.rs").read_bytes()).hexdigest(),
                         "96122945c938f461b30d6c29145e91876639be84742aa69aa1ab2d9480c22454")

    @unittest.skipUnless(Path(RUSTC).is_file(), "rustc unavailable")
    def test_native_non_ios_hosts_path_is_unchanged(self):
        self.compile_hosts_gate(ios=False)

    @unittest.skipUnless(Path(RUSTC).is_file() and os.name != "nt", "host-run iOS cfg fixture requires Unix rustc")
    def test_ios_target_gate_returns_no_system_file(self):
        # The real production cfg-gated functions are compiled, not a rewritten policy.
        # This executes a native fixture with the iOS cfg enabled; it is not a device test.
        self.compile_hosts_gate(ios=True)

    @unittest.skipUnless(Path(RUSTC).is_file() and os.name != "nt", "host-run iOS cfg fixture requires Unix rustc")
    def test_ios_selects_apple_dns_instead_of_generic_resolv_conf(self):
        source = (VENDOR / "src/system_config.rs").read_text()
        start = source.index('#[cfg(target_os = "android")]\nuse android::read_system_dns;')
        end = source.index("/// Reads the host system's DNS configuration", start)
        selected_imports = source[start:end]
        modules = '\n'.join(f'mod {name} {{ pub fn read_system_dns() -> &' + "'static str" + f' {{ "{name}" }} }}'
                            for name in ["android", "apple", "unix", "windows"])
        modules += "\nmod hosts { pub struct Hosts; }\n"
        with tempfile.TemporaryDirectory(prefix="ar-privacy-dns-") as temporary:
            path = Path(temporary)
            rust = path / "reader_gate.rs"
            executable = path / "reader_gate"
            rust.write_text(modules + selected_imports + '\nfn main() { assert_eq!(read_system_dns(), "apple"); }')
            subprocess.run([RUSTC, "--edition", "2024", str(rust), "-o", str(executable),
                            "--cfg", 'target_os="ios"', "--cfg", 'target_vendor="apple"',
                            "-Aexplicit_builtin_cfgs_in_flags"], check=True, capture_output=True, text=True)
            subprocess.run([str(executable)], check=True, capture_output=True, text=True)

    def compile_hosts_gate(self, *, ios):
        source = (VENDOR / "src/system_config/hosts.rs").read_text()
        start = source.index('#[cfg(target_os = "ios")]\nfn hosts_path()')
        end = source.index("#[cfg(test)]", start)
        functions = source[start:end]
        if ios:
            main = "fn main() { assert_eq!(hosts_path(), None); }"
        elif os.name == "nt":
            main = 'fn main() { let root = std::env::var_os("SystemRoot").unwrap(); assert_eq!(hosts_path(), Some(std::path::Path::new(&root).join("System32\\\\drivers\\\\etc\\\\hosts"))); }'
        else:
            main = 'fn main() { assert_eq!(hosts_path(), Some(std::path::PathBuf::from("/etc/hosts"))); }'
        with tempfile.TemporaryDirectory(prefix="ar-privacy-gate-") as temporary:
            path = Path(temporary)
            rust = path / "hosts_gate.rs"
            executable = path / ("hosts_gate.exe" if os.name == "nt" else "hosts_gate")
            rust.write_text(functions + "\n" + main)
            args = [RUSTC, "--edition", "2024", str(rust), "-o", str(executable)]
            if ios:
                args += ["--cfg", 'target_os="ios"', "-Aexplicit_builtin_cfgs_in_flags"]
            subprocess.run(args, check=True, capture_output=True, text=True)
            subprocess.run([str(executable)], check=True, capture_output=True, text=True)


if __name__ == "__main__":
    unittest.main()
