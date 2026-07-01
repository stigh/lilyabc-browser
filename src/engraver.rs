//! Invokes the external engravers (`lilypond`, `abcm2ps`) and collects their SVG output.
//!
//! This module does not engrave music itself — it shells out to the canonical tools and
//! reads back the SVG they produce. Commands and output-file names were verified against
//! lilypond 2.24.3 and abcm2ps 8.14.14:
//!   * `lilypond -dbackend=svg -dcrop -dno-point-and-click -o BASE IN.ly` → `BASE.cropped.svg`
//!   * `abcm2ps -g -O BASE IN.abc` → `BASE001.svg`, `BASE002.svg`, … (one per tune)
//!
//! Both produce path-based SVG (no embedded music font), which `resvg` renders faithfully.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::model::{Format, Page, RenderOutput, RENDER_SCALE};

/// A single render request. The source text is rendered (rather than the file on disk) so
/// the same path serves both the file viewer and the live-edit buffer.
pub struct RenderRequest {
    pub format: Format,
    pub source: String,
    /// Working directory for the engraver, so LilyPond `\include` and relative paths resolve
    /// against the original file's directory.
    pub base_dir: PathBuf,
    /// For multi-tune ABC files, render only this 1-based tune; `None` renders all tunes.
    pub tune: Option<u32>,
    /// Bypass the worker's render cache (used by Reload to pick up external edits).
    pub force: bool,
}

/// The output stem used for engraver products inside the scratch dir.
const STEM: &str = "out";

/// Render `req`, using scratch directory `work` for the temp source and engraver output.
pub fn render(req: &RenderRequest, work: &Path) -> RenderOutput {
    match req.format {
        Format::LilyPond => render_lilypond(req, work),
        Format::Abc => render_abc(req, work),
    }
}

fn render_lilypond(req: &RenderRequest, work: &Path) -> RenderOutput {
    let mut out = RenderOutput::default();
    let input = work.join("sheet.ly");
    if let Err(e) = std::fs::write(&input, &req.source) {
        out.diagnostics = format!("cannot write temp source: {e}");
        return out;
    }
    let base = work.join(STEM);
    let mut cmd = Command::new("lilypond");
    cmd.current_dir(&req.base_dir)
        .arg("-dbackend=svg")
        .arg("-dcrop")
        .arg("-dno-point-and-click")
        .arg(format!("-I{}", req.base_dir.display()))
        .arg("-o")
        .arg(&base)
        .arg(&input);

    let (ok, log) = run(cmd);
    out.diagnostics = log;
    out.pages = collect_svgs(work)
        .into_iter()
        .filter_map(|p| read_page(&p))
        .collect();
    out.ok = ok && !out.pages.is_empty();
    out
}

fn render_abc(req: &RenderRequest, work: &Path) -> RenderOutput {
    let mut out = RenderOutput::default();
    let input = work.join("sheet.abc");
    if let Err(e) = std::fs::write(&input, &req.source) {
        out.diagnostics = format!("cannot write temp source: {e}");
        return out;
    }
    let base = work.join(STEM);
    let mut cmd = Command::new("abcm2ps");
    cmd.current_dir(&req.base_dir)
        .arg("-g") // SVG, one tune per file
        .arg("-O")
        .arg(&base);
    if let Some(n) = req.tune {
        cmd.arg(format!("-e{n}")); // select a single tune
    }
    cmd.arg(&input);

    let (ok, log) = run(cmd);
    out.diagnostics = log;
    out.pages = collect_svgs(work)
        .into_iter()
        .filter_map(|p| read_page(&p))
        .collect();
    out.ok = ok && !out.pages.is_empty();
    out
}

/// Run a command, capturing merged stdout+stderr and whether it exited successfully.
fn run(mut cmd: Command) -> (bool, String) {
    match cmd.output() {
        Ok(o) => {
            let mut log = String::new();
            log.push_str(&String::from_utf8_lossy(&o.stdout));
            log.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), log)
        }
        Err(e) => (false, format!("failed to run engraver: {e}")),
    }
}

/// Collect SVG products from the scratch dir, preferring LilyPond's cropped single image,
/// otherwise all `out*.svg` files in sorted (page/tune) order.
fn collect_svgs(dir: &Path) -> Vec<PathBuf> {
    let cropped = dir.join(format!("{STEM}.cropped.svg"));
    if cropped.is_file() {
        return vec![cropped];
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("svg")
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with(STEM))
        })
        .collect();
    found.sort();
    found
}

/// Read an engraver SVG and rasterize it to a font-aware PNG. usvg (used by egui's SVG
/// loader) cannot render the engravers' title text, but `rsvg-convert` (librsvg) can.
/// Falls back to the raw SVG (music only, no text) if `rsvg-convert` is unavailable.
fn read_page(svg_path: &Path) -> Option<Page> {
    let svg = std::fs::read(svg_path).ok()?;
    let width = svg_width_px(&svg);
    if let Some(png) = rasterize_png(svg_path) {
        let hash = blake3::hash(&png).to_hex();
        return Some(Page {
            uri: format!("bytes://{hash}.png"),
            bytes: Arc::from(png.into_boxed_slice()),
            width,
            render_scale: RENDER_SCALE,
        });
    }
    let hash = blake3::hash(&svg).to_hex();
    Some(Page {
        uri: format!("bytes://{hash}.svg"),
        bytes: Arc::from(svg.into_boxed_slice()),
        width,
        render_scale: 1.0,
    })
}

/// Rasterize an SVG to PNG bytes with `rsvg-convert` (librsvg is font-aware, at RENDER_SCALE×
/// with a white "paper" background).
fn rasterize_png(svg_path: &Path) -> Option<Vec<u8>> {
    let png_path = svg_path.with_extension("png");
    let ok = Command::new("rsvg-convert")
        .arg("-z")
        .arg(format!("{RENDER_SCALE}"))
        .arg("-b")
        .arg("white")
        .arg("-o")
        .arg(&png_path)
        .arg(svg_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then(|| std::fs::read(&png_path).ok()).flatten()
}

/// Parse the root `<svg>` width into pixels. usvg (egui's SVG loader) converts physical
/// units at 96 dpi, so matching that yields an exact fit-to-width scale.
fn svg_width_px(bytes: &[u8]) -> f32 {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
    attr_px(&head, "width")
}

fn attr_px(s: &str, name: &str) -> f32 {
    let needle = format!("{name}=\"");
    let Some(start) = s.find(&needle) else {
        return 0.0;
    };
    let rest = &s[start + needle.len()..];
    let Some(end) = rest.find('"') else {
        return 0.0;
    };
    let val = rest[..end].trim();
    let num: String = val
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    let n: f32 = num.parse().unwrap_or(0.0);
    match val.trim_start_matches(num.as_str()).trim() {
        "mm" => n * 96.0 / 25.4,
        "cm" => n * 96.0 / 2.54,
        "pt" => n * 96.0 / 72.0,
        "in" => n * 96.0,
        _ => n, // px or unitless
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_from_extension() {
        assert_eq!(Format::from_path(Path::new("a.ly")), Some(Format::LilyPond));
        assert_eq!(Format::from_path(Path::new("a.ABC")), Some(Format::Abc));
        assert_eq!(Format::from_path(Path::new("a.txt")), None);
    }
}
