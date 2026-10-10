//! The family's one codec (compact JSON, UTF-8, newline-terminated) plus the
//! session strike policy.
//!
//! The CODEC itself ([`FrameReader`], [`Inbound`], [`Violation`], [`write_frame`],
//! [`MAX_FRAME_BYTES`]) lives in `mcpmesh-codec` — ONE implementation provably shared by
//! both wire ends (this daemon side and the no-iroh `mcpmesh-local-api` client side) — and
//! is re-exported here so downstream paths keep compiling. [`Strikes`]/[`StrikeOutcome`]
//! are SESSION policy (who closes after repeated violations), not codec, so they stay in
//! net next to the session loop that enforces them (`endpoint::recv_frame`).
pub use mcpmesh_codec::{FrameReader, Inbound, MAX_FRAME_BYTES, Violation, write_frame};

#[derive(Debug, Default)]
pub struct Strikes(u8);

#[derive(Debug, PartialEq)]
pub enum StrikeOutcome {
    Continue,
    Close,
}

impl Strikes {
    pub fn register(&mut self) -> StrikeOutcome {
        // Latching: saturate so a u8 wrap can never reset the counter in release builds.
        self.0 = self.0.saturating_add(1);
        if self.0 >= 3 {
            StrikeOutcome::Close
        } else {
            StrikeOutcome::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_strikes_close() {
        let mut s = Strikes::default();
        assert_eq!(s.register(), StrikeOutcome::Continue);
        assert_eq!(s.register(), StrikeOutcome::Continue);
        assert_eq!(s.register(), StrikeOutcome::Close);
    }
}
