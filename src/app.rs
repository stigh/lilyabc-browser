//! The egui application: file tree (left), source editor (centre), rendered score (right),
//! status/diagnostics (bottom). Selecting a file or tune submits an async render job.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

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

/// What to do once a background folder scan finishes.
enum AfterScan {
    /// Manual "Open Folder": leave nothing selected.
    Nothing,
    /// Auto-select and render the first entry (opening a directory on startup).
    SelectFirst,
    /// Select and render a specific file (opening a file path directly).
    SelectPath(PathBuf),
    /// Rescan: re-locate the previously selected file by path (no re-render).
    Reselect(Option<(PathBuf, Option<u32>)>),
}

/// Result of a background folder scan, delivered back to the UI thread.
struct ScanResult {
    seq: u64,
    folder: PathBuf,
    entries: Vec<FileEntry>,
    tree: DirNode,
    after: AfterScan,
}

pub struct App {
    worker: RenderWorker,
    egui_ctx: egui::Context,
    scan_tx: Sender<ScanResult>,
    scan_rx: Receiver<ScanResult>,
    scanning: bool,
    scan_seq: u64,
    folder: Option<PathBuf>,
    entries: Vec<FileEntry>,
    tree: DirNode,
    selection: Option<Selection>,
    source: String,
    source_loaded: bool,
    output: Option<RenderOutput>,
    messages: String,
    next_id: u64,
    latest_id: u64,
    rendering: bool,
    zoom: f32,
    zoom_mode: ZoomMode,
    status: String,
    tool_warning: Option<String>,
    last_edit_at: f64,
    pending_edit: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        let (scan_tx, scan_rx) = channel();
        let mut app = Self {
            worker: RenderWorker::spawn(cc.egui_ctx.clone()),
            egui_ctx: cc.egui_ctx.clone(),
            scan_tx,
            scan_rx,
            scanning: false,
            scan_seq: 0,
            folder: None,
            entries: Vec::new(),
            tree: DirNode::default(),
            selection: None,
            source: String::new(),
            source_loaded: false,
            output: None,
            messages: String::new(),
            next_id: 0,
            latest_id: 0,
            rendering: false,
            zoom: 1.0,
            zoom_mode: ZoomMode::FitWidth,
            status: String::new(),
            tool_warning: missing_tools(),
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
            self.start_scan(path, true, AfterScan::SelectFirst);
        } else if path.is_file() {
            if let Some(parent) = path.parent() {
                self.start_scan(parent.to_path_buf(), true, AfterScan::SelectPath(path));
            }
        } else {
            self.status = format!("Path not found: {}", path.display());
        }
    }

    /// Start scanning `folder` off the UI thread (scanning ~hundreds of files reads+parses
    /// each, which would otherwise freeze the render loop). `reset` clears the current view
    /// immediately (a new folder); leave it false to keep the view while the list refreshes.
    fn start_scan(&mut self, folder: PathBuf, reset: bool, after: AfterScan) {
        if reset {
            self.selection = None;
            self.replace_output(None);
            self.messages.clear();
            self.source.clear();
            self.source_loaded = false;
            self.entries.clear();
            self.tree = DirNode::default();
            // Drop any in-flight render from the previous folder.
            self.next_id += 1;
            self.latest_id = self.next_id;
            self.rendering = false;
        }
        self.scan_seq += 1;
        let seq = self.scan_seq;
        self.scanning = true;
        self.status = format!("Scanning {}…", folder.display());
        let tx = self.scan_tx.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let entries = index::scan(&folder);
            let tree = index::build_tree(&folder, &entries);
            let _ = tx.send(ScanResult {
                seq,
                folder,
                entries,
                tree,
                after,
            });
            ctx.request_repaint();
        });
    }

    /// Apply a finished scan (ignoring superseded ones) and run its follow-up action.
    fn apply_scan(&mut self, r: ScanResult) {
        if r.seq != self.scan_seq {
            return; // a newer scan superseded this one
        }
        self.scanning = false;
        self.entries = r.entries;
        self.tree = r.tree;
        self.status = format!("{} file(s) in {}", self.entries.len(), r.folder.display());
        self.folder = Some(r.folder);
        match r.after {
            AfterScan::Nothing => {}
            AfterScan::SelectFirst => {
                let tune = self
                    .entries
                    .first()
                    .and_then(|e| e.tunes.first().map(|t| t.number));
                if !self.entries.is_empty() {
                    self.select(0, tune);
                }
            }
            AfterScan::SelectPath(path) => {
                if let Some(i) = self.entries.iter().position(|e| e.path == path) {
                    self.select(i, None);
                }
            }
            AfterScan::Reselect(prev) => {
                self.selection = prev.and_then(|(path, tune)| {
                    self.entries
                        .iter()
                        .position(|e| e.path == path)
                        .map(|entry| Selection { entry, tune })
                });
            }
        }
    }

    fn select(&mut self, entry: usize, tune: Option<u32>) {
        let Some(e) = self.entries.get(entry) else {
            return;
        };
        let path = e.path.clone();
        self.selection = Some(Selection { entry, tune });
        if self.load_source(&path) {
            self.render(false);
        }
    }

    /// Re-read the selected file from disk and force a fresh render (bypasses the cache,
    /// so external edits — including `\include` targets — are picked up).
    fn reload(&mut self) {
        let Some(sel) = self.selection else {
            return;
        };
        let Some(e) = self.entries.get(sel.entry) else {
            return;
        };
        let path = e.path.clone();
        if self.load_source(&path) {
            self.render(true);
        }
    }

    /// Load a file into the editor buffer (lossy UTF-8). Returns false on read error and
    /// leaves the buffer untouched, so a later Save cannot clobber an unreadable file.
    fn load_source(&mut self, path: &std::path::Path) -> bool {
        match std::fs::read(path) {
            Ok(bytes) => {
                self.source = String::from_utf8_lossy(&bytes).into_owned();
                self.source_loaded = true;
                true
            }
            Err(err) => {
                self.status = format!("Cannot read {}: {err}", path.display());
                self.source_loaded = false;
                false
            }
        }
    }

    /// Submit a render of the current buffer/selection to the worker (latest-wins).
    /// `force` bypasses the worker's render cache (used by Reload).
    fn render(&mut self, force: bool) {
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
            force,
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
        if !self.source_loaded {
            self.status = "Refusing to save: the file was not loaded successfully".to_owned();
            return;
        }
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
            ui.label(if self.scanning {
                "Scanning…"
            } else {
                "(open a folder of .ly / .abc files)"
            });
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

/// External tools we shell out to; returns a warning naming any missing from PATH, so a
/// broken install is visible instead of producing silently-blank output.
fn missing_tools() -> Option<String> {
    let missing: Vec<&str> = [
        ("lilypond", "lilypond"),
        ("abcm2ps", "abcm2ps"),
        ("rsvg-convert", "librsvg2-bin"),
    ]
    .into_iter()
    .filter_map(|(bin, pkg)| which::which(bin).is_err().then_some(pkg))
    .collect();
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "Missing: {} — install to render scores/titles",
            missing.join(", ")
        ))
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

        // Absorb finished folder scans (off the UI thread).
        for r in self.scan_rx.try_iter().collect::<Vec<_>>() {
            self.apply_scan(r);
        }

        // Debounced live re-render: fire once the user pauses typing.
        if self.pending_edit {
            let now = ui.input(|i| i.time);
            if now - self.last_edit_at >= DEBOUNCE_MS as f64 / 1000.0 {
                self.pending_edit = false;
                self.render(false);
            }
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open Folder…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.start_scan(p, true, AfterScan::Nothing);
                    }
                }
                if ui.button("Rescan").clicked() {
                    if let Some(folder) = self.folder.clone() {
                        // Remember the selected file by path so re-sorting can't misdirect Save.
                        let prev = self.selection.and_then(|s| {
                            self.entries.get(s.entry).map(|e| (e.path.clone(), s.tune))
                        });
                        self.start_scan(folder, false, AfterScan::Reselect(prev));
                    }
                }
                if ui.button("Reload").clicked() {
                    self.reload();
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
                if self.rendering || self.scanning {
                    ui.separator();
                    ui.spinner();
                    ui.label(if self.scanning { "scanning…" } else { "rendering…" });
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(w) = &self.tool_warning {
                    ui.colored_label(egui::Color32::from_rgb(220, 120, 40), w);
                    ui.separator();
                }
                ui.label(if self.status.is_empty() {
                    "Ready"
                } else {
                    self.status.as_str()
                });
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
