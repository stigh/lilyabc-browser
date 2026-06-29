//! Shared domain types used across the engraver, cache, and UI.

use std::path::Path;
use std::sync::Arc;

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

    pub fn label(self) -> &'static str {
        match self {
            Format::LilyPond => "LilyPond",
            Format::Abc => "ABC",
        }
    }
}

/// How a rendered [`Page`]'s bytes are encoded. Mirrors the URI extension egui uses
/// to pick an image loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Svg,
    Png,
}

impl PageKind {
    pub fn ext(self) -> &'static str {
        match self {
            PageKind::Svg => "svg",
            PageKind::Png => "png",
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
    pub kind: PageKind,
    /// Native size in px (from the SVG root, unit-converted at 96 dpi); 0.0 if unknown.
    pub width: f32,
    pub height: f32,
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
