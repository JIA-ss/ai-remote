//! The user-key RECOVERY PHRASE (#85 ask 2) — 32 key bytes rendered as words a human can write
//! down, and read back.
//!
//! # Why this exists
//!
//! A person's `b64u:` user id is the identity peers pin, kb audiences key on, and a roster names.
//! It lived in exactly one file on exactly one machine, with no export, import, or escrow verb
//! anywhere on the control surface. Replacing a laptop destroyed it: the new machine mints a fresh
//! user key, presents a new `b64u:`, and is a stranger even to peers that had pinned the old one.
//! The recovery path was an in-person SAS ceremony with every person you had ever paired with.
//!
//! This is the artifact that survives the hardware.
//!
//! # What it is NOT
//!
//! **It is not a password. It IS the private key**, in a form that fits on paper. Anyone who reads
//! it can present your identity. Treat it exactly as you would the key file — the point of the
//! encoding is that a human can transcribe it, not that it is protected.
//!
//! Holding the phrase also does not, by itself, get a new device admitted by your peers: they
//! authorize per DEVICE, and a restored user key does not put the new endpoint in anyone's
//! allowlist. That is #85 ask 3, and it is not shipped.
//!
//! # The encoding
//!
//! 33 words: 32 carrying one key byte each, then a checksum word.
//!
//! The alphabet is [`sas::WORDS`] — the first 256 words of the BIP39 English list, already in this
//! crate for the SAS. 256 words is exactly 8 bits, so the mapping is one word per byte with no bit
//! packing: a person can check their transcription word by word, and a decoder can name the
//! position that is wrong. Using the full 2048-word BIP39 list would fit the key in 24 words, at
//! the cost of an 11-bit packing where one wrong word corrupts its neighbours.
//!
//! The checksum is a byte of the BLAKE3 digest of the 32 key bytes, so a swapped or mistyped word
//! is refused rather than silently importing a different identity — which would be indistinguishable
//! from "your peers all forgot you".
use anyhow::{Context, Result};

use super::sas::WORDS;

/// How many words carry key material. The user key is ed25519: 32 secret bytes.
const KEY_WORDS: usize = 32;

/// Total words in a phrase — the key plus one checksum word.
pub const PHRASE_WORDS: usize = KEY_WORDS + 1;

/// The checksum word's byte: the first byte of `blake3(key)`.
///
/// One byte, so a transcription error slips through 1 time in 256. That is a deliberate trade
/// against phrase length, and it is not the last line of defence: an imported key that is wrong
/// yields a different `user_id`, which the import RESULT reports back so a person can compare it
/// against the one they are recovering.
fn checksum(key: &[u8; 32]) -> u8 {
    blake3::hash(key).as_bytes()[0]
}

/// Render 32 key bytes as a space-separated recovery phrase.
pub fn encode(key: &[u8; 32]) -> String {
    let mut words: Vec<&str> = key.iter().map(|b| WORDS[*b as usize]).collect();
    words.push(WORDS[checksum(key) as usize]);
    words.join(" ")
}

