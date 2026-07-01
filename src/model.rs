//! Shared domain types used across the engraver, cache, and UI.

use std::path::Path;
use std::sync::Arc;

/// Rasterization zoom passed to `rsvg-convert` so zoomed-in scores stay crisp. The displayed
/// scale is divided by [`Page::render_scale`] to compensate.
pub const RENDER_SCALE: f32 = 2.0;

/// A sheet-music source format this app can browse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    LilyPond,
    Abc,
}

impl Format {
    /// Infer the format from a file extension (`.ly`/`.ily` → LilyPond, `.abc` → ABC).
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "ly" | "ily" => Some(Format::LilyPond),
            "abc" => Some(Format::Abc),
            _ => None,
        }
    }
}

/// One rendered page (LilyPond) or tune (ABC), as image bytes ready to hand to egui.
#[derive(Clone)]
pub struct Page {
    /// Content-addressed URI (`bytes://<hash>.<ext>`). The extension selects the egui
    /// image loader; the hash makes it a stable cache key that reuses the GPU texture
    /// whenever identical output is re-rendered.
    pub uri: String,
    pub bytes: Arc<[u8]>,
    /// Native width in px (from the SVG root, unit-converted at 96 dpi); 0.0 if unknown.
    pub width: f32,
    /// egui's intrinsic px ÷ natural px (RENDER_SCALE for a rasterized PNG, 1.0 for raw SVG).
    pub render_scale: f32,
}

/// The result of running an engraver.
#[derive(Clone, Default)]
pub struct RenderOutput {
    pub pages: Vec<Page>,
    /// Combined stdout/stderr from the engraver (warnings/errors shown in the status bar).
    pub diagnostics: String,
    /// True when the engraver produced at least one page.
    pub ok: bool,
}
