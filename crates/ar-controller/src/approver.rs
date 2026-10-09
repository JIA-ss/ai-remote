//! This device's approver key: what lets a person allow a waiting call on a host from here.
//!
//! The key is made and kept by the OS behind a presence check, so signing needs the person, not
//! just this user account: on Windows a Windows Hello key credential (PIN, fingerprint or face for
//! every signature). The private key never leaves the OS. An agent running on this computer can ask
//! for a signature, but only the person can complete the check, and the OS dialog appears on their
//! screen. macOS (Secure Enclave with Touch ID) comes with the macOS controller; until then a Mac can
//! deny but not allow.
//!
//! Builds with the `software-approver` feature also accept a plain key file when
//! `AI_REMOTE_SOFTWARE_APPROVER=1`, for automated tests only: it has no presence check, so release
//! builds do not contain it.

use anyhow::Result;
use ar_core::{
    Profile,
    approval::{AllowTarget, KeyAlg, allow_message},
    machine_name,
};
use data_encoding::BASE64;

/// The public half of this device's approver key.
#[derive(Debug, Clone)]
pub struct ApproverKey {
    pub alg: KeyAlg,
    /// DER SubjectPublicKeyInfo.
    pub public_key: Vec<u8>,
    /// What the host shows when asked to accept it ("Windows Hello on home-laptop").
    pub label: String,
    backend: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    #[cfg(windows)]
    Hello(String),
    #[cfg(feature = "software-approver")]
    Software(std::path::PathBuf),
}

impl ApproverKey {
    pub fn public_key_b64(&self) -> String {
        BASE64.encode(&self.public_key)
    }

    /// The fingerprint hosts show for this key (first 8 bytes of its BLAKE3, in hex).
    pub fn fingerprint(&self) -> String {
        blake3::hash(&self.public_key).to_hex()[..16].to_string()
    }

    /// Signs the allow message for `target`. Blocks until the person passes (or fails) the OS check;
    /// call it off the async runtime.
    pub fn sign_allow(&self, target: &AllowTarget) -> Result<Vec<u8>> {
        let message = allow_message(target)?;
        match &self.backend {
            #[cfg(windows)]
            Backend::Hello(name) => hello::sign(name, &message),
            #[cfg(feature = "software-approver")]
            Backend::Software(path) => software::sign(path, &message),
        }
    }
}

/// This device's approver key, if it has one. Never prompts.
pub fn existing(profile: &Profile) -> Result<Option<ApproverKey>> {
    #[cfg(feature = "software-approver")]
    if software::enabled() {
        return software::open(profile, false);
    }
    #[cfg(windows)]
    {
        hello::open(&hello::name(profile))
    }
    #[cfg(not(windows))]
    {
        let _ = profile;
        Ok(None)
    }
}

/// This device's approver key, created when missing (the OS asks the person to confirm).
pub fn get_or_create(profile: &Profile) -> Result<ApproverKey> {
    #[cfg(feature = "software-approver")]
    if software::enabled() {
        return software::open(profile, true)?.ok_or_else(|| anyhow::anyhow!("no software approver key"));
    }
    #[cfg(windows)]
    {
        let name = hello::name(profile);
        if let Some(key) = hello::open(&name)? {
            return Ok(key);
        }
        hello::create(&name)
    }
    #[cfg(not(windows))]
    {
        let _ = profile;
        anyhow::bail!(
            "Allowing from this device needs Touch ID support, which comes with the macOS controller. You can deny              from here; allow at the host or on another device."
        )
    }
}

fn label(kind: &str) -> String {
    format!("{kind} on {}", machine_name())
}

#[cfg(windows)]
mod hello {
    use anyhow::{Context, Result, bail};
    use ar_core::{Profile, approval::KeyAlg};
    use windows::{
        Security::{
            Credentials::{KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus},
            Cryptography::{Core::CryptographicPublicKeyBlobType, CryptographicBuffer},
        },
        Storage::Streams::IBuffer,
    };
    use windows_core::{Array, HSTRING};

    use super::{ApproverKey, Backend, label};

    /// One key per profile, so a test profile never touches the person's real key.
    pub fn name(profile: &Profile) -> String {
        format!("ai-remote approver {}", profile.instance_id())
    }

    fn bytes(buffer: &IBuffer) -> Result<Vec<u8>> {
        let mut out = Array::<u8>::new();
        CryptographicBuffer::CopyToByteArray(buffer, &mut out)?;
        Ok(out.to_vec())
    }

    fn key(name: &str, credential: &windows::Security::Credentials::KeyCredential) -> Result<ApproverKey> {
        let public =
            credential.RetrievePublicKeyWithBlobType(CryptographicPublicKeyBlobType::X509SubjectPublicKeyInfo)?;
        Ok(ApproverKey {
            alg: KeyAlg::RsaPkcs1Sha256,
            public_key: bytes(&public)?,
            label: label("Windows Hello"),
            backend: Backend::Hello(name.to_string()),
        })
    }

