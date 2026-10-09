//! The result budget the gate applies to every tool result, built-in or mounted, so that one result
//! can neither flood an agent's context nor be refused by its model API:
//!
//! - text (and text resources) over [`budget::TEXT_LIMIT`] characters is cut to head and tail;
//! - structured content over that size is dropped, because a JSON value cannot be cut and stay
//!   valid; MCP servers send the same value as text, which is kept (and cut like any text);
//! - images over what model APIs accept are re-encoded or scaled down, with a note saying how, so
//!   pixel coordinates read from a scaled image can be mapped back;
//! - audio and binary resources over the image limit are replaced by a note.
//!
//! Mounted servers are the main reason this exists: their results are passed through unchanged, and
//! some (UI tree snapshots with a screenshot) are large by design.

use std::io::Cursor;

use ar_core::budget::{self, IMAGE_MAX_B64, IMAGE_MAX_EDGE, TEXT_LIMIT};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use image::{DynamicImage, codecs::jpeg::JpegEncoder, imageops::FilterType};
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};

/// JPEG quality for images that have to be re-encoded: small artefacts, readable text.
const JPEG_QUALITY: u8 = 85;

/// Applies the budget to `r`. Image work runs on a blocking thread.
pub async fn apply(mut r: CallToolResult) -> CallToolResult {
    budget_structured(&mut r);
    budget_text(&mut r);
    if !r
        .content
        .iter()
        .any(|c| matches!(c, ContentBlock::Image(_)))
    {
        return r;
    }
    match tokio::task::spawn_blocking(move || {
        budget_images(&mut r, IMAGE_MAX_B64, IMAGE_MAX_EDGE);
        r
    })
    .await
    {
        Ok(r) => r,
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!(
            "the tool ran, but its result could not be checked against the result budget ({e}); try again \
             with a smaller request"
        ))]),
    }
}

fn budget_structured(r: &mut CallToolResult) {
    let Some(value) = &r.structured_content else {
        return;
    };
    let json = serde_json::to_string(value).unwrap_or_default();
    let chars = json.chars().count();
    if chars <= TEXT_LIMIT {
        return;
    }
    r.structured_content = None;
    if !r.content.iter().any(|c| matches!(c, ContentBlock::Text(_))) {
        // The server sent no text form; this one takes its place (and is cut like any text).
        r.content.push(ContentBlock::text(json));
    }
    r.content.push(ContentBlock::text(format!(
        "[structured content omitted: {chars} characters, over the {TEXT_LIMIT}-character result budget. The \
         text above is what is left (cut to head and tail when it is over the budget too). Narrow the request \
         (a smaller window or region, a query, a lower element limit) to get it in full.]"
    )));
}

fn budget_text(r: &mut CallToolResult) {
    for block in &mut r.content {
        let replacement = match block {
            // `len()` is bytes, an upper bound for characters: a cheap filter before clipping.
            ContentBlock::Text(t) if t.text.len() > TEXT_LIMIT => {
                t.text = budget::clip(&t.text);
                None
            }
            ContentBlock::Resource(res) => match &mut res.resource {
                ResourceContents::TextResourceContents { text, .. } if text.len() > TEXT_LIMIT => {
                    *text = budget::clip(text);
                    None
                }
                ResourceContents::BlobResourceContents {
                    blob,
                    mime_type,
                    uri,
                    ..
                } if blob.len() > IMAGE_MAX_B64 => Some(omitted(
                    "resource",
                    blob.len(),
                    mime_type.as_deref(),
                    Some(uri),
                )),
                _ => None,
            },
            ContentBlock::Audio(a) if a.data.len() > IMAGE_MAX_B64 => {
                Some(omitted("audio", a.data.len(), Some(&a.mime_type), None))
            }
            _ => None,
        };
        if let Some(note) = replacement {
            *block = ContentBlock::text(note);
        }
    }
}

fn omitted(what: &str, b64_len: usize, mime: Option<&str>, uri: Option<&str>) -> String {
    format!(
        "[{what} omitted{}{}: {} of base64 data, over the {}-byte result budget]",
        mime.map(|m| format!(" ({m})")).unwrap_or_default(),
        uri.map(|u| format!(" {u}")).unwrap_or_default(),
        b64_len,
        IMAGE_MAX_B64
    )
}

