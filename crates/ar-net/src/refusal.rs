//! Why a host refused or dropped a controller's connection, in words a person can act on.
//!
//! A refusal reaches the controller as a QUIC application close (code + reason bytes) or as a
//! failed dial. Left raw, an agent sees "closed by peer: unauthorized (code 401)" and has no idea
//! whether to retry, re-pair or ask someone. Both ends live in this crate's vocabulary: the host
//! closes with the constants below, and the transport (mcpmesh) closes with its own 401 codes, so
//! the mapping is kept here, next to the transport it describes.

use std::fmt;

use crate::iroh::endpoint::ConnectionError;

/// Close code the host uses when the transport admitted a device but the host will not serve it.
pub const CLOSE_NO_ACCESS: u32 = 403;
/// The device is not (or no longer) a paired peer of the host. Also sent by `forget` before the
/// pairing is removed, so the controller hears "pair again" rather than the transport's "revoked".
pub const REASON_NOT_PAIRED: &[u8] = b"not paired";
/// The device is paired but the host grants it no permission level.
pub const REASON_NO_LEVEL: &[u8] = b"no access level granted";
/// The host could not look the device up (its own state is unreadable).
pub const REASON_UNRESOLVED: &[u8] = b"cannot resolve caller";
/// Sent with close code 0 when the host stops.
pub const REASON_STOPPING: &[u8] = b"host stopping";

/// The transport gate's close code (mcpmesh `CLOSE_UNAUTHORIZED`).
const CLOSE_UNAUTHORIZED: u64 = 401;
/// Sent when the host owner revokes a device: its live connections are cut with this reason.
const REASON_ACCESS_REVOKED: &[u8] = b"access revoked";
/// Sent by the gate to a device it does not resolve: never paired, forgotten, or revoked earlier.
const REASON_UNAUTHORIZED: &[u8] = b"unauthorized";
/// Sent by a node that revoked its peer, on the connections that node dialled itself. Received
/// here it therefore means the *remote* revoked this device, not the other way round.
const REASON_REVOKED_BY_SENDER: &[u8] = b"revoked on this node";
/// How the transport words a dial this node refuses because it revoked the peer itself.
const DIAL_REVOKED_HERE: &str = "REVOKED on this node";

/// The plain-language category of a refused or lost connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The host owner revoked this device; the live connection was cut.
    Revoked,
    /// The host does not recognise this device: never paired, forgotten, or revoked earlier.
    NotPaired,
    /// The host knows this device but grants it no level.
    NoLevel,
    /// This machine itself refuses that peer (revoked here).
    RevokedHere,
    /// The host closed the connection without a refusal (it stopped or restarted).
    Closed,
    /// This side closed the connection (the host was unpaired here, or the agent is stopping).
    ClosedHere,
    /// The host is offline, unreachable or timed out.
    Unreachable,
    /// Anything else, with the raw reason for diagnosis.
    Other(String),
}

impl Refusal {
    /// Classifies a QUIC application close.
    pub fn from_close(code: u64, reason: &[u8]) -> Self {
        match (code, reason) {
            (CLOSE_UNAUTHORIZED, r)
                if r == REASON_ACCESS_REVOKED || r == REASON_REVOKED_BY_SENDER || r == b"roster revoked" =>
            {
                Refusal::Revoked
            }
            (CLOSE_UNAUTHORIZED, r) if r == REASON_UNAUTHORIZED => Refusal::NotPaired,
            (c, r) if c == CLOSE_NO_ACCESS as u64 && r == REASON_NOT_PAIRED => Refusal::NotPaired,
            (c, r) if c == CLOSE_NO_ACCESS as u64 && r == REASON_NO_LEVEL => Refusal::NoLevel,
            (0, _) => Refusal::Closed,
            (code, reason) => Refusal::Other(format!("closed by the host: {} (code {code})", String::from_utf8_lossy(reason))),
        }
    }

    /// Classifies how an established connection ended.
    pub fn from_connection_error(e: &ConnectionError) -> Self {
        match e {
            ConnectionError::ApplicationClosed(close) => Self::from_close(close.error_code.into_inner(), &close.reason),
            ConnectionError::TimedOut | ConnectionError::Reset => Refusal::Unreachable,
            ConnectionError::LocallyClosed => Refusal::ClosedHere,
            ConnectionError::VersionMismatch => {
                Refusal::Other("the host runs an incompatible transport version".into())
            }
            other => Refusal::Other(other.to_string()),
        }
    }

    /// Classifies a failed dial. A peer that is simply offline is a dial failure, so anything that
    /// is not a recognisable refusal counts as unreachable.
    pub fn from_dial_error(e: &anyhow::Error) -> Self {
        if let Some(ce) = e.chain().find_map(|c| c.downcast_ref::<ConnectionError>()) {
            let r = Self::from_connection_error(ce);
            if !matches!(r, Refusal::Other(_)) {
                return r;
            }
        }
        if format!("{e:#}").contains(DIAL_REVOKED_HERE) {
            return Refusal::RevokedHere;
        }
        Refusal::Unreachable
    }

    /// Retrying will not help until a person changes something.
    pub fn is_final(&self) -> bool {
        matches!(self, Refusal::Revoked | Refusal::NotPaired | Refusal::NoLevel | Refusal::RevokedHere)
    }

    /// The host owner has to act (lift a revocation, grant a level): worth naming this device so
    /// the owner can find it, even when its pairing (and with it its name there) is gone.
    pub fn needs_host_owner(&self) -> bool {
        matches!(self, Refusal::Revoked | Refusal::NotPaired | Refusal::NoLevel)
    }