    fn supported() -> Result<bool> {
        Ok(KeyCredentialManager::IsSupportedAsync()?.join()?)
    }

    pub fn open(name: &str) -> Result<Option<ApproverKey>> {
        if !supported().unwrap_or(false) {
            return Ok(None);
        }
        let result = KeyCredentialManager::OpenAsync(&HSTRING::from(name))?.join()?;
        match result.Status()? {
            KeyCredentialStatus::Success => Ok(Some(key(name, &result.Credential()?)?)),
            KeyCredentialStatus::NotFound => Ok(None),
            other => bail!("Windows Hello could not open the approver key ({})", status(other)),
        }
    }

    pub fn create(name: &str) -> Result<ApproverKey> {
        if !supported().context("ask Windows whether Windows Hello is set up")? {
            bail!(
                "Windows Hello is not set up on this computer. Set up a PIN, fingerprint or face in Settings > \
                 Accounts > Sign-in options, then try again. Until then you can deny from here, and allow at the \
                 host or on another device."
            );
        }
        let result =
            KeyCredentialManager::RequestCreateAsync(&HSTRING::from(name), KeyCredentialCreationOption::FailIfExists)?
                .join()?;
        match result.Status()? {
            KeyCredentialStatus::Success => key(name, &result.Credential()?),
            other => bail!("Windows Hello did not create the approver key ({})", status(other)),
        }
    }

    pub fn sign(name: &str, message: &[u8]) -> Result<Vec<u8>> {
        let opened = KeyCredentialManager::OpenAsync(&HSTRING::from(name))?.join()?;
        if opened.Status()? != KeyCredentialStatus::Success {
            bail!("this device's approver key is gone ({}); set it up again", status(opened.Status()?));
        }
        let data = CryptographicBuffer::CreateFromByteArray(message)?;
        let signed = opened.Credential()?.RequestSignAsync(&data)?.join()?;
        match signed.Status()? {
            KeyCredentialStatus::Success => bytes(&signed.Result()?),
            other => bail!("not approved: {}", status(other)),
        }
    }

    fn status(s: KeyCredentialStatus) -> &'static str {
        match s {
            KeyCredentialStatus::UserCanceled => "cancelled",
            KeyCredentialStatus::UserPrefersPassword => "Windows Hello was declined",
            KeyCredentialStatus::NotFound => "no key",
            KeyCredentialStatus::CredentialAlreadyExists => "a key already exists",
            KeyCredentialStatus::SecurityDeviceLocked => "the security device is locked; try again later",
            _ => "Windows Hello error",
        }
    }
}

#[cfg(feature = "software-approver")]
mod software {
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result};
    use ar_core::{Profile, Role, approval};
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair},
    };

    use super::{ApproverKey, Backend, label};

    pub fn enabled() -> bool {
        std::env::var("AI_REMOTE_SOFTWARE_APPROVER").is_ok_and(|v| v == "1")
    }

    fn path(profile: &Profile) -> Result<PathBuf> {
        Ok(profile.role_dir(Role::Controller)?.join("approver-software.p8"))
    }

    fn load(path: &Path) -> Result<EcdsaKeyPair> {
        let pkcs8 = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &SystemRandom::new())
            .map_err(|_| anyhow::anyhow!("bad software approver key"))
    }

    pub fn open(profile: &Profile, create: bool) -> Result<Option<ApproverKey>> {
        let path = path(profile)?;
        if !path.exists() {
            if !create {
                return Ok(None);
            }
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &SystemRandom::new())
                .map_err(|_| anyhow::anyhow!("generate a key"))?;
            ar_core::store::write_atomic(&path, pkcs8.as_ref())?;
        }
        let pair = load(&path)?;
        Ok(Some(ApproverKey {
            alg: approval::KeyAlg::EcdsaP256Sha256,
            public_key: approval::p256_spki(pair.public_key().as_ref())?,
            label: label("software test key"),
            backend: Backend::Software(path),
        }))
    }

    pub fn sign(path: &Path, message: &[u8]) -> Result<Vec<u8>> {
        let sig = load(path)?.sign(&SystemRandom::new(), message).map_err(|_| anyhow::anyhow!("sign"))?;
        Ok(sig.as_ref().to_vec())
    }
}

