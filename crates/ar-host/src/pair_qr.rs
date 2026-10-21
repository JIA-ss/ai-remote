//! The QR code of a pair code: an SVG path for the apps to draw, and block characters for a terminal.

use qrcode::{Color, EcLevel, QrCode};
use serde::Serialize;

/// A QR code as one SVG path over a `size` x `size` grid of modules (quiet zone not included).
#[derive(Debug, Clone, Serialize)]
pub struct QrPath {
    pub size: usize,
    /// `M x y h1 v1 h-1 z` runs, one per dark stretch of a row.
    pub path: String,
}

fn encode(text: &str) -> Option<QrCode> {
    // The link is short, so the highest error correction still gives a small, coarse code.
    QrCode::with_error_correction_level(text.as_bytes(), EcLevel::Q).ok()
}

/// The QR code of `text` as an SVG path.
pub fn svg_path(text: &str) -> Option<QrPath> {
    let code = encode(text)?;
    let size = code.width();
    let colors = code.to_colors();
    let mut path = String::new();
    for y in 0..size {
        let row = &colors[y * size..(y + 1) * size];
        let mut x = 0;
        while x < size {
            if row[x] == Color::Dark {
                let start = x;
                while x < size && row[x] == Color::Dark {
                    x += 1;
                }
                path.push_str(&format!("M{start} {y}h{}v1h-{}z", x - start, x - start));
            } else {
                x += 1;
            }
        }
    }
    Some(QrPath { size, path })
}

/// The QR code of `text` in half-block characters, two module rows per line, with a quiet zone.
/// Dark modules are drawn as spaces on a light block background, so it scans on dark and light
/// terminals alike.
pub fn terminal(text: &str) -> Option<String> {
    let code = encode(text)?;
    let size = code.width();
    let colors = code.to_colors();
    let quiet = 2;
    let dark = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < size
            && (y as usize) < size
            && colors[y as usize * size + x as usize] == Color::Dark
    };
    let span = size as isize + quiet;
    let mut out = String::new();
    let mut y = -quiet;
    while y < span {
        for x in -quiet..span {
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (false, false) => '█',
                (true, false) => '▄',
                (false, true) => '▀',
                (true, true) => ' ',
            });
        }
        out.push('\n');
        y += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_a_small_code_for_a_pair_link() {
        let qr = svg_path("ai-remote://pair?code=48210937").unwrap();
        assert!(qr.size <= 33, "a pair link fits a small QR version, got {} modules", qr.size);
        assert!(qr.path.starts_with("M0 0h7v1h-7z"), "the finder pattern's top edge comes first");
        let text = terminal("ai-remote://pair?code=48210937").unwrap();
        assert_eq!(text.lines().count(), (qr.size + 4).div_ceil(2));
        assert!(text.lines().all(|l| l.chars().count() == qr.size + 4));
    }
}
