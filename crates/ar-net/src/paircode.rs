//! Pairing with a short code instead of the long ticket.
//!
//! A ticket (`mcpmesh-invite:...`, about 1000 characters) carries the host's address and a 32-byte
//! secret, so it works with nothing else running, but it has to be copied whole. A pair code is
//! eight digits a person reads off one screen and types on the other (or scans as a QR code). The
//! code is not the credential: it only finds the host and proves the controller saw the screen,
//! and the host then hands over an ordinary ticket, which is redeemed exactly as a pasted one
//! would be (same ceremony, same three-word safety code on both screens).
//!
//! How the code reaches the host, with no server of ours:
//!
//! - Both sides stretch the code with Argon2id into 64 bytes: a secret key for a short-lived
//!   *rendezvous endpoint* and an authentication key. The host binds an iroh endpoint with that key
//!   for as long as the code is shown; it publishes its address the way every endpoint does
//!   (n0's DNS/pkarr, and mDNS on the local network when `lan_discovery` allows it).
//! - The controller derives the same endpoint id from the code and dials it on
//!   [`ALPN_PAIR_CODE`] from a throwaway endpoint of its own. TLS proves the far side holds the
//!   code-derived key; the controller proves it knows the code with a keyed hash bound to both
//!   endpoint ids. Only then does the host send the ticket.
//! - The rendezvous endpoint is separate from the node's own, so it is outside the node's
//!   default-deny gate (which admits paired devices only) and the node's identity is never
//!   published under a guessable key.
//!
//! What limits guessing: an online guess means one Argon2id derivation plus a lookup for an id
//! that exists only while some host shows that exact code (10^8 codes, 10 minutes); a connection
//! that reaches the host without the code gets [`MAX_FAILED`] tries before the code is burned; a
//! code delivers one ticket and is gone. An observer who sees the rendezvous id (the DNS service,
//! a relay, or someone on the same network listening to mDNS) can search the code space offline,
//! one Argon2id derivation per candidate; [`KDF_MEMORY_KIB`] and the 10-minute life keep that
//! out of reach of anything short of a large cluster, and the safety words still have to match.
//! The design note with the alternatives considered is in `docs/architecture.md` ("Pairing").

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use iroh::{Endpoint, EndpointId, SecretKey, endpoint::Connection, protocol::Router};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{LanDiscovery, NetworkOptions, iroh};

/// ALPN the rendezvous endpoint serves (and the only one).
pub const ALPN_PAIR_CODE: &[u8] = b"app/ai-remote/pair-code/1";

/// Digits in a code.
pub const CODE_DIGITS: usize = 8;

/// How long a code is shown before it stops working.
pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);

/// Connections that reach the rendezvous endpoint without knowing the code, before it is burned.
pub const MAX_FAILED: u32 = 5;

/// Argon2id memory cost (KiB) and passes: 78 ms per code on an Apple M5 Max (release build, `kdf_cost`
/// below); slower machines and phones take longer. Paid once per code on each side.
pub const KDF_MEMORY_KIB: u32 = 64 * 1024;
const KDF_PASSES: u32 = 3;
const KDF_SALT: &[u8] = b"ai-remote pair-code v1";

/// How long the controller looks for the host behind a code.
const DIAL_TIMEOUT: Duration = Duration::from_secs(20);
/// How long either side waits for the other's one line.
const LINE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LINE: usize = 8 * 1024;

/// mDNS service name of rendezvous endpoints, apart from the node's own announcements.
const MDNS_SERVICE: &str = "ai-remote-pair";

const LINK_PREFIX: &str = "ai-remote://pair?code=";

/// An eight-digit pair code.
#[derive(Clone, PartialEq, Eq)]
pub struct PairCode(String);

impl std::fmt::Debug for PairCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A code is a short-lived credential: keep it out of logs.
        f.write_str("PairCode(********)")
    }
}