/// Parse a recovery phrase back to 32 key bytes.
///
/// Tolerant of how a human writes it down — any whitespace, any case — and intolerant of anything
/// that would change WHICH key comes out: an unknown word, the wrong count, or a failed checksum
/// are all refused by position, never guessed at.
pub fn decode(phrase: &str) -> Result<[u8; 32]> {
    let words: Vec<String> = phrase
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_ascii_alphabetic())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    anyhow::ensure!(
        words.len() == PHRASE_WORDS,
        "a recovery phrase is {PHRASE_WORDS} words; got {}",
        words.len()
    );
    let mut key = [0u8; 32];
    for (i, w) in words.iter().enumerate() {
        let idx = WORDS
            .iter()
            .position(|c| *c == w.as_str())
            // NAMED by position: "word 7 is not a recovery word" is actionable against a written
            // page; "invalid phrase" sends someone back to re-read all 33.
            .with_context(|| format!("word {} ('{w}') is not a recovery word", i + 1))?;
        if i < KEY_WORDS {
            key[i] = idx as u8;
        } else {
            anyhow::ensure!(
                idx as u8 == checksum(&key),
                "the recovery phrase's checksum word does not match — a word is mistyped or out of \
                 order. Importing anyway would restore a DIFFERENT identity, which looks exactly \
                 like every peer having forgotten you"
            );
        }
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phrase_round_trips() {
        for seed in [0u8, 1, 7, 255] {
            let key = [seed; 32];
            let phrase = encode(&key);
            assert_eq!(
                phrase.split_whitespace().count(),
                PHRASE_WORDS,
                "a phrase is one word per key byte plus a checksum"
            );
            assert_eq!(decode(&phrase).unwrap(), key);
        }
        // A key with EVERY byte distinct, so a codec that collapsed or reordered bytes fails.
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        assert_eq!(decode(&encode(&key)).unwrap(), key);
    }

    /// A person writes this on paper. It must survive being written on paper.
    #[test]
    fn transcription_is_forgiving_about_shape_and_not_about_content() {
        let key = [42u8; 32];
        let phrase = encode(&key);
        let words: Vec<&str> = phrase.split_whitespace().collect();

        // Case, extra whitespace and newlines.
        let messy = format!(
            "  {}\n\t{}  ",
            words[..4].join("  ").to_uppercase(),
            words[4..].join("\n")
        );
        assert_eq!(decode(&messy).unwrap(), key, "shape must not matter");

        // PUNCTUATION — the shape someone actually transcribes from a numbered list, and the one
        // this test used to CLAIM to cover while exercising only whitespace and case. A
        // `to_lowercase`-only decoder passed it; review caught that by mutation.
        let numbered: String = words
            .iter()
            .enumerate()
            .map(|(i, w)| format!("{}. {w},", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            decode(&numbered).unwrap(),
            key,
            "a phrase copied out of a numbered, comma-separated list must decode"
        );
        assert_eq!(
            decode(&words.join("; ")).unwrap(),
            key,
            "…and stray separators between words"
        );

        // A WRONG word is refused, and named by position.
        let mut bad = words.clone();
        bad[6] = "zzzz-not-a-word";
        let e = decode(&bad.join(" ")).unwrap_err();
        assert!(
            format!("{e:#}").contains("word 7"),
            "the error must name the position so it is actionable against a written page: {e:#}"
        );

        // The wrong COUNT is refused rather than padded.
        assert!(decode(&words[..PHRASE_WORDS - 1].join(" ")).is_err());
        assert!(decode(&format!("{phrase} abandon")).is_err());
        assert!(decode("").is_err());
    }

    /// THE property the checksum exists for: a phrase that decodes to the WRONG key must be
    /// refused, not imported.
    ///
    /// Restoring a different identity is not a visible failure — it looks exactly like every peer
    /// having forgotten you, which is the situation the person is already trying to fix.
    #[test]
    fn a_mistyped_word_is_refused_rather_than_restoring_a_different_identity() {
        // Every byte DISTINCT — an all-identical key makes a swap a no-op, which is a real
        // property (see `a_no_op_swap_still_decodes`) and would make this test assert nothing.
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        let words: Vec<String> = encode(&key)
            .split_whitespace()
            .map(str::to_string)
            .collect();

        // Swap two key words: still 33 valid words, still the right count — only the checksum
        // catches it.
        let mut swapped = words.clone();
        swapped.swap(3, 11);
        assert_ne!(
            swapped, words,
            "precondition: the swap actually changed the phrase"
        );
        let e = decode(&swapped.join(" ")).unwrap_err();
        assert!(format!("{e:#}").contains("checksum"), "{e:#}");

        // One key word replaced by a DIFFERENT valid word — the common transcription slip.
        let mut typo = words.clone();
        typo[0] = WORDS[(key[0] as usize + 1) % WORDS.len()].to_string();
        assert!(decode(&typo.join(" ")).is_err());

        // …and the checksum word itself being wrong is caught, so a phrase cannot be made to
        // validate by fixing up its last word. Picked as "not the real checksum" rather than a
        // literal, so the assertion cannot pass by coincidence.
        let mut tail = words.clone();
        let last = tail.len() - 1;
        let real = WORDS.iter().position(|w| *w == tail[last]).unwrap();
        tail[last] = WORDS[(real + 1) % WORDS.len()].to_string();
        assert!(decode(&tail.join(" ")).is_err());
    }

    /// A swap of two IDENTICAL bytes is not an error, and must not be reported as one — every byte
    /// of this key is the same, so any swap is a no-op.
    #[test]
    fn a_no_op_swap_still_decodes() {
        let key = [5u8; 32];
        let mut words: Vec<String> = encode(&key)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        words.swap(2, 20);
        assert_eq!(decode(&words.join(" ")).unwrap(), key);
    }
}