#[cfg(test)]
mod tests {
    /// Looking for a key never prompts and finds none in a fresh profile.
    #[test]
    fn a_fresh_profile_has_no_approver_key() {
        let dir = std::env::temp_dir().join(format!("ar-approver-{}", std::process::id()));
        let profile = ar_core::Profile::at(&dir).unwrap();
        assert!(super::existing(&profile).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A call waiting on a host, as listed by [`pending`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Waiting {
    pub id: String,
    pub peer: String,
    pub tool: String,
    #[serde(default)]
    pub intent: Option<String>,
    pub args_hash: String,
    pub args_preview: String,
    pub expires_ms: u64,
}

/// The calls waiting for a person on `host` (or the only paired host), with the host's name.
pub async fn pending(agent: &mut ar_net::ipc::Conn, host: Option<&str>) -> Result<(String, Vec<Waiting>)> {
    let v = agent.request(&serde_json::json!({"op": "approvals", "host": host, "request": {"pending": true}})).await?;
    let host = v["host"].as_str().unwrap_or_default().to_string();
    Ok((host, serde_json::from_value(v["pending"].clone())?))
}

/// Finds a waiting call by its id or an unambiguous prefix of it (at least 6 characters).
pub async fn find(agent: &mut ar_net::ipc::Conn, host: Option<&str>, id: &str) -> Result<(String, Waiting)> {
    let (host, pending) = pending(agent, host).await?;
    let matches: Vec<_> =
        pending.into_iter().filter(|w| w.id == id || (id.len() >= 6 && w.id.starts_with(id))).collect();
    match matches.len() {
        1 => Ok((host, matches.into_iter().next().unwrap())),
        0 => anyhow::bail!("nothing with id '{id}' is waiting on host '{host}' (answered, timed out or mistyped)"),
        _ => anyhow::bail!("'{id}' matches several waiting calls; use more of the id"),
    }
}

/// Allows `waiting` on `host`: signs with this device's approver key, which makes the OS ask the
/// person (Windows Hello), then sends the answer. The host's device id comes from this device's own
/// pairing record, never from the host's message.
pub async fn allow(agent: &mut ar_net::ipc::Conn, profile: &Profile, host: &str, waiting: &Waiting) -> Result<()> {
    let key = existing(profile)?.ok_or_else(|| {
        anyhow::anyhow!(
            "this device has no approver key yet. Set it up with `ai-remote approver --enroll {host}` (someone at the \
             host confirms it once)"
        )
    })?;
    let hosts = agent.request(&serde_json::json!({"op": "hosts"})).await?;
    let device = hosts
        .as_array()
        .and_then(|a| a.iter().find(|h| h["name"] == host))
        .and_then(|h| h["device"].as_str())
        .ok_or_else(|| anyhow::anyhow!("host '{host}' is not paired with this device"))?
        .to_string();
    let target = AllowTarget {
        host: device,
        id: waiting.id.clone(),
        tool: waiting.tool.clone(),
        args_hash: waiting.args_hash.clone(),
        expires_ms: waiting.expires_ms,
    };
    let signer = key.clone();
    let signature = tokio::task::spawn_blocking(move || signer.sign_allow(&target)).await??;
    agent
        .request(&serde_json::json!({
            "op": "approvals",
            "host": host,
            "request": {"answer": {
                "id": waiting.id,
                "decision": "allow",
                "alg": key.alg,
                "key": key.public_key_b64(),
                "signature": BASE64.encode(&signature),
            }},
        }))
        .await?;
    Ok(())
}

/// Denies waiting call `id` on `host`; `note` is told to the agent. Needs no approver key.
pub async fn deny(agent: &mut ar_net::ipc::Conn, host: &str, id: &str, note: Option<&str>) -> Result<()> {
    let answer = serde_json::json!({"id": id, "decision": "deny", "note": note});
    agent.request(&serde_json::json!({"op": "approvals", "host": host, "request": {"answer": answer}})).await?;
    Ok(())
}

/// Offers this device's approver key to `host` (creating the key first, which the OS confirms with
/// the person). Someone at the host must accept it there; this waits for that answer.
pub async fn enroll(agent: &mut ar_net::ipc::Conn, profile: &Profile, host: Option<&str>) -> Result<serde_json::Value> {
    let p = profile.clone();
    let key = tokio::task::spawn_blocking(move || get_or_create(&p)).await??;
    let request = serde_json::json!({"enroll": {"alg": key.alg, "key": key.public_key_b64(), "label": key.label}});
    agent.request(&serde_json::json!({"op": "approvals", "host": host, "request": request})).await
}

/// Whether `host` accepted this device's approver key (`None`: this device has no key).
pub async fn accepted(agent: &mut ar_net::ipc::Conn, profile: &Profile, host: Option<&str>) -> Result<Option<bool>> {
    let Some(key) = existing(profile)? else { return Ok(None) };
    let request = serde_json::json!({"approver_status": {"alg": key.alg, "key": key.public_key_b64()}});
    let v = agent.request(&serde_json::json!({"op": "approvals", "host": host, "request": request})).await?;
    Ok(Some(v["accepted"] == true))
}
