//! Background jobs: long-running commands whose output goes to a log on the host.

use std::{path::PathBuf, sync::Arc};

use super::Tool;

/// Tracks background jobs started on this host.
pub struct JobManager {
    #[allow(dead_code)]
    dir: PathBuf,
}

impl JobManager {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

pub fn tools() -> Vec<Arc<dyn Tool>> {
    Vec::new()
}