impl PairCode {
    /// A fresh random code.
    pub fn generate() -> Self {
        // Rejection sampling keeps every code equally likely.
        let digits = loop {
            let mut b = [0u8; 4];
            getrandom::fill(&mut b).expect("system randomness");
            let n = u32::from_le_bytes(b);
            if n < 4_200_000_000 {
                break n % 100_000_000;
            }
        };
        Self(format!("{digits:08}"))
    }

    /// Reads a code as people type or paste it: digits with any spaces, dashes or dots between
    /// them, or the `ai-remote://pair?code=` link of the QR code. `None` for anything else, a
    /// ticket included.
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        let s = s.strip_prefix(LINK_PREFIX).unwrap_or(s);
        if s.len() > 40 {
            return None;
        }
        let mut digits = String::with_capacity(CODE_DIGITS);
        for c in s.chars() {
            match c {
                '0'..='9' => digits.push(c),
                // Full-width digits from an input method.
                '０'..='９' => digits.push(char::from(b'0' + (c as u32 - '０' as u32) as u8)),
                ' ' | '-' | '.' | '_' | '\u{00a0}' | '\u{2009}' | '\u{202f}' | '\u{3000}' => {}
                _ => return None,
            }
        }
        (digits.len() == CODE_DIGITS).then_some(Self(digits))
    }

    /// The bare digits.
    pub fn digits(&self) -> &str {
        &self.0
    }

    /// Grouped for reading aloud: `4821 0937`.
    pub fn grouped(&self) -> String {
        format!("{} {}", &self.0[..4], &self.0[4..])
    }

    /// What the QR code holds: a link that opens the app on this code.
    pub fn link(&self) -> String {
        format!("{LINK_PREFIX}{}", self.0)
    }
}

/// What both sides derive from a code.
struct Keys {
    /// Secret key of the host's rendezvous endpoint.
    endpoint: SecretKey,
    /// Key of the controller's proof.
    auth: [u8; 32],
}

fn derive_blocking(code: &PairCode) -> Result<Keys> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(KDF_MEMORY_KIB, KDF_PASSES, 1, Some(64)).map_err(|e| anyhow!("argon2 params: {e}"))?;
    let mut out = [0u8; 64];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(code.digits().as_bytes(), KDF_SALT, &mut out)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    let mut endpoint = [0u8; 32];
    endpoint.copy_from_slice(&out[..32]);
    let mut auth = [0u8; 32];
    auth.copy_from_slice(&out[32..]);
    Ok(Keys { endpoint: SecretKey::from_bytes(&endpoint), auth })
}

async fn derive(code: &PairCode) -> Result<Keys> {
    let code = code.clone();
    tokio::task::spawn_blocking(move || derive_blocking(&code)).await.context("derive pair-code keys")?
}

/// The rendezvous endpoint id behind a code: what an observer would see.
#[cfg(test)]
fn rendezvous_id(code: &PairCode) -> EndpointId {
    derive_blocking(code).unwrap().endpoint.public()
}

fn proof(auth: &[u8; 32], controller: &EndpointId, host: &EndpointId) -> blake3::Hash {
    let mut h = blake3::Hasher::new_keyed(auth);
    h.update(b"ai-remote pair-code proof v1");
    h.update(controller.as_bytes());
    h.update(host.as_bytes());
    h.finalize()
}

#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    v: u32,
    /// Hex keyed hash proving the controller knows the code.
    proof: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Answer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ticket: Option<String>,
    /// Why no ticket: see [`Refusal`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused: Option<Refusal>,
}

/// Why the host behind a code did not hand over its ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The connection did not prove the code.
    WrongCode,
    /// The code was shown for longer than [`CODE_TTL`].
    Expired,
    /// The code already paired a device.
    Used,
    /// Too many connections failed to prove the code; it no longer works.
    Locked,
}

