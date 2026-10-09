//! Remote approvals: the message a person signs to allow one waiting call, and the host's check.
//!
//! An approver key is created on a controller device in the OS key store behind a presence check
//! (Windows Hello, Touch ID, Face ID), so a signature needs the person at that device, not just the
//! device's files or an agent running there. A host accepts a key only after someone at the host
//! confirms it, and allows a waiting call from afar only with a signature over [`allow_message`].
//! Denying needs no signature: it can only make things safer.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use spki::{ObjectIdentifier, SubjectPublicKeyInfoRef, der::Decode};

/// Signature schemes of approver keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyAlg {
    /// ECDSA over P-256 with SHA-256, DER-encoded signature (Secure Enclave keys).
    #[serde(rename = "ecdsa-p256-sha256")]
    EcdsaP256Sha256,
    /// RSASSA-PKCS1-v1_5 with SHA-256 (Windows Hello key credentials).
    #[serde(rename = "rsa-pkcs1-sha256")]
    RsaPkcs1Sha256,
}

impl KeyAlg {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyAlg::EcdsaP256Sha256 => "ecdsa-p256-sha256",
            KeyAlg::RsaPkcs1Sha256 => "rsa-pkcs1-sha256",
        }
    }
}

/// What a waiting call is, as far as a signature is concerned. The host publishes these fields with
/// every approval request; the signer takes `host` from its own pairing record, not from the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowTarget {
    /// The host's device principal (`eid:<hex>`).
    pub host: String,
    /// The approval's id (random, chosen by the host).
    pub id: String,
    pub tool: String,
    /// BLAKE3 of the call's canonical argument JSON.
    pub args_hash: String,
    /// When the host stops waiting, in milliseconds since the Unix epoch.
    pub expires_ms: u64,
}

/// The bytes signed to allow exactly one call: the host, the approval, the tool and the arguments
/// the person saw. A signature for one call is worthless for any other.
pub fn allow_message(t: &AllowTarget) -> Result<Vec<u8>> {
    for (what, v) in [("host", &t.host), ("id", &t.id), ("tool", &t.tool), ("args hash", &t.args_hash)] {
        if v.is_empty() || v.chars().any(char::is_control) {
            bail!("approval {what} is empty or has control characters");
        }
    }
    Ok(format!(
        "ai-remote approval v1\nallow\nhost {}\nid {}\ntool {}\nargs {}\nexpires {}\n",
        t.host, t.id, t.tool, t.args_hash, t.expires_ms
    )
    .into_bytes())
}

const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const PRIME256V1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");

/// Checks that `spki_der` (an X.509 SubjectPublicKeyInfo) is a key of scheme `alg`.
pub fn check_key(alg: KeyAlg, spki_der: &[u8]) -> Result<()> {
    key_bytes(alg, spki_der).map(|_| ())
}

/// Verifies `signature` over `message` with the approver key `spki_der`.
pub fn verify(alg: KeyAlg, spki_der: &[u8], message: &[u8], signature: &[u8]) -> Result<()> {
    use ring::signature::{ECDSA_P256_SHA256_ASN1, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
    let key = key_bytes(alg, spki_der)?;
    let checked = match alg {
        KeyAlg::EcdsaP256Sha256 => UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key).verify(message, signature),
        KeyAlg::RsaPkcs1Sha256 => UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key).verify(message, signature),
    };
    checked.map_err(|_| anyhow::anyhow!("the approval signature does not match"))
}

/// The key inside a SubjectPublicKeyInfo: the uncompressed point for P-256, the PKCS#1 key for RSA.
fn key_bytes(alg: KeyAlg, spki_der: &[u8]) -> Result<&[u8]> {
    let info = SubjectPublicKeyInfoRef::from_der(spki_der).context("the approver key is not a public key")?;
    let ok = match alg {
        KeyAlg::RsaPkcs1Sha256 => info.algorithm.oid == RSA_ENCRYPTION,
        KeyAlg::EcdsaP256Sha256 => {
            info.algorithm.oid == EC_PUBLIC_KEY
                && info.algorithm.parameters.and_then(|p| p.decode_as::<ObjectIdentifier>().ok()) == Some(PRIME256V1)
        }
    };
    if !ok {
        bail!("the approver key is not a {} key", alg.as_str());
    }
    info.subject_public_key.as_bytes().context("the approver key has a partial byte")
}

/// The DER SubjectPublicKeyInfo of a P-256 key given as its uncompressed point (`04 || x || y`), the
/// form the Secure Enclave and ring export.
pub fn p256_spki(point: &[u8]) -> Result<Vec<u8>> {
    if point.len() != 65 || point[0] != 4 {
        bail!("not an uncompressed P-256 point");
    }
    const PREFIX: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48,
        0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(point);
    Ok(der)
}

