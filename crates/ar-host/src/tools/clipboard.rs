//! Built-in clipboard tools (filled in by the M0 tool work).

use std::sync::Arc;

use super::Tool;

pub fn tools() -> Vec<Arc<dyn Tool>> {
    Vec::new()
}
