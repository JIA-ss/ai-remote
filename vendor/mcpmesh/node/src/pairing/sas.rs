//! Short authentication code (SAS): the read-aloud pairing verification words.
//!
//! A few words derived from a blake3 transcript hash over BOTH EndpointIds + the invite
//! secret, shown on BOTH sides at pair completion. Its whole job is to let a human catch a
//! whole-invite forgery / address-swap MITM out-of-band: if an attacker substitutes its own
//! endpoint into the invite, the transcript (and therefore the words) diverge, so the two
//! sides read out different codes and the humans abort. The code is **display-only** — it is
//! never sent on the wire, never checked programmatically, and carries no key material; its
//! only security property is entropy-per-word for the human comparison.
//!
//! The code MUST be **endpoint-order-independent**: the inviter computes it over
//! `(inviter_id, redeemer_id, secret)` and the redeemer over `(redeemer_id, inviter_id,
//! secret)`, yet both MUST print the same words. We get that by sorting the two ids before
//! hashing, so the transcript does not depend on who plays which role.

/// A fixed 256-word list (exactly 2^8 entries → a clean, uniform ~8 bits/word: `byte % 256
/// == byte`). Three words give a 24-bit SAS — the standard short-auth-string strength.
///
/// This is the first 256 words of the BIP39 English wordlist: a well-known public set of
/// short, lowercase, unambiguous common words (designed for a unique 4-char prefix, no
/// near-homophones, no profanity) — ideal for human read-aloud comparison. It is display-only
/// (see the module doc), so the exact membership is not security-critical beyond entropy.
pub(crate) static WORDS: &[&str] = &[
    "abandon", "ability", "able", "about", "above", "absent", "absorb", "abstract", "absurd",
    "abuse", "access", "accident", "account", "accuse", "achieve", "acid", "acoustic", "acquire",
    "across", "act", "action", "actor", "actress", "actual", "adapt", "add", "addict", "address",
    "adjust", "admit", "adult", "advance", "advice", "aerobic", "affair", "afford", "afraid",
    "again", "age", "agent", "agree", "ahead", "aim", "air", "airport", "aisle", "alarm", "album",
    "alcohol", "alert", "alien", "all", "alley", "allow", "almost", "alone", "alpha", "already",
    "also", "alter", "always", "amateur", "amazing", "among", "amount", "amused", "analyst",
    "anchor", "ancient", "anger", "angle", "angry", "animal", "ankle", "announce", "annual",
    "another", "answer", "antenna", "antique", "anxiety", "any", "apart", "apology", "appear",
    "apple", "approve", "april", "arch", "arctic", "area", "arena", "argue", "arm", "armed",
    "armor", "army", "around", "arrange", "arrest", "arrive", "arrow", "art", "artefact", "artist",
    "artwork", "ask", "aspect", "assault", "asset", "assist", "assume", "asthma", "athlete",
    "atom", "attack", "attend", "attitude", "attract", "auction", "audit", "august", "aunt",
    "author", "auto", "autumn", "average", "avocado", "avoid", "awake", "aware", "away", "awesome",
    "awful", "awkward", "axis", "baby", "bachelor", "bacon", "badge", "bag", "balance", "balcony",
    "ball", "bamboo", "banana", "banner", "bar", "barely", "bargain", "barrel", "base", "basic",
    "basket", "battle", "beach", "bean", "beauty", "because", "become", "beef", "before", "begin",
    "behave", "behind", "believe", "below", "belt", "bench", "benefit", "best", "betray", "better",
    "between", "beyond", "bicycle", "bid", "bike", "bind", "biology", "bird", "birth", "bitter",
    "black", "blade", "blame", "blanket", "blast", "bleak", "bless", "blind", "blood", "blossom",
    "blouse", "blue", "blur", "blush", "board", "boat", "body", "boil", "bomb", "bone", "bonus",
    "book", "boost", "border", "boring", "borrow", "boss", "bottom", "bounce", "box", "boy",
    "bracket", "brain", "brand", "brass", "brave", "bread", "breeze", "brick", "bridge", "brief",
    "bright", "bring", "brisk", "broccoli", "broken", "bronze", "broom", "brother", "brown",
    "brush", "bubble", "buddy", "budget", "buffalo", "build", "bulb", "bulk", "bullet", "bundle",
    "bunker", "burden", "burger", "burst", "bus", "business", "busy", "butter", "buyer", "buzz",
    "cabbage", "cabin", "cable",
];

