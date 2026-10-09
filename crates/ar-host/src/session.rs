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
    ///
    /// Ids carry the session id (`<session>-s<n>`): a controller that reconnects gets a new session
    /// whose counter starts again, and an id held over from the old one must fail as unknown
    /// instead of silently naming a different screenshot (maybe on another monitor).
    pub fn remember_shot(&self, geometry: ShotGeometry) -> String {
        let mut s = self.shots.lock().unwrap();
        s.next += 1;
        let id = self.shot_id(s.next);
        s.by_id.insert(id.clone(), geometry);
        s.last = Some(id.clone());
        // Keep memory bounded on long sessions.
        if s.by_id.len() > 64 {
            let oldest = self.shot_id(s.next - 64);
            s.by_id.remove(&oldest);
        }
        id
    }

    fn shot_id(&self, n: u32) -> String {
        format!("{}-s{n}", self.id)
    }

    /// Whether `id` looks like a screenshot id of another session (an earlier connection), so the
    /// caller can say why it is unknown.
    pub fn is_foreign_shot_id(&self, id: &str) -> bool {
        id.split_once("-s").is_some_and(|(session, _)| session != self.id)
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

    fn session() -> Session {
        let peer =
            Peer { name: "test".into(), principal: None, user_id: None, level: Some(Level::Observe), revoked: false };
        Session::new(peer, Level::Observe, CancellationToken::new())
    }

    fn any_geometry(image_width: u32) -> ShotGeometry {
        ShotGeometry {
            monitor_x: 0,
            monitor_y: 0,
            monitor_width: 1920,
            monitor_height: 1080,
            image_width,
            image_height: 720,
        }
    }

    #[test]
    fn shot_ids_are_unique_across_sessions() {
        let (a, b) = (session(), session());
        let id_a = a.remember_shot(any_geometry(1280));
        let id_b = b.remember_shot(any_geometry(640));
        assert_ne!(id_a, id_b);
        assert!(id_a.starts_with(&a.id) && id_a.ends_with("-s1"), "{id_a}");
        // An id from the other session is unknown here, and recognisably foreign.
        assert!(a.shot(Some(&id_b)).is_none());
        assert!(a.is_foreign_shot_id(&id_b) && !a.is_foreign_shot_id(&id_a));
        assert!(!a.is_foreign_shot_id("s1"));
        let (latest, g) = a.shot(None).unwrap();
        assert_eq!((latest, g.image_width), (id_a, 1280));
    }

    #[test]
    fn keeps_the_latest_64_shots() {
        let s = session();
        let first = s.remember_shot(any_geometry(1));
        for i in 2..=65 {
            s.remember_shot(any_geometry(i));
        }
        assert!(s.shot(Some(&first)).is_none());
        let (latest, g) = s.shot(None).unwrap();
        assert!(latest.ends_with("-s65") && g.image_width == 65);
        assert!(s.shot(Some(&format!("{}-s2", s.id))).is_some());
    }

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