#[cfg(test)]
mod tests {
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair},
    };

    use super::*;

    fn target() -> AllowTarget {
        AllowTarget {
            host: "eid:abc".into(),
            id: "0123abcd".into(),
            tool: "shell".into(),
            args_hash: "ff00".into(),
            expires_ms: 1_700_000_000_000,
        }
    }

    fn key() -> (EcdsaKeyPair, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let spki = p256_spki(pair.public_key().as_ref()).unwrap();
        (pair, spki)
    }

    #[test]
    fn a_signature_allows_exactly_the_call_it_names() {
        let (pair, spki) = key();
        let msg = allow_message(&target()).unwrap();
        let sig = pair.sign(&SystemRandom::new(), &msg).unwrap();
        verify(KeyAlg::EcdsaP256Sha256, &spki, &msg, sig.as_ref()).unwrap();

        for other in [
            AllowTarget { host: "eid:other".into(), ..target() },
            AllowTarget { id: "0123abce".into(), ..target() },
            AllowTarget { tool: "shell_x".into(), ..target() },
            AllowTarget { args_hash: "ff01".into(), ..target() },
            AllowTarget { expires_ms: 1, ..target() },
        ] {
            let msg = allow_message(&other).unwrap();
            assert!(verify(KeyAlg::EcdsaP256Sha256, &spki, &msg, sig.as_ref()).is_err());
        }
    }

    /// A Windows Hello key signs with RSASSA-PKCS1-v1_5 / SHA-256; this vector was made with OpenSSL.
    #[test]
    fn rsa_pkcs1_signatures_verify() {
        const RSA_PUB: &str = concat!(
            "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAxwhN/03wJ0g0SCV5cAMuE/Xn8kU/Ov4wZs3xudXtISTst1MuZCnG",
            "4DMVr+FAbrRUydYoNyC5nGHKXnBoK4YxGM6qCn34sZ65cXodW8Bth0tgjQpfb155LH7PIpiVUzyx5MEgGWEYEKbllYGyO89X",
            "QMlhugxbWUv/3M+3KBX1l2MpQJQmHRVUjgzsKz2uBTKXz/2k6iPSGtgMghxH/jDtlfUX34IFoezMoXTLR9Yo8B365FvUdZpG",
            "q6tNkrG9ojaoWxK4YVqWUu0VxHCSzhdVw/Qssrn6D8igRWubG8OM4a/Qo5w2V+0/x6HS2iw4a2Te6t/yafTWw+tONkqz1KVf",
            "KwIDAQAB",
        );
        const RSA_SIG: &str = concat!(
            "f001qJTaeOZJpFGvGsq3kiXrQ+yfWSoaxdHfe0fv/LM/1wIOwn9/LN6O5z6aZ+hzb4VN9gmwew2aWJr9vKZXVhB8/tfKz2eh",
            "YcStq1mROE9jmZzwFFy4mSb5o/Fewg7LlG8zLJyRO5EyHdaoomwG2ElD+kBe4FG5FpSjpZ9sE2QHflcYlIV7lCIpkmsyRIWl",
            "JNLePJkORo2LXLa+W3vN+cHuqLUCXnkhjyQuvPr5nePztTDl5sZGijuXmV49ARabSD4WGDs6D8C5hZY1lvniMcmAJxHUHTH2",
            "VLtCq4wPfWjol/zXfWFtSyK/VD4kd15KdNKczSYMuN7mIYfmHYktqw==",
        );
        let spki = data_encoding::BASE64.decode(RSA_PUB.as_bytes()).unwrap();
        let sig = data_encoding::BASE64.decode(RSA_SIG.as_bytes()).unwrap();
        check_key(KeyAlg::RsaPkcs1Sha256, &spki).unwrap();
        assert!(check_key(KeyAlg::EcdsaP256Sha256, &spki).is_err());
        let msg = allow_message(&target()).unwrap();
        verify(KeyAlg::RsaPkcs1Sha256, &spki, &msg, &sig).unwrap();
        let other = allow_message(&AllowTarget { tool: "shell2".into(), ..target() }).unwrap();
        assert!(verify(KeyAlg::RsaPkcs1Sha256, &spki, &other, &sig).is_err());
    }

    #[test]
    fn keys_must_match_their_scheme() {
        let (_, spki) = key();
        check_key(KeyAlg::EcdsaP256Sha256, &spki).unwrap();
        assert!(check_key(KeyAlg::RsaPkcs1Sha256, &spki).is_err());
        assert!(check_key(KeyAlg::EcdsaP256Sha256, b"not der").is_err());
    }

    #[test]
    fn fields_with_control_characters_are_refused() {
        assert!(allow_message(&AllowTarget { tool: "shell\nallow".into(), ..target() }).is_err());
        assert!(allow_message(&AllowTarget { id: String::new(), ..target() }).is_err());
    }
}
