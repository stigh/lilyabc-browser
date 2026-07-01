#![forbid(unsafe_code)]
//! lilyabc-browser — a desktop browser/viewer for LilyPond (.ly) and ABC (.abc)
//! sheet music. It does not engrave music itself; it shells out to the canonical
//! engravers (`lilypond`, `abcm2ps`) and displays their rendered output.

use eframe::egui;

mod app;
mod engraver;
mod index;
mod model;
mod worker;

use app::App;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("lilyabc-browser"),
        ..Default::default()
    };

    // Optional CLI arg: a folder (or file) to open on startup.
    let initial = std::env::args().nth(1).map(std::path::PathBuf::from);

    eframe::run_native(
        "lilyabc-browser",
        options,
        Box::new(move |cc| {
            // Enables `ui.image(...)` to rasterize our SVG/PNG bytes via the egui_extras loaders.
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(App::new(cc, initial)))
        }),
    )
}