impl Refusal {
    fn message(self) -> &'static str {
        match self {
            Self::WrongCode => "the device behind this pair code refused it",
            Self::Expired => "this pair code has expired: show a new one on the other device",
            Self::Used => "this pair code was already used: show a new one on the other device",
            Self::Locked => {
                "this pair code was tried too many times and no longer works: show a new one on the other device"
            }
        }
    }
}

/// The host's side of one code: the ticket it guards and what has happened to it.
#[derive(Debug)]
pub(crate) struct Offer {
    ticket: String,
    expires: Instant,
    failed: u32,
    used: bool,
}

impl Offer {
    pub(crate) fn new(ticket: String, expires: Instant) -> Self {
        Self { ticket, expires, failed: 0, used: false }
    }

    /// Answers one connection: the ticket when `proven` and the code still works, otherwise why not.
    /// A delivered ticket and every failed proof count against the code.
    pub(crate) fn answer(&mut self, proven: bool, now: Instant) -> Result<String, Refusal> {
        if self.used {
            return Err(Refusal::Used);
        }
        if self.failed >= MAX_FAILED {
            return Err(Refusal::Locked);
        }
        if now >= self.expires {
            return Err(Refusal::Expired);
        }
        if !proven {
            self.failed += 1;
            return Err(if self.failed >= MAX_FAILED { Refusal::Locked } else { Refusal::WrongCode });
        }
        self.used = true;
        Ok(std::mem::take(&mut self.ticket))
    }

    /// The code can no longer deliver a ticket.
    pub(crate) fn finished(&self, now: Instant) -> bool {
        self.used || self.failed >= MAX_FAILED || now >= self.expires
    }
}

/// What the host shows: the code and until when it works.
#[derive(Debug, Clone, Serialize)]
pub struct CodeOffer {
    /// The eight digits.
    pub code: String,
    /// The digits grouped for display (`4821 0937`).
    pub grouped: String,
    /// The QR code's content.
    pub link: String,
    /// Epoch seconds.
    pub expires_at: u64,
}

/// A code the host is showing, with the endpoint that answers it.
pub(crate) struct RunningOffer {
    router: Router,
    state: Arc<Mutex<Offer>>,
}

impl RunningOffer {
    pub(crate) async fn stop(self) {
        let _ = self.router.shutdown().await;
    }

    pub(crate) fn finished(&self) -> bool {
        self.state.lock().expect("offer lock").finished(Instant::now())
    }
}

#[derive(Debug)]
struct OfferHandler {
    auth: [u8; 32],
    host: EndpointId,
    state: Arc<Mutex<Offer>>,
    done: tokio::sync::Notify,
}

impl iroh::protocol::ProtocolHandler for OfferHandler {
    async fn accept(&self, conn: Connection) -> Result<(), iroh::protocol::AcceptError> {
        if let Err(e) = self.serve(&conn).await {
            tracing::debug!(error = %format!("{e:#}"), "pair-code connection ended");
        }
        Ok(())
    }
}

impl OfferHandler {
    async fn serve(&self, conn: &Connection) -> Result<()> {
        let controller = conn.remote_id();
        let (mut send, recv) = tokio::time::timeout(LINE_TIMEOUT, conn.accept_bi()).await.context("no request")??;
        let line = read_line(recv).await;
        let proven = line
            .ok()
            .and_then(|l| serde_json::from_str::<Hello>(&l).ok())
            .filter(|h| h.v == 1)
            .and_then(|h| blake3::Hash::from_hex(h.proof.trim()).ok())
            // `blake3::Hash` compares in constant time.
            .is_some_and(|p| p == proof(&self.auth, &controller, &self.host));
        let verdict = self.state.lock().expect("offer lock").answer(proven, Instant::now());
        let answer = match verdict {
            Ok(ticket) => {
                tracing::info!(controller = %controller.fmt_short(), "pair code accepted; ticket handed over");
                Answer { ticket: Some(ticket), refused: None }
            }
            Err(r) => {
                tracing::warn!(controller = %controller.fmt_short(), refused = ?r, "pair code refused");
                Answer { ticket: None, refused: Some(r) }
            }
        };
        let mut bytes = serde_json::to_vec(&answer)?;
        bytes.push(b'\n');
        send.write_all(&bytes).await?;
        send.finish()?;
        // Let the reply reach the controller before the connection drops.
        let _ = tokio::time::timeout(LINE_TIMEOUT, conn.closed()).await;
        if self.state.lock().expect("offer lock").finished(Instant::now()) {
            self.done.notify_one();
        }
        Ok(())
    }
}

