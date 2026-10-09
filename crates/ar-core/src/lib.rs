//! Shared basics for every ai-remote component: where state lives, the device key, permission
//! levels, the host configuration file, tool-result budgets and remote-approval signatures.

pub mod approval;
pub mod budget;
pub mod config;
pub mod identity;
pub mod level;
pub mod profile;
pub mod store;

pub use level::Level;
pub use profile::{Profile, Role};

/// The MCP client name a controller gives the sessions it opens for a person at its keyboard (the
/// desktop app while someone drives); every other session is an agent's.
pub const PERSON_CLIENT: &str = "ai-remote person";

/// Version of this build, reported to peers in `system_info`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Best-effort display name of this machine.
pub fn machine_name() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return v.trim().to_string();
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
