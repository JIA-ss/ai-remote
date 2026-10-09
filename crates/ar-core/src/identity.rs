//! Device identity: 32 secret bytes of an Ed25519 key, generated on first run and kept in the
//! profile. We hold the key ourselves (instead of letting the transport mint one) so the device id
//! stays the same if the transport layer is ever swapped.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::store;

/// Loads the secret key bytes at `path`, creating and persisting new random bytes if absent.
pub fn load_or_create(path: &Path) -> Result<[u8; 32]> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_hex(text.trim())
            .with_context(|| format!("{} is not a valid identity key", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = random_bytes()?;
            store::write_atomic(path, data_encoding::HEXLOWER.encode(&key).as_bytes())?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn parse_hex(text: &str) -> Result<[u8; 32]> {
    let bytes = data_encoding::HEXLOWER_PERMISSIVE.decode(text.as_bytes())?;
    let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) else {
        bail!("expected 32 bytes, found {}", bytes.len());
    };
    Ok(key)
}

/// 32 bytes from the operating system's CSPRNG.
fn random_bytes() -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow::anyhow!("operating system RNG failed: {e}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_across_loads() {
        let dir = std::env::temp_dir().join(format!("ar-identity-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identity.key");
        let _ = std::fs::remove_file(&path);
        let a = load_or_create(&path).unwrap();
        let b = load_or_create(&path).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, [0u8; 32]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