async fn read_line(recv: iroh::endpoint::RecvStream) -> Result<String> {
    let mut reader = BufReader::new(tokio::io::AsyncReadExt::take(recv, MAX_LINE as u64));
    let mut line = String::new();
    tokio::time::timeout(LINE_TIMEOUT, reader.read_line(&mut line)).await.context("no reply in time")??;
    if !line.ends_with('\n') {
        bail!("truncated line");
    }
    Ok(line)
}

/// A throwaway endpoint configured like the node's: the same relays, proxy and local discovery.
async fn ephemeral_endpoint(secret: SecretKey, net: &NetworkOptions, serve: bool) -> Result<Endpoint> {
    use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver};
    let relay = if net.relays.is_empty() {
        iroh::RelayMode::Default
    } else {
        let mut urls = net.relays.clone();
        if net.relay_fallback_n0 {
            urls.extend(crate::n0_relay_urls());
        }
        let urls = urls
            .iter()
            .map(|u| u.parse::<iroh::RelayUrl>().map_err(|e| anyhow!("relay {u:?}: {e}")))
            .collect::<Result<Vec<_>>>()?;
        iroh::RelayMode::custom(urls)
    };
    let mut b = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(relay)
        .address_lookup(PkarrResolver::n0_dns())
        .address_lookup(DnsAddressLookup::n0_dns());
    if serve {
        b = b.address_lookup(PkarrPublisher::n0_dns()).alpns(vec![ALPN_PAIR_CODE.to_vec()]);
    }
    if let Some((url, _)) = net.proxy.resolve() {
        b = b.proxy_url(url.parse().map_err(|e| anyhow!("proxy {url:?}: {e}"))?);
    }
    let ep = b.secret_key(secret).bind().await.context("bind pair-code endpoint")?;
    if net.lan != LanDiscovery::Off {
        let announce = serve && net.lan == LanDiscovery::On;
        match iroh_mdns_address_lookup::MdnsAddressLookup::builder()
            .advertise(announce)
            .service_name(MDNS_SERVICE)
            .build(ep.id())
        {
            Ok(mdns) => ep.address_lookup().map_err(|e| anyhow!("pair-code endpoint closed: {e}"))?.add(mdns),
            Err(e) => tracing::warn!(error = %e, "pair code: no local-network discovery"),
        }
    }
    Ok(ep)
}

/// Starts answering `code` with `ticket` until `expires`.
pub(crate) async fn start_offer(
    code: &PairCode,
    ticket: String,
    expires: Instant,
    net: &NetworkOptions,
) -> Result<RunningOffer> {
    let keys = derive(code).await?;
    let ep = ephemeral_endpoint(keys.endpoint, net, true).await?;
    serve_offer(ep, keys.auth, ticket, expires)
}

fn serve_offer(ep: Endpoint, auth: [u8; 32], ticket: String, expires: Instant) -> Result<RunningOffer> {
    let state = Arc::new(Mutex::new(Offer::new(ticket, expires)));
    let handler =
        Arc::new(OfferHandler { auth, host: ep.id(), state: state.clone(), done: tokio::sync::Notify::new() });
    let router = Router::builder(ep).accept(ALPN_PAIR_CODE, handler.clone()).spawn();
    // Take the endpoint down once the code can do nothing more, so its id stops being published.
    let closer = router.clone();
    tokio::spawn(async move {
        let until = tokio::time::Instant::from_std(expires);
        tokio::select! {
            _ = tokio::time::sleep_until(until) => {}
            _ = handler.done.notified() => {}
        }
        let _ = closer.shutdown().await;
    });
    Ok(RunningOffer { router, state })
}

