# n0-dns-resolver iOS privacy boundary patch

Source: the published crates.io `n0-dns-resolver` **0.1.0** archive:
https://static.crates.io/crates/n0-dns-resolver/n0-dns-resolver-0.1.0.crate

Archive SHA-256: `535c99c9a786bee714155d2ee4fc2477405c2c43536e1bc6a51de09a0d1448f7`.
The published package records repository `https://github.com/n0-computer/iroh` and
VCS revision `16a791ce8cfd6456518970872ba3251b0756112d` in `.cargo_vcs_info.json`.
The original package metadata, README, `LICENSE-MIT` and `LICENSE-APACHE` are retained.
License: **MIT OR Apache-2.0**; copyright 2025 N0, INC.

Only `src/system_config/hosts.rs` changes behavior: on `target_os = "ios"`,
`hosts_path()` returns `None`, so `Hosts::from_system()` never opens `/etc/hosts`.
The Apple SystemConfiguration DNS reader and its normal/fallback configuration
remain unchanged. Other Unix targets retain `/etc/hosts`; the Windows hosts-file
path is unchanged. No DNS transport, TLS/crypto provider, pairing or encryption
configuration changes.

The iPhone application's persisted state is under its Application Support
container. File metadata for that local state has the approved privacy reason
`NSPrivacyAccessedAPICategoryFileTimestamp` / `C617.1`; system hosts-file metadata
would fall outside that reason. See Apple's current API/reason list:
https://developer.apple.com/documentation/bundleresources/app-privacy-configuration/nsprivacyaccessedapitypes/nsprivacyaccessedapitype

Validation: `tools/test_ios_privacy.py` compiles the actual target-gated path
functions in a host-run iOS-cfg boundary harness. Native resolver hosts tests and
an `aarch64-apple-ios` controller check validate the normal build paths. These
checks do not establish phone runtime or App Store Connect acceptance; re-scan
the final Release IPA and confirm its bundled manifest in CI. Remove the patch
when an upstream iOS fix has been verified.
