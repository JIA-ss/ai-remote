//! Per-session state and the context every tool call receives.

use std::{collections::HashMap, sync::Mutex};

use ar_core::Level;
use ar_net::Peer;
use tokio_util::sync::CancellationToken;

use crate::HostState;

/// Where a screenshot came from, so coordinates in it can be mapped back to the screen.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ShotGeometry {
    /// Monitor origin and size in screen coordinates (physical pixels on Windows, points on macOS).
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub monitor_width: u32,
    pub monitor_height: u32,
    /// Size of the image the caller received.
    pub image_width: u32,
    pub image_height: u32,
}

impl ShotGeometry {
    /// Maps a pixel of the screenshot image to a screen coordinate (pixel centre, clamped to the
    /// monitor).
    pub fn to_screen(&self, px: f64, py: f64) -> (i32, i32) {
        let sx = self.monitor_width as f64 / self.image_width.max(1) as f64;
        let sy = self.monitor_height as f64 / self.image_height.max(1) as f64;
        let x = self.monitor_x as f64 + (px + 0.5) * sx;
        let y = self.monitor_y as f64 + (py + 0.5) * sy;
        let max_x = self.monitor_x + self.monitor_width as i32 - 1;
        let max_y = self.monitor_y + self.monitor_height as i32 - 1;
        ((x.floor() as i32).clamp(self.monitor_x, max_x), (y.floor() as i32).clamp(self.monitor_y, max_y))
    }
}

/// One MCP session from one controller over one stream.
pub struct Session {
    /// Short id used in the audit log.
    pub id: String,
    pub peer: Peer,
    /// Level at session start; the gate re-checks the current level on every call.
    pub level: Level,
    /// Cancelled when the session or the whole connection ends (and on revocation).
    pub cancel: CancellationToken,
    shots: Mutex<Shots>,
}

#[derive(Default)]
struct Shots {
    next: u32,
    last: Option<String>,
    by_id: HashMap<String, ShotGeometry>,
}

impl Session {
    pub fn new(peer: Peer, level: Level, cancel: CancellationToken) -> Self {
        let id = format!("{:08x}", rand_u32());
        Self { id, peer, level, cancel, shots: Mutex::new(Shots::default()) }
    }

    /// Records a screenshot's geometry and returns its id.
    pub fn remember_shot(&self, geometry: ShotGeometry) -> String {
        let mut s = self.shots.lock().unwrap();
        s.next += 1;
        let id = format!("s{}", s.next);
        s.by_id.insert(id.clone(), geometry);
        s.last = Some(id.clone());
        // Keep memory bounded on long sessions.
        if s.by_id.len() > 64 {
            let oldest = format!("s{}", s.next - 64);
            s.by_id.remove(&oldest);
        }
        id
    }

    /// Geometry of a screenshot by id, or of the latest one when `id` is `None`.
    pub fn shot(&self, id: Option<&str>) -> Option<(String, ShotGeometry)> {
        let s = self.shots.lock().unwrap();
        let id = id.map(str::to_string).or_else(|| s.last.clone())?;
        s.by_id.get(&id).map(|g| (id, *g))
    }
}

/// What a tool call can see.
pub struct ToolCtx<'a> {
    pub host: &'a HostState,
    pub session: &'a Session,
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    u32::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_downscaled_point_to_screen() {
        let g = ShotGeometry {
            monitor_x: 0,
            monitor_y: 0,
            monitor_width: 5120,
            monitor_height: 2160,
            image_width: 1280,
            image_height: 540,
        };
        assert_eq!(g.to_screen(640.0, 270.0), (2562, 1082));
    }

    #[test]
    fn maps_negative_origin_monitor() {
        let g = ShotGeometry {
            monitor_x: -1440,
            monitor_y: -9,
            monitor_width: 1440,
            monitor_height: 2560,
            image_width: 720,
            image_height: 1280,
        };
        let (x, y) = g.to_screen(0.0, 0.0);
        assert_eq!((x, y), (-1439, -8));
        assert_eq!(g.to_screen(10_000.0, 10_000.0), (-1, 2550));
    }
}
