//! Approver keys this host accepted: public keys of people who may allow waiting calls from a paired
//! device (see [`ar_core::approval`]). Each was confirmed by someone at this host, in a prompt or
//! with `ai-remote approvers add` in its own terminal. Stored in `approvers.json` next to the host
//! configuration; forgetting a device removes its keys.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Result, bail};
use ar_core::{
    approval::{self, KeyAlg},
    store, unix_now,
};
use data_encoding::BASE64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproverKey {
    /// The device the key belongs to (`eid:<hex>`); only that device may use it.
    pub principal: String,
    /// The device's name on this host when the key was added.
    pub device: String,
    pub alg: KeyAlg,
    /// Base64 of the DER SubjectPublicKeyInfo.
    pub key: String,
    /// What the device said the key is ("Windows Hello on home-laptop").
    pub label: String,
    /// Unix seconds.
    pub added: u64,
}

impl ApproverKey {
    pub fn new(principal: &str, device: &str, alg: KeyAlg, key_der: &[u8], label: &str) -> Result<Self> {
        approval::check_key(alg, key_der)?;
        let label: String = label.chars().filter(|c| !c.is_control()).take(80).collect();
        Ok(Self {
            principal: principal.to_string(),
            device: device.to_string(),
            alg,
            key: BASE64.encode(key_der),
            label,
            added: unix_now(),
        })
    }

    /// Short fingerprint for people comparing keys: the first 8 bytes of the key's BLAKE3, in hex.
    pub fn fingerprint(&self) -> String {
        fingerprint(self.key.as_bytes())
    }
}

/// Fingerprint of a base64 key as shown by `ai-remote approver` on the controller.
pub fn fingerprint(key_b64: &[u8]) -> String {
    let der = BASE64.decode(key_b64).unwrap_or_default();
    blake3::hash(&der).to_hex()[..16].to_string()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    keys: Vec<ApproverKey>,
}

pub struct ApproverKeys {
    path: PathBuf,
    keys: Mutex<Vec<ApproverKey>>,
}

impl ApproverKeys {
    pub fn open(path: &Path) -> Result<Self> {
        let file: File = store::read_json(path)?;
        Ok(Self { path: path.to_path_buf(), keys: Mutex::new(file.keys) })
    }

    pub fn list(&self) -> Vec<ApproverKey> {
        self.keys.lock().unwrap().clone()
    }

    /// Whether any key could answer a prompt from afar.
    pub fn any(&self) -> bool {
        !self.keys.lock().unwrap().is_empty()
    }

    /// The accepted key of `principal` with this algorithm and DER encoding, if any.
    pub fn find(&self, principal: &str, alg: KeyAlg, key_b64: &str) -> Option<ApproverKey> {
        let der = BASE64.decode(key_b64.as_bytes()).ok()?;
        let key = BASE64.encode(&der);
        self.keys.lock().unwrap().iter().find(|k| k.principal == principal && k.alg == alg && k.key == key).cloned()
    }

    /// Adds a key; `false` when that device already had it.
    pub fn add(&self, key: ApproverKey) -> Result<bool> {
        let mut keys = self.keys.lock().unwrap();
        if keys.iter().any(|k| k.principal == key.principal && k.key == key.key) {
            return Ok(false);
        }
        if keys.iter().filter(|k| k.principal == key.principal).count() >= 8 {
            bail!("device {} already has 8 approver keys; remove some first", key.device);
        }
        keys.push(key);
        self.save(&keys)?;
        Ok(true)
    }

    /// Removes every key of a device, or only the one with this fingerprint. Returns how many.
    pub fn remove(&self, principal: &str, fingerprint: Option<&str>) -> Result<usize> {
        let mut keys = self.keys.lock().unwrap();
        let before = keys.len();
        keys.retain(|k| k.principal != principal || fingerprint.is_some_and(|f| !k.fingerprint().starts_with(f)));
        let removed = before - keys.len();
        if removed > 0 {
            self.save(&keys)?;
        }
        Ok(removed)
    }

    fn save(&self, keys: &[ApproverKey]) -> Result<()> {
        store::write_json_atomic(&self.path, &File { keys: keys.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p256_key() -> Vec<u8> {
        use ring::{
            rand::SystemRandom,
            signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair},
        };
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        approval::p256_spki(pair.public_key().as_ref()).unwrap()
    }

    #[test]
    fn keys_are_stored_per_device_and_removed_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvers.json");
        let store = ApproverKeys::open(&path).unwrap();
        assert!(!store.any());
        let (a, b) = (p256_key(), p256_key());
        let ka = ApproverKey::new("eid:aa", "laptop", KeyAlg::EcdsaP256Sha256, &a, "test\nkey").unwrap();
        assert_eq!(ka.label, "testkey");
        assert!(store.add(ka.clone()).unwrap());
        assert!(!store.add(ka.clone()).unwrap(), "the same key twice is one key");
        store.add(ApproverKey::new("eid:bb", "phone", KeyAlg::EcdsaP256Sha256, &b, "").unwrap()).unwrap();

        let reopened = ApproverKeys::open(&path).unwrap();
        assert!(reopened.find("eid:aa", KeyAlg::EcdsaP256Sha256, &BASE64.encode(&a)).is_some());
        assert!(reopened.find("eid:bb", KeyAlg::EcdsaP256Sha256, &BASE64.encode(&a)).is_none(), "keys are per device");
        assert!(reopened.find("eid:aa", KeyAlg::RsaPkcs1Sha256, &BASE64.encode(&a)).is_none());

        assert_eq!(reopened.remove("eid:aa", Some("0000")).unwrap(), 0, "fingerprint must match");
        assert_eq!(reopened.remove("eid:aa", Some(&ka.fingerprint()[..6])).unwrap(), 1);
        assert_eq!(reopened.remove("eid:bb", None).unwrap(), 1);
        assert!(!ApproverKeys::open(&path).unwrap().any());
    }

    #[test]
    fn keys_of_the_wrong_kind_are_refused() {
        assert!(ApproverKey::new("eid:aa", "x", KeyAlg::RsaPkcs1Sha256, &p256_key(), "").is_err());
    }
}