fn budget_images(r: &mut CallToolResult, max_b64: usize, max_edge: u32) {
    let mut out = Vec::with_capacity(r.content.len() + 1);
    for block in std::mem::take(&mut r.content) {
        let ContentBlock::Image(mut img) = block else {
            out.push(block);
            continue;
        };
        match fit_image(&img.data, &img.mime_type, max_b64, max_edge) {
            Fit::Unchanged => out.push(ContentBlock::Image(img)),
            Fit::Replaced { data, note } => {
                img.data = data;
                img.mime_type = "image/jpeg".into();
                out.push(ContentBlock::Image(img));
                out.push(ContentBlock::text(note));
            }
            Fit::Dropped(note) => out.push(ContentBlock::text(note)),
        }
    }
    r.content = out;
}

#[derive(Debug)]
enum Fit {
    Unchanged,
    /// Re-encoded as JPEG (and possibly scaled); `note` tells the agent what changed.
    Replaced {
        data: String,
        note: String,
    },
    Dropped(String),
}

/// Keeps one base64 image within `max_b64` characters and `max_edge` pixels per side.
fn fit_image(data: &str, mime: &str, max_b64: usize, max_edge: u32) -> Fit {
    let over_size = data.len() > max_b64;
    let Ok(bytes) = BASE64.decode(data.trim()) else {
        // Not valid base64: nothing to shrink. Small ones pass (the client will report them).
        if over_size {
            return Fit::Dropped(dropped_note(
                mime,
                data.len(),
                max_b64,
                "it is not valid base64",
            ));
        }
        return Fit::Unchanged;
    };
    // The header is enough to know the size; most images need nothing more.
    let dims = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok());
    let over_edge = dims.is_some_and(|(w, h)| w.max(h) > max_edge);
    if !over_size && !over_edge {
        return Fit::Unchanged;
    }
    let img = match image::load_from_memory(&bytes) {
        Ok(img) => img,
        Err(e) => {
            let why = format!("it could not be decoded: {e}");
            return Fit::Dropped(dropped_note(mime, data.len(), max_b64, &why));
        }
    };
    let (w, h) = (img.width(), img.height());
    let mut scale = (max_edge as f64 / w.max(h) as f64).min(1.0);
    for _ in 0..8 {
        let (sw, sh) = (
            (w as f64 * scale).round().max(1.0) as u32,
            (h as f64 * scale).round().max(1.0) as u32,
        );
        let same_size = (sw, sh) == (w, h);
        let encoded = if same_size {
            encode_jpeg(&img)
        } else {
            encode_jpeg(&img.resize_exact(sw, sh, FilterType::Triangle))
        };
        let Some(encoded) = encoded else { break };
        let b64 = BASE64.encode(&encoded);
        if b64.len() <= max_b64 {
            let note = if same_size {
                format!(
                    "[image re-encoded as JPEG to fit the result budget ({} -> {} base64 characters); same size, so \
                     pixel coordinates in it are unchanged]",
                    data.len(),
                    b64.len()
                )
            } else {
                format!(
                    "[image scaled from {w}x{h} to {sw}x{sh} pixels to fit the result budget: multiply pixel \
                     coordinates read from it by {:.4} before passing them to a tool that expects the original \
                     image's pixels]",
                    w as f64 / sw as f64
                )
            };
            return Fit::Replaced { data: b64, note };
        }
        scale *= 0.7;
    }
    Fit::Dropped(dropped_note(
        mime,
        data.len(),
        max_b64,
        "it could not be made small enough",
    ))
}

fn dropped_note(mime: &str, b64_len: usize, max_b64: usize, why: &str) -> String {
    format!(
        "[image omitted ({mime}, {b64_len} base64 characters): over what model APIs accept ({max_b64} characters, \
         {IMAGE_MAX_EDGE} pixels per side), and {why}. Ask for a smaller image or region.]"
    )
}