/// Finds the host showing `code` and fetches its ticket.
pub(crate) async fn fetch_ticket(code: &PairCode, net: &NetworkOptions) -> Result<String> {
    let keys = derive(code).await?;
    let host = keys.endpoint.public();
    let ep = ephemeral_endpoint(SecretKey::generate(), net, false).await?;
    let r = fetch_from(&ep, host.into(), &keys.auth).await;
    ep.close().await;
    r
}

async fn fetch_from(ep: &Endpoint, host: iroh::EndpointAddr, auth: &[u8; 32]) -> Result<String> {
    let host_id = host.id;
    let conn = match tokio::time::timeout(DIAL_TIMEOUT, ep.connect(host, ALPN_PAIR_CODE)).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "pair code: dial failed");
            bail!(NOT_FOUND)
        }
        Err(_) => bail!(NOT_FOUND),
    };
    let (mut send, recv) = conn.open_bi().await.context("open pair-code stream")?;
    let hello = Hello { v: 1, proof: proof(auth, &ep.id(), &host_id).to_hex().to_string() };
    let mut bytes = serde_json::to_vec(&hello)?;
    bytes.push(b'\n');
    send.write_all(&bytes).await?;
    send.finish()?;
    let line = read_line(recv).await.context("the device behind this pair code did not answer")?;
    conn.close(0u32.into(), b"done");
    let answer: Answer = serde_json::from_str(&line).context("unreadable pair-code answer")?;
    match (answer.ticket, answer.refused) {
        (Some(t), _) => Ok(t),
        (None, Some(r)) => bail!(r.message()),
        (None, None) => bail!("the device behind this pair code gave no ticket"),
    }
}

