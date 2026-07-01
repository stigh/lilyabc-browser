//! The egui application: file tree (left), source editor (centre), rendered score (right),
//! status/diagnostics (bottom). Selecting a file or tune submits an async render job.

use std::path::PathBuf;

use eframe::egui;

use crate::engraver::RenderRequest;
use crate::index::{self, DirNode, FileEntry};
use crate::model::RenderOutput;
use crate::worker::RenderWorker;

/// Idle time after the last keystroke before a live re-render fires.
const DEBOUNCE_MS: u64 = 550;

#[derive(Clone, Copy)]
struct Selection {
    entry: usize,
    tune: Option<u32>,
}

#[derive(Clone, Copy, PartialEq)]
enum ZoomMode {
    Manual,
    FitWidth,
}

pub struct App {
    worker: RenderWorker,
    egui_ctx: egui::Context,
    folder: Option<PathBuf>,
    entries: Vec<FileEntry>,
    tree: DirNode,
    selection: Option<Selection>,
    source: String,
    output: Option<RenderOutput>,
    messages: String,
    next_id: u64,
    latest_id: u64,
    rendering: bool,
    zoom: f32,
    zoom_mode: ZoomMode,
    status: String,
    last_edit_at: f64,
    pending_edit: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        let mut app = Self {
            worker: RenderWorker::spawn(cc.egui_ctx.clone()),
            egui_ctx: cc.egui_ctx.clone(),
            folder: None,
            entries: Vec::new(),
            tree: DirNode::default(),
            selection: None,
            source: String::new(),
            output: None,
            messages: String::new(),
            next_id: 0,
            latest_id: 0,
            rendering: false,
            zoom: 1.0,
            zoom_mode: ZoomMode::FitWidth,
            status: String::new(),
            last_edit_at: 0.0,
            pending_edit: false,
        };
        if let Some(path) = initial {
            app.open_path(path);
        }
        app
    }

    /// Open a folder, or a single file (opens its parent folder and selects it).
    fn open_path(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.open_folder(path, true);
        } else if path.is_file() {
            if let Some(parent) = path.parent() {
                self.open_folder(parent.to_path_buf(), false);
                if let Some(i) = self.entries.iter().position(|e| e.path == path) {
                    self.select(i, None);
                }
            }
        } else {
            self.status = format!("Path not found: {}", path.display());
        }
    }

    fn open_folder(&mut self, path: PathBuf, auto_select: bool) {
        self.entries = index::scan(&path);
        self.tree = index::build_tree(&path, &self.entries);
        self.status = format!("{} file(s) in {}", self.entries.len(), path.display());
        self.folder = Some(path);
        self.selection = None;
        self.replace_output(None);
        self.messages.clear();
        self.source.clear();
        if auto_select {
            let tune = self
                .entries
                .first()
                .and_then(|e| e.tunes.first().map(|t| t.number));
            if !self.entries.is_empty() {
                self.select(0, tune);
            }
        }
    }

    fn select(&mut self, entry: usize, tune: Option<u32>) {
        let Some(e) = self.entries.get(entry) else {
            return;
        };
        self.source = std::fs::read_to_string(&e.path).unwrap_or_default();
        self.selection = Some(Selection { entry, tune });
        self.render();
    }

    /// Submit a render of the current buffer/selection to the worker (latest-wins).
    fn render(&mut self) {
        let Some(sel) = self.selection else {
            return;
        };
        let Some(e) = self.entries.get(sel.entry) else {
            return;
        };
        let base_dir = e
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let req = RenderRequest {
            format: e.format,
            source: self.source.clone(),
            base_dir,
            tune: sel.tune,
        };
        self.next_id += 1;
        self.latest_id = self.next_id;
        self.rendering = true;
        self.worker.submit(self.next_id, req);
    }

    fn save(&mut self) {
        let Some(sel) = self.selection else {
            self.status = "Nothing selected to save".to_owned();
            return;
        };
        if let Some(e) = self.entries.get(sel.entry) {
            match std::fs::write(&e.path, &self.source) {
                Ok(()) => self.status = format!("Saved {}", e.path.display()),
                Err(err) => self.status = format!("Save failed: {err}"),
            }
        }
    }

    /// Replace the rendered output, freeing egui's cached image/texture entries for pages no
    /// longer shown. egui retains every `bytes://<hash>` URI it is given and never evicts on
    /// its own, so without this, browsing / live-editing grows CPU+GPU memory without bound.
    fn replace_output(&mut self, new: Option<RenderOutput>) {
        if let Some(old) = self.output.take() {
            let keep: std::collections::HashSet<&str> = new
                .as_ref()
                .map(|o| o.pages.iter().map(|p| p.uri.as_str()).collect())
                .unwrap_or_default();
            for p in &old.pages {
                if !keep.contains(p.uri.as_str()) {
                    self.egui_ctx.forget_image(&p.uri);
                }
            }
        }
        self.output = new;
    }

    fn tree_ui(&mut self, ui: &mut egui::Ui) {
        if self.entries.is_empty() {
            ui.label("(open a folder of .ly / .abc files)");
            return;
        }
        let mut clicked: Option<(usize, Option<u32>)> = None;
        self.dir_ui(&self.tree, ui, &mut clicked);
        if let Some((i, tune)) = clicked {
            self.select(i, tune);
        }
    }

    /// Render a directory node: sub-folders (collapsed) first, then files.
    fn dir_ui(
        &self,
        node: &DirNode,
        ui: &mut egui::Ui,
        clicked: &mut Option<(usize, Option<u32>)>,
    ) {
        for sub in &node.dirs {
            egui::CollapsingHeader::new(format!("{}/", sub.name))
                .id_salt(("dir", sub.name.as_str()))
                .default_open(false)
                .show(ui, |ui| self.dir_ui(sub, ui, clicked));
        }
        for &i in &node.files {
            self.file_ui(i, ui, clicked);
        }
    }

    /// Render a single file: a leaf for LilyPond, or an expandable tune list for ABC.
    fn file_ui(&self, i: usize, ui: &mut egui::Ui, clicked: &mut Option<(usize, Option<u32>)>) {
        let Some(e) = self.entries.get(i) else {
            return;
        };
        if e.tunes.is_empty() {
            let selected = matches!(self.selection, Some(s) if s.entry == i && s.tune.is_none());
            if ui.selectable_label(selected, e.title.as_str()).clicked() {
                *clicked = Some((i, None));
            }
        } else {
            // Bold the file name when the whole file (no specific tune) is selected.
            let file_selected =
                matches!(self.selection, Some(s) if s.entry == i && s.tune.is_none());
            let title = if file_selected {
                egui::RichText::new(e.title.as_str()).strong()
            } else {
                egui::RichText::new(e.title.as_str())
            };
            let resp = egui::CollapsingHeader::new(title)
                .id_salt(("file", i))
                .show(ui, |ui| {
                    for t in &e.tunes {
                        let selected = matches!(
                            self.selection,
                            Some(s) if s.entry == i && s.tune == Some(t.number)
                        );
                        if ui
                            .selectable_label(selected, format!("{}. {}", t.number, t.title))
                            .clicked()
                        {
                            *clicked = Some((i, Some(t.number)));
                        }
                    }
                });
            // Clicking the file row itself (not a tune) renders the whole file (all tunes).
            if resp.header_response.clicked() {
                *clicked = Some((i, None));
            }
        }
    }

    fn score_ui(&mut self, ui: &mut egui::Ui) {
        let Some(out) = self.output.as_ref() else {
            ui.label("(select a file to render the score)");
            return;
        };
        if out.pages.is_empty() {
            ui.colored_label(egui::Color32::RED, "The engraver produced no output.");
        }
        let mode = self.zoom_mode;
        let zoom = self.zoom;
        // Width available for a page, minus the white card's margins and a scrollbar.
        let avail_w = (ui.available_width() - 28.0).max(50.0);
        let scroll = if mode == ZoomMode::FitWidth {
            egui::ScrollArea::vertical()
        } else {
            egui::ScrollArea::both()
        };
        scroll.auto_shrink([false, false]).show(ui, |ui| {
            for page in &out.pages {
                let scale = match mode {
                    ZoomMode::Manual => zoom,
                    ZoomMode::FitWidth if page.width > 1.0 => {
                        (avail_w / page.width).clamp(0.1, 8.0)
                    }
                    ZoomMode::FitWidth => zoom,
                };
                // PNG pages are rasterized at render_scale×; divide back to natural zoom.
                let display_scale = scale / page.render_scale.max(0.01);
                let source = egui::ImageSource::Bytes {
                    uri: page.uri.clone().into(),
                    bytes: egui::load::Bytes::Shared(page.bytes.clone()),
                };
                // Sheet music is black ink on transparent: give each page a white
                // "paper" card so it stays readable under the dark UI theme.
                egui::Frame::NONE
                    .fill(egui::Color32::WHITE)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.add(egui::Image::new(source).fit_to_original_size(display_scale));
                    });
                ui.add_space(8.0);
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Absorb finished renders (keep only the newest).
        for res in self.worker.poll() {
            if res.id == self.latest_id {
                self.rendering = false;
                self.messages = res.output.diagnostics.trim().to_owned();
                self.status = if res.output.ok {
                    format!("Rendered {} page(s)", res.output.pages.len())
                } else {
                    "Render failed — see messages below".to_owned()
                };
                self.replace_output(Some(res.output));
            }
        }

        // Debounced live re-render: fire once the user pauses typing.
        if self.pending_edit {
            let now = ui.input(|i| i.time);
            if now - self.last_edit_at >= DEBOUNCE_MS as f64 / 1000.0 {
                self.pending_edit = false;
                self.render();
            }
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open Folder…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.open_folder(p, false);
                    }
                }
                if ui.button("Rescan").clicked() {
                    if let Some(folder) = self.folder.clone() {
                        self.entries = index::scan(&folder);
                        self.tree = index::build_tree(&folder, &self.entries);
                        self.status = format!("Rescanned: {} file(s)", self.entries.len());
                    }
                }
                if ui.button("Reload").clicked() {
                    self.render();
                }
                if ui.button("Save").clicked() {
                    self.save();
                }
                ui.separator();
                if ui
                    .selectable_label(self.zoom_mode == ZoomMode::FitWidth, "Fit width")
                    .clicked()
                {
                    self.zoom_mode = ZoomMode::FitWidth;
                }
                if ui.button("−").clicked() {
                    self.zoom_mode = ZoomMode::Manual;
                    self.zoom = (self.zoom / 1.25).max(0.2);
                }
                if ui.button("+").clicked() {
                    self.zoom_mode = ZoomMode::Manual;
                    self.zoom = (self.zoom * 1.25).min(8.0);
                }
                ui.label(if self.zoom_mode == ZoomMode::FitWidth {
                    "fit".to_owned()
                } else {
                    format!("{:.0}%", self.zoom * 100.0)
                });
                if self.rendering {
                    ui.separator();
                    ui.spinner();
                    ui.label("rendering…");
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(if self.status.is_empty() {
                "Ready"
            } else {
                self.status.as_str()
            });
        });

        egui::Panel::left("tree")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                ui.heading("Files");
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.tree_ui(ui));
            });

        // Editor lives in a resizable right panel; the score gets the large central area.
        egui::Panel::right("editor")
            .resizable(true)
            .default_size(420.0)
            .show(ui, |ui| {
                ui.heading("Source");
                ui.separator();
                if !self.messages.is_empty() {
                    egui::CollapsingHeader::new("Engraver messages")
                        .default_open(!self.output.as_ref().map(|o| o.ok).unwrap_or(true))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(self.messages.as_str()).monospace());
                        });
                    ui.separator();
                }
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let resp = ui.add(
                            egui::TextEdit::multiline(&mut self.source)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(24)
                                .hint_text("Select a file, or paste LilyPond / ABC here"),
                        );
                        if resp.changed() {
                            self.last_edit_at = ui.input(|i| i.time);
                            self.pending_edit = true;
                            ui.ctx().request_repaint_after(
                                std::time::Duration::from_millis(DEBOUNCE_MS),
                            );
                        }
                    });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Score");
            ui.separator();
            self.score_ui(ui);
        });
    }
}