    /// The explanation for a call that was running when the connection ended. Its request may
    /// already have reached the host, so "try again" would be bad advice for anything that changes
    /// state: the outcome is unknown and the host's state has to be checked first.
    pub fn during_call(&self) -> String {
        let why = match self {
            Refusal::Closed => {
                "the host closed the connection while the call was running (it stopped or restarted)".to_string()
            }
            Refusal::ClosedHere => "this controller closed the connection while the call was running".to_string(),
            other => other.to_string(),
        };
        format!("{why}. The call's outcome is unknown: it may have run on the host. Check the host's state before repeating it")
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The controller cannot tell whether the pairing was also removed after the revocation
            // (later dials only meet the gate's generic refusal), so both remedies are named.
            Refusal::Revoked => f.write_str(
                "access revoked by the host: it refuses this controller. Its owner can lift that (on the host: \
                 ai-remote unrevoke <this controller>); if the pairing was removed too, pair again afterwards with a \
                 fresh ticket. Do not retry until then",
            ),
            Refusal::NotPaired => f.write_str(
                "not paired: the host does not recognise this controller (never paired, the pairing was removed, or \
                 it was revoked there). Pair again with a fresh ticket (on the host: ai-remote pair-code)",
            ),
            Refusal::NoLevel => f.write_str(
                "no access level: the host knows this controller but grants it no level. Ask its owner to grant one \
                 (on the host: ai-remote level <this controller> control)",
            ),
            Refusal::RevokedHere => f.write_str("this controller has revoked that host; it will not connect to it"),
            Refusal::Closed => f.write_str("the host closed the connection (it stopped or restarted); try again shortly"),
            Refusal::ClosedHere => f.write_str(
                "this controller closed the connection (the host was unpaired here, or the controller agent is stopping)",
            ),
            Refusal::Unreachable => f.write_str(
                "host offline or unreachable: check that `ai-remote host` is running there and the machine is online",
            ),
            Refusal::Other(raw) => write!(f, "connection failed: {raw}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_transport_closes() {
        assert_eq!(Refusal::from_close(401, b"access revoked"), Refusal::Revoked);
        assert_eq!(Refusal::from_close(401, b"unauthorized"), Refusal::NotPaired);
        // Sent by the node that did the revoking: received here, the host revoked us.
        assert_eq!(Refusal::from_close(401, b"revoked on this node"), Refusal::Revoked);
        assert_eq!(Refusal::from_close(0, b""), Refusal::Closed);
        assert_eq!(Refusal::from_close(0, REASON_STOPPING), Refusal::Closed);
    }

    #[test]
    fn maps_host_closes() {
        assert_eq!(Refusal::from_close(CLOSE_NO_ACCESS as u64, REASON_NOT_PAIRED), Refusal::NotPaired);
        assert_eq!(Refusal::from_close(CLOSE_NO_ACCESS as u64, REASON_NO_LEVEL), Refusal::NoLevel);
        assert!(matches!(Refusal::from_close(CLOSE_NO_ACCESS as u64, REASON_UNRESOLVED), Refusal::Other(_)));
    }

    #[test]
    fn dial_failures_are_unreachable_unless_recognised() {
        let offline = anyhow::anyhow!("dial timed out").context("dial 'office' on a custom protocol");
        assert_eq!(Refusal::from_dial_error(&offline), Refusal::Unreachable);
        let revoked = anyhow::anyhow!("'office' is REVOKED on this node");
        assert_eq!(Refusal::from_dial_error(&revoked), Refusal::RevokedHere);
    }

    #[test]
    fn closing_our_own_side_is_not_blamed_on_the_host() {
        let r = Refusal::from_connection_error(&ConnectionError::LocallyClosed);
        assert_eq!(r, Refusal::ClosedHere);
        assert!(r.to_string().starts_with("this controller closed the connection"));
        assert!(!r.is_final());
    }

    #[test]
    fn a_call_in_flight_has_an_unknown_outcome() {
        let stopped = Refusal::Closed.during_call();
        assert!(stopped.starts_with("the host closed the connection while the call was running"), "{stopped}");
        assert!(!stopped.contains("try again"), "{stopped}");
        for r in [Refusal::Closed, Refusal::Revoked, Refusal::Unreachable, Refusal::ClosedHere] {
            assert!(r.during_call().contains("outcome is unknown"), "{r:?}");
        }
    }

    #[test]
    fn finality() {
        assert!(Refusal::Revoked.is_final());
        assert!(Refusal::NoLevel.is_final());
        assert!(!Refusal::Unreachable.is_final());
        assert!(!Refusal::Closed.is_final());
        assert!(!Refusal::ClosedHere.is_final());
        assert!(Refusal::Revoked.needs_host_owner());
        assert!(!Refusal::RevokedHere.needs_host_owner());
    }

    #[test]
    fn messages_name_the_fix() {
        assert!(Refusal::Revoked.to_string().starts_with("access revoked by the host"));
        // Both remedies: the controller cannot tell whether the pairing went too.
        assert!(Refusal::Revoked.to_string().contains("unrevoke"));
        assert!(Refusal::Revoked.to_string().contains("pair again"));
        assert!(Refusal::NotPaired.to_string().starts_with("not paired"));
        assert!(Refusal::NoLevel.to_string().starts_with("no access level"));
        assert!(Refusal::Unreachable.to_string().starts_with("host offline or unreachable"));
    }
}
