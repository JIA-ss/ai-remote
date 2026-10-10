//! The gated per-scope app-blob provider. A SEPARATE transport from
//! the roster blob (`roster::transport::RosterBlobs`, MemStore, `iroh_blobs::ALPN`, ungated): an
//! `FsStore` at `<data_dir>/blobs/` advertised on `APP_BLOB_ALPN`, built with
//! `BlobsProtocol::new(&fsstore, Some(events))` so a request-time Intercept hook (`provider`) can
//! serve a hash ONLY to callers a scope grants (`scope`). Hashes are integrity proofs, never
//! capabilities: authorization is a separate scope-membership decision keyed on the AUTHENTICATED
//! endpoint id resolved via the daemon's trust gate.
pub mod provider;
pub mod scope;

/// The app-blob ALPN (the named `mcpmesh/blob/1` protocol), DISTINCT from `iroh_blobs::ALPN`
/// (which the untouched roster provider AND the iroh-blobs `Downloader` are pinned to). The daemon
/// accept loop dispatches this ALPN to the gated `AppBlobs` provider; the caller-side fetch opens a
/// connection on it and uses `store.remote().fetch` (the Downloader cannot — it hardcodes
/// `iroh_blobs::ALPN`).
pub const APP_BLOB_ALPN: &[u8] = b"mcpmesh/blob/1";

/// Parse a blake3 hash from a caller-supplied string WITHOUT panicking.
///
/// `iroh_blobs::Hash: FromStr` decodes into a fixed 32-byte buffer via `data-encoding`'s
/// `decode_mut`, which opens with `assert_eq!(Ok(output.len()), self.decode_len(input.len()))`.
/// The base32 branch has no length guard, so **any** input that is neither 64 chars nor 52 chars
/// panics rather than returning `Err` — `""`, `"nope"`, or a 64-hex string with a trailing newline
/// all abort the calling task. On the control socket that unwinds the per-connection handler and
/// the client gets EOF with no JSON-RPC response at all, instead of the error the docs promise.
///
/// Guarding the LENGTH is sufficient: at 64 (hex) and 52 (base32) `decode_len` matches the buffer,
/// so invalid symbols return `Err` normally. The charset check is belt-and-braces.
pub(crate) fn parse_blob_hash(s: &str) -> anyhow::Result<iroh_blobs::Hash> {
    let plausible = (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        || (s.len() == 52 && s.bytes().all(|b| b.is_ascii_alphanumeric()));
    if !plausible {
        anyhow::bail!("not a blake3 hash: {s:?} (expected 64 hex chars or 52 base32 chars)");
    }
    s.parse::<iroh_blobs::Hash>()
        .map_err(|_| anyhow::anyhow!("not a blake3 hash: {s:?}"))
}

#[cfg(test)]
mod hash_parse_tests {
    /// #83 review: these inputs PANICKED before the length guard, killing the control connection.
    #[test]
    fn malformed_hashes_error_instead_of_panicking() {
        for bad in [
            "",
            "nope",
            "not-a-hash",
            "abc",
            &format!("{}\n", "aa".repeat(32)),
        ] {
            assert!(
                super::parse_blob_hash(bad).is_err(),
                "{bad:?} must be a clean error, not a panic"
            );
        }
        // Valid forms still parse, and both renderings of the SAME hash agree.
        let hex = "aa".repeat(32);
        let parsed = super::parse_blob_hash(&hex).expect("64-hex parses");
        assert_eq!(parsed.to_hex().to_string(), hex);
    }
}