const NOT_FOUND: &str = "no device answered this pair code: check the digits, and that the code is still shown on the \
                         other device (it lasts 10 minutes and works once)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_codes_the_way_people_type_them() {
        let want = Some(PairCode("48210937".into()));
        assert_eq!(PairCode::parse("48210937"), want);
        assert_eq!(PairCode::parse(" 4821 0937 "), want);
        assert_eq!(PairCode::parse("4821-0937"), want);
        assert_eq!(PairCode::parse("482.109.37"), want);
        assert_eq!(PairCode::parse("４８２１ ０９３７"), want, "full-width digits");
        assert_eq!(PairCode::parse("ai-remote://pair?code=48210937"), want);
        assert_eq!(PairCode::parse("4821093"), None, "seven digits");
        assert_eq!(PairCode::parse("482109377"), None, "nine digits");
        assert_eq!(PairCode::parse("4821O937"), None, "a letter");
        assert_eq!(PairCode::parse("mcpmesh-invite:GEZDGNBV"), None, "a ticket is not a code");
        let c = PairCode::parse("48210937").unwrap();
        assert_eq!(c.grouped(), "4821 0937");
        assert_eq!(PairCode::parse(&c.link()), Some(c.clone()));
        assert_eq!(format!("{c:?}"), "PairCode(********)", "never logged");
    }

    #[test]
    fn generates_eight_digit_codes() {
        let codes: std::collections::HashSet<String> =
            (0..200).map(|_| PairCode::generate().digits().to_string()).collect();
        assert!(codes.len() > 190, "codes repeat too often");
        assert!(codes.iter().all(|c| c.len() == CODE_DIGITS && c.bytes().all(|b| b.is_ascii_digit())));
    }

    #[test]
    fn both_sides_derive_the_same_endpoint_and_neighbouring_codes_do_not() {
        let a = PairCode::parse("12345678").unwrap();
        let b = PairCode::parse("12345679").unwrap();
        assert_eq!(rendezvous_id(&a), rendezvous_id(&a));
        assert_ne!(rendezvous_id(&a), rendezvous_id(&b));
        let (ka, kb) = (derive_blocking(&a).unwrap(), derive_blocking(&b).unwrap());
        assert_ne!(ka.auth, kb.auth);
    }

    /// What one stretch costs (the figure `docs/architecture.md` quotes):
    /// `cargo test -p ar-net --release --lib kdf_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, run by hand"]
    fn kdf_cost() {
        let code = PairCode::generate();
        derive_blocking(&code).unwrap();
        let n = 20;
        let start = Instant::now();
        for _ in 0..n {
            derive_blocking(&code).unwrap();
        }
        println!("argon2id {KDF_MEMORY_KIB} KiB x {KDF_PASSES}: {:?} per code", start.elapsed() / n);
    }

    #[test]
    fn a_code_survives_only_a_few_failed_proofs() {
        let now = Instant::now();
        let mut offer = Offer::new("ticket".into(), now + CODE_TTL);
        for _ in 1..MAX_FAILED {
            assert_eq!(offer.answer(false, now), Err(Refusal::WrongCode));
        }
        assert_eq!(offer.answer(false, now), Err(Refusal::Locked), "the last allowed failure burns it");
        assert_eq!(offer.answer(true, now), Err(Refusal::Locked), "even the right code is refused afterwards");
        assert!(offer.finished(now));
    }

    #[test]
    fn a_code_expires() {
        let now = Instant::now();
        let mut offer = Offer::new("ticket".into(), now + CODE_TTL);
        assert!(!offer.finished(now));
        let later = now + CODE_TTL;
        assert!(offer.finished(later));
        assert_eq!(offer.answer(true, later), Err(Refusal::Expired));
    }

    #[test]
    fn a_code_pairs_once() {
        let now = Instant::now();
        let mut offer = Offer::new("ticket".into(), now + CODE_TTL);
        assert_eq!(offer.answer(false, now), Err(Refusal::WrongCode));
        assert_eq!(offer.answer(true, now).as_deref(), Ok("ticket"), "a failure before does not block it");
        assert_eq!(offer.answer(true, now), Err(Refusal::Used));
        assert!(offer.finished(now));
    }

    /// Two hermetic endpoints on loopback: the whole exchange, and a wrong proof that does not burn the code.
    #[tokio::test(flavor = "multi_thread")]
    async fn hands_over_the_ticket_to_whoever_proves_the_code() {
        let code = PairCode::generate();
        let keys = derive(&code).await.unwrap();
        let bind = |secret: SecretKey, alpns: Vec<Vec<u8>>| async move {
            Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .secret_key(secret)
                .alpns(alpns)
                .bind()
                .await
                .unwrap()
        };
        let host_ep = bind(keys.endpoint.clone(), vec![ALPN_PAIR_CODE.to_vec()]).await;
        let addr = host_ep.addr();
        let offer = serve_offer(host_ep, keys.auth, "mcpmesh-invite:TICKET".into(), Instant::now() + CODE_TTL).unwrap();

        let wrong = bind(SecretKey::generate(), vec![]).await;
        let err = fetch_from(&wrong, addr.clone(), &[9; 32]).await.unwrap_err();
        assert!(format!("{err:#}").contains("refused"), "{err:#}");
        assert!(!offer.finished(), "one wrong proof does not burn the code");

        let right = bind(SecretKey::generate(), vec![]).await;
        assert_eq!(fetch_from(&right, addr.clone(), &keys.auth).await.unwrap(), "mcpmesh-invite:TICKET");
        assert!(offer.finished());

        // The used code takes its endpoint down (a later dial finds nobody: covered by `a_code_pairs_once`).
        offer.stop().await;
    }
}