fn encode_jpeg(img: &DynamicImage) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
    DynamicImage::ImageRgb8(img.to_rgb8())
        .write_with_encoder(encoder)
        .ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use image::{ImageFormat, RgbImage};
    use serde_json::json;

    use super::*;

    fn text_of(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| c.as_text())
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Deterministic noise: compresses badly in both PNG and JPEG.
    fn noise(w: u32, h: u32) -> DynamicImage {
        let mut seed = 0x2545_f491_u32;
        DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |_, _| {
            let mut px = [0u8; 3];
            for c in &mut px {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *c = seed as u8;
            }
            image::Rgb(px)
        }))
    }

    fn png_b64(img: &DynamicImage) -> String {
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        BASE64.encode(buf.into_inner())
    }

    fn decoded_dims(b64: &str) -> (u32, u32) {
        let img = image::load_from_memory(&BASE64.decode(b64).unwrap()).unwrap();
        (img.width(), img.height())
    }

    #[tokio::test]
    async fn small_results_are_unchanged() {
        let image = ContentBlock::image(png_b64(&noise(4, 4)), "image/png");
        let mut r = CallToolResult::success(vec![ContentBlock::text("hi"), image]);
        r.structured_content = Some(json!({"a": 1}));
        let before = serde_json::to_value(&r).unwrap();
        assert_eq!(serde_json::to_value(apply(r).await).unwrap(), before);
    }

    #[tokio::test]
    async fn oversized_structured_content_is_dropped_and_its_text_cut() {
        let value = json!({ "elements": "e".repeat(200_000) });
        let mut r = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
        r.structured_content = Some(value);
        let r = apply(r).await;
        assert!(r.structured_content.is_none());
        let text = text_of(&r);
        assert!(text.contains("characters omitted"), "the text form is cut");
        assert!(text.contains("structured content omitted") && text.contains("Narrow the request"));
        assert!(serde_json::to_string(&r).unwrap().len() < 2 * TEXT_LIMIT);
    }

    #[tokio::test]
    async fn structured_only_results_keep_a_text_form() {
        let mut r = CallToolResult::success(vec![]);
        r.structured_content = Some(json!({ "elements": "e".repeat(200_000) }));
        let r = apply(r).await;
        assert!(r.structured_content.is_none());
        assert!(
            text_of(&r).starts_with("{\"elements\":\"eee"),
            "head of the JSON stays readable"
        );
    }

    #[test]
    fn large_audio_and_blobs_are_replaced_by_a_note() {
        let mut r = CallToolResult::success(vec![ContentBlock::audio(
            "A".repeat(IMAGE_MAX_B64 + 4),
            "audio/wav",
        )]);
        budget_text(&mut r);
        assert!(text_of(&r).contains("audio omitted (audio/wav)"));
    }

    #[test]
    fn images_over_the_edge_limit_are_scaled_with_a_note() {
        let data = png_b64(&noise(200, 100));
        let Fit::Replaced { data, note } = fit_image(&data, "image/png", usize::MAX, 50) else {
            panic!("expected a scaled image")
        };
        assert_eq!(decoded_dims(&data), (50, 25));
        assert!(
            note.contains("200x100 to 50x25") && note.contains("4.0000"),
            "{note}"
        );
    }

    #[test]
    fn images_over_the_size_limit_are_reencoded_first() {
        let img = noise(96, 96);
        let png = png_b64(&img);
        let jpeg = BASE64.encode(encode_jpeg(&img).unwrap());
        assert!(jpeg.len() < png.len(), "noise: JPEG is smaller than PNG");
        let limit = (png.len() + jpeg.len()) / 2;
        let Fit::Replaced { data, note } = fit_image(&png, "image/png", limit, IMAGE_MAX_EDGE)
        else {
            panic!("expected JPEG")
        };
        assert_eq!(
            decoded_dims(&data),
            (96, 96),
            "same size: coordinates stay valid"
        );
        assert!(note.contains("coordinates in it are unchanged"), "{note}");

        // Too big even as JPEG at full size: scaled down until it fits.
        let Fit::Replaced { data, note } =
            fit_image(&png, "image/png", jpeg.len() / 3, IMAGE_MAX_EDGE)
        else {
            panic!("expected a scaled image")
        };
        assert!(data.len() <= jpeg.len() / 3);
        assert!(
            decoded_dims(&data).0 < 96 && note.contains("multiply pixel coordinates"),
            "{note}"
        );
    }

    #[test]
    fn undecodable_oversized_images_are_dropped() {
        let garbage = BASE64.encode(vec![7u8; 3000]);
        let fit = |data: &str, max_b64| fit_image(data, "image/webp", max_b64, IMAGE_MAX_EDGE);
        assert!(
            matches!(fit(&garbage, 100), Fit::Dropped(n) if n.contains("could not be decoded"))
        );
        assert!(
            matches!(fit(&garbage, usize::MAX), Fit::Unchanged),
            "small: passed on"
        );
        assert!(matches!(fit("%%%", 1), Fit::Dropped(n) if n.contains("not valid base64")));
    }

    #[test]
    fn budget_images_replaces_blocks_in_place() {
        let mut r = CallToolResult::success(vec![
            ContentBlock::text("before"),
            ContentBlock::image(png_b64(&noise(200, 100)), "image/png"),
            ContentBlock::text("after"),
        ]);
        budget_images(&mut r, usize::MAX, 50);
        assert_eq!(r.content.len(), 4);
        let img = r.content[1].as_image().expect("image stays in place");
        assert_eq!(img.mime_type, "image/jpeg");
        assert!(r.content[2].as_text().unwrap().text.contains("scaled"));
        assert_eq!(r.content[3].as_text().unwrap().text, "after");
    }
}