/// Derive the display-only short authentication code (SAS) for a pairing.
///
/// Endpoint-order-independent: `short_auth_code(a, b, s) == short_auth_code(b, a, s)`, so the
/// inviter and redeemer print the same words regardless of role. A change to either endpoint
/// id or the secret changes the words (that is the whole-invite-forgery defense).
pub fn short_auth_code(id_a: &[u8; 32], id_b: &[u8; 32], secret: &[u8; 32]) -> String {
    // Order-independent: sort the two ids so inviter/redeemer compute the same transcript.
    let (lo, hi) = if id_a <= id_b {
        (id_a, id_b)
    } else {
        (id_b, id_a)
    };
    let mut h = blake3::Hasher::new();
    h.update(b"mcpmesh/pair/sas/1"); // domain separation
    h.update(lo);
    h.update(hi);
    h.update(secret);
    let digest = h.finalize();
    let bytes = digest.as_bytes();

    // Three words from the first three digest bytes. `WORDS` is a fixed non-empty list, so the
    // `% n` (and thus the index) is provably in bounds — the debug_assert makes that explicit.
    let n = WORDS.len();
    debug_assert!(
        !WORDS.is_empty(),
        "WORDS must be non-empty for `% n` to be safe"
    );
    (0..3)
        .map(|i| WORDS[bytes[i] as usize % n])
        .collect::<Vec<_>>()
        .join("-")
}

/// A short-word fingerprint of a public key, for the pinned org-root trust-ceremony display
/// Reuses the SAS `WORDS` (same read-aloud property). Domain-separated
/// from the pairing SAS so the two never collide. Four words (~32-bit) — a human cross-check value,
/// NEVER the raw key.
pub fn fingerprint_words(pk_bytes: &[u8]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"mcpmesh/roster/org-root-fingerprint/1"); // domain separation (distinct from the pairing SAS)
    h.update(pk_bytes);
    let digest = h.finalize();
    let bytes = digest.as_bytes();
    let n = WORDS.len();
    debug_assert!(
        !WORDS.is_empty(),
        "WORDS must be non-empty for `% n` to be safe"
    );
    (0..4)
        .map(|i| WORDS[bytes[i] as usize % n])
        .collect::<Vec<_>>()
        .join("-")
}

/// A short-word fingerprint of a JOIN CODE's identity fields (`user_pk ∥ device_endpoint_id`), for
/// the enrollment ceremony: the joiner and operator read it back out-of-band to confirm they hold the
/// SAME join code — catching a substituted code on the operator-ward channel (an attacker's code
/// carries a DIFFERENT `user_pk`, so the words diverge). The enrollment analog of the pairing SAS.
/// Domain-separated (`b"mcpmesh/join/code-fingerprint/1"`) from the org-root fingerprint, the pairing
/// SAS, and the device-binding sig, so a value can never be confused across purposes. Covers
/// `user_pk` (the person→user_pk bind the other artifacts miss) + `device_endpoint_id`. NEVER the raw key.
pub fn join_code_fingerprint(user_pk: &[u8], device_endpoint_id: &[u8]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"mcpmesh/join/code-fingerprint/1");
    h.update(user_pk);
    h.update(device_endpoint_id);
    let digest = h.finalize();
    let bytes = digest.as_bytes();
    let n = WORDS.len();
    debug_assert!(!WORDS.is_empty());
    (0..4)
        .map(|i| WORDS[bytes[i] as usize % n])
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_transcript_yields_same_code_order_independent_endpoints() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let secret = [9u8; 32];
        // Both sides MUST derive the SAME code regardless of who is "inviter":
        let code1 = short_auth_code(&a, &b, &secret);
        let code2 = short_auth_code(&b, &a, &secret); // swapped inviter/redeemer
        assert_eq!(code1, code2, "SAS must be endpoint-order-independent");
        // A different secret → different code.
        assert_ne!(code1, short_auth_code(&a, &b, &[8u8; 32]));
        // Shape: a few words joined by '-'.
        assert!(code1.split('-').count() >= 3);
    }

    #[test]
    fn fingerprint_words_is_deterministic_and_shaped() {
        let a = fingerprint_words(&[7u8; 32]);
        assert_eq!(a, fingerprint_words(&[7u8; 32])); // deterministic
        assert_ne!(a, fingerprint_words(&[8u8; 32])); // key-sensitive
        assert_eq!(a.split('-').count(), 4); // four words
        // Not the raw key bytes anywhere.
        assert!(!a.contains("b64u"));
    }

    #[test]
    fn join_code_fingerprint_is_deterministic_and_binds_the_user_pk() {
        let a = join_code_fingerprint(&[1u8; 32], &[2u8; 32]);
        assert_eq!(a, join_code_fingerprint(&[1u8; 32], &[2u8; 32])); // deterministic
        assert_ne!(a, join_code_fingerprint(&[9u8; 32], &[2u8; 32])); // a DIFFERENT user_pk → different words
        assert_ne!(a, join_code_fingerprint(&[1u8; 32], &[9u8; 32])); // a different device → different words
        // Domain-separated from the org-root fingerprint over the same 32 bytes (different words).
        assert_ne!(a, fingerprint_words(&[1u8; 32]));
        assert_eq!(a.split('-').count(), 4);
        assert!(!a.contains("b64u"));
    }

    #[test]
    fn wordlist_is_at_least_256_and_has_no_duplicates() {
        assert!(
            WORDS.len() >= 256,
            "SAS needs >= 256 words for ~8 bits/word (got {})",
            WORDS.len()
        );
        let mut sorted = WORDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            WORDS.len(),
            "SAS wordlist must have no duplicates"
        );
    }
}
