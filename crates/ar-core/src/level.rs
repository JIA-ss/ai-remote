//! Permission levels a host grants to each paired controller.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Ordered: `Observe < Control < Admin`. A tool requires one level; a caller may use it when the
/// caller's level is at least that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Read-only: system info, screenshots, window list, UI tree.
    Observe,
    /// Plus input and UI actions.
    Control,
    /// Plus shell, background jobs and files.
    Admin,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Observe, Level::Control, Level::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Observe => "observe",
            Level::Control => "control",
            Level::Admin => "admin",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "observe" => Ok(Level::Observe),
            "control" => Ok(Level::Control),
            "admin" => Ok(Level::Admin),
            other => Err(format!("unknown level '{other}' (expected observe, control or admin)")),
        }
    }
}
