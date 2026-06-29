//! `harness-gui` — a desktop front-end for authoring a spec's five-layer
//! vertical slice.
//!
//! The window is split in two:
//!
//! * **Left column** — every spec under `.specs/`, selectable.
//! * **Right main area** — an accordion of the selected spec's five layers, in
//!   production order: **requirements → design → tasks → code → evals**. Each
//!   section shows that layer's status and content, and offers a *Generate*
//!   button that is enabled only when every upstream layer already exists.
//!
//! Generation never runs in-process. Clicking *Generate* opens a small window
//! with an optional free-text prompt and a *Proceed* button; proceeding shells
//! out to the `harness` CLI (the same command a user would type) and streams its
//! output into a log pane at the bottom. Disk — `.specs/`, the spec's `owns`
//! globs, and `evals/<spec>/` — stays the single source of truth; when a job
//! finishes the accordion simply re-reads it.
//!
//! Only the **requirements** layer accepts a prompt today (`harness spec
//! requirements <spec> --brief …`). The four downstream CLI commands take no
//! free-text argument, so for those the prompt box is shown disabled with a
//! note rather than silently dropping what the user typed.

mod runner;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, RichText};

use harness_core::layers::{layer_state, Layer, LayerState, LayerStatus};
use harness_core::manifest::expand_owned_paths;
use harness_core::spec::{
    list_specs, load_requirements, load_tasks, spec_dir, Requirement, RequirementsFile, Task,
    TaskStatus,
};

use runner::RunHandle;

const POLL_INTERVAL: Duration = Duration::from_millis(400);

// ── palette ──────────────────────────────────────────────────────────────────
const ACCENT: Color32 = Color32::from_rgb(86, 182, 194);
const OK: Color32 = Color32::from_rgb(126, 192, 80);
const FAIL: Color32 = Color32::from_rgb(224, 108, 117);
const DIM: Color32 = Color32::from_rgb(130, 137, 151);
/// Slightly lighter than the window background, to set each layer box apart.
const BOX_BG: Color32 = Color32::from_rgb(38, 42, 50);
/// Fixed width for the Generate/Regenerate header button so its size doesn't
/// jump with the label length.
const GEN_BTN_W: f32 = 96.0;

fn main() -> eframe::Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Harness — spec layers"),
        ..Default::default()
    };
    eframe::run_native(
        "harness-gui",
        options,
        Box::new(|_cc| Ok(Box::new(GuiApp::new(root)))),
    )
}

// ── pending generation, captured by the modal window ─────────────────────────
struct GenDialog {
    layer: Layer,
    spec: String,
    prompt: String,
}

/// Everything the accordion needs for the selected spec, read once per tick.
struct Content {
    spec: String,
    state: LayerState,
    requirements: Option<RequirementsFile>,
    owns: Vec<String>,
    design: String,
    tasks: Vec<Task>,
    code_files: Vec<String>,
    eval_files: Vec<String>,
}

impl Content {
    fn load(root: &Path, spec: &str) -> Content {
        let dir = spec_dir(root, spec);
        let requirements = load_requirements(&dir).ok();
        let owns = requirements
            .as_ref()
            .map(|r| r.owns.clone())
            .unwrap_or_default();
        let design = std::fs::read_to_string(dir.join("2-design.md")).unwrap_or_default();
        let tasks = load_tasks(&dir).unwrap_or_default();
        let code_files = expand_owned_paths(root, &owns).unwrap_or_default();
        let eval_files = list_eval_files(root, spec);
        Content {
            spec: spec.to_string(),
            state: layer_state(root, spec),
            requirements,
            owns,
            design,
            tasks,
            code_files,
            eval_files,
        }
    }
}

struct GuiApp {
    root: PathBuf,
    specs: Vec<String>,
    selected: Option<String>,
    content: Option<Content>,
    last_load: Instant,

    /// The open generation modal, if any.
    dialog: Option<GenDialog>,

    /// The running CLI job (generation or eval run) and its streamed output.
    run: Option<RunHandle>,
    log: Vec<String>,
    last_exit: Option<i32>,

    /// One-shot override for every layer's collapsing state, set by the
    /// "Open all" / "Close all" buttons and cleared after a single frame.
    set_all_open: Option<bool>,
}

impl GuiApp {
    fn new(root: PathBuf) -> Self {
        let specs = list_specs(&root).unwrap_or_default();
        let selected = specs.first().cloned();
        let content = selected.as_ref().map(|s| Content::load(&root, s));
        GuiApp {
            root,
            specs,
            selected,
            content,
            last_load: Instant::now(),
            dialog: None,
            run: None,
            log: Vec::new(),
            last_exit: None,
            set_all_open: None,
        }
    }

    fn is_running(&self) -> bool {
        self.run.as_ref().map(|r| r.is_running()).unwrap_or(false)
    }

    fn select(&mut self, spec: &str) {
        self.selected = Some(spec.to_string());
        self.content = Some(Content::load(&self.root, spec));
        self.last_load = Instant::now();
    }

    /// Re-read the spec list and the selected spec's content from disk.
    fn reload(&mut self) {
        self.specs = list_specs(&self.root).unwrap_or_default();
        if let Some(sel) = &self.selected {
            if !self.specs.iter().any(|s| s == sel) {
                self.selected = self.specs.first().cloned();
            }
        } else {
            self.selected = self.specs.first().cloned();
        }
        self.content = self.selected.clone().map(|s| Content::load(&self.root, &s));
        self.last_load = Instant::now();
    }

    /// Launch `harness <args…>`, streaming output into the log pane.
    fn launch(&mut self, args: Vec<String>) {
        if self.is_running() {
            return;
        }
        self.log.clear();
        self.last_exit = None;
        self.log.push(format!("$ harness {}", args.join(" ")));
        match RunHandle::spawn_args(&self.root, &args) {
            Ok(h) => self.run = Some(h),
            Err(e) => {
                self.log.push(format!("failed to launch: {e}"));
                self.last_exit = Some(-1);
            }
        }
    }

    /// Drain the running job's output; reload content when it finishes.
    fn poll_job(&mut self) {
        let mut just_finished = false;
        if let Some(h) = self.run.as_mut() {
            self.log.extend(h.poll());
            if !h.is_running() {
                self.last_exit = h.exit_code();
                just_finished = true;
            }
        }
        if just_finished {
            self.run = None;
            self.reload();
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job();
        // Periodically re-read disk so externally-driven changes show up too.
        if self.last_load.elapsed() >= POLL_INTERVAL {
            if let Some(s) = self.selected.clone() {
                self.content = Some(Content::load(&self.root, &s));
            }
            self.last_load = Instant::now();
        }

        self.left_panel(ctx);
        self.bottom_log(ctx);
        self.central_accordion(ctx);
        self.generation_modal(ctx);

        // Keep polling a live job (and the disk timer) without user input.
        if self.is_running() {
            ctx.request_repaint_after(Duration::from_millis(120));
        } else {
            ctx.request_repaint_after(POLL_INTERVAL);
        }
    }
}

// ── panels ───────────────────────────────────────────────────────────────────
impl GuiApp {
    fn left_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("specs")
            .resizable(true)
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading("Specs");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .link("refresh")
                            .on_hover_text("Reload from disk")
                            .clicked()
                        {
                            self.reload();
                        }
                    });
                });
                ui.separator();
                if self.specs.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(DIM, "No specs under .specs/.");
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let specs = self.specs.clone();
                    for spec in specs {
                        let selected = self.selected.as_deref() == Some(spec.as_str());
                        if ui.selectable_label(selected, &spec).clicked() && !selected {
                            self.select(&spec);
                        }
                    }
                });
            });
    }

    fn central_accordion(&mut self, ctx: &egui::Context) {
        let mut reload_requested = false;
        let mut open_all = false;
        let mut close_all = false;

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(content) = self.content.as_ref() else {
                ui.centered_and_justified(|ui| {
                    ui.colored_label(DIM, "Select a spec on the left.");
                });
                return;
            };
            let spec = content.spec.clone();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(&spec);
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .link("refresh")
                        .on_hover_text("Reload from disk")
                        .clicked()
                    {
                        reload_requested = true;
                    }
                    ui.colored_label(DIM, "·");
                    if ui.link("close all").clicked() {
                        close_all = true;
                    }
                    ui.colored_label(DIM, "·");
                    if ui.link("open all").clicked() {
                        open_all = true;
                    }
                });
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                for layer in Layer::ALL {
                    self.layer_section(ui, layer);
                }
            });
        });

        if open_all {
            self.set_all_open = Some(true);
        } else if close_all {
            self.set_all_open = Some(false);
        } else {
            // The override only lasts the frame the button was pressed.
            self.set_all_open = None;
        }
        if reload_requested {
            self.reload();
        }
    }

    /// One accordion section for `layer`: status header, content, controls.
    fn layer_section(&mut self, ui: &mut egui::Ui, layer: Layer) {
        // Snapshot the status (owned) so the closure below doesn't hold a borrow
        // of `self.content` while it re-borrows `self` mutably for the controls.
        let Some(content) = self.content.as_ref() else {
            return;
        };
        let status = content.state.status(layer).clone();
        let upstream_ready = layer
            .upstream()
            .iter()
            .all(|&u| content.state.status(u).is_present());

        let (glyph_color, word) = match status {
            LayerStatus::Present => (OK, "present"),
            LayerStatus::Absent => (DIM, "absent"),
            LayerStatus::Invalid(_) => (FAIL, "invalid"),
        };
        // Upstream layers still missing — shown as a tooltip on the disabled
        // Generate button so the reason survives the accordion being collapsed.
        let missing: Vec<&'static str> = layer
            .upstream()
            .iter()
            .filter(|&&u| !content.state.status(u).is_present())
            .map(|u| u.label())
            .collect();

        // e.g. "● requirements (absent)" — status word now lives in the title.
        let header = format!("{}  {} ({})", status.glyph(), layer.label(), word);
        let default_open = matches!(status, LayerStatus::Present | LayerStatus::Invalid(_));
        let set_all_open = self.set_all_open;
        let running = self.is_running();
        let spec = content.spec.clone();

        // A bordered, filled box around each layer makes the sections easy to
        // tell apart from each other and from the window background.
        egui::Frame::group(ui.style()).fill(BOX_BG).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let id = ui.make_persistent_id(("layer", layer.label()));
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                default_open,
            );
            if let Some(open) = set_all_open {
                state.set_open(open);
            }
            state
                .show_header(ui, |ui| {
                    ui.label(RichText::new(header).color(glyph_color).strong());
                    // Generate / Regenerate sits at the far right of the header
                    // row, so it's reachable even when the section is collapsed.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if status.is_present() {
                            "Regenerate"
                        } else {
                            "Generate"
                        };
                        let can_generate = upstream_ready && !running;
                        let mut btn = ui.add_enabled(
                            can_generate,
                            egui::Button::new(label).min_size(egui::vec2(GEN_BTN_W, 0.0)),
                        );
                        if !missing.is_empty() {
                            btn = btn
                                .on_disabled_hover_text(format!("needs: {}", missing.join(", ")));
                        }
                        if btn.clicked() {
                            self.dialog = Some(GenDialog {
                                layer,
                                spec: spec.clone(),
                                prompt: String::new(),
                            });
                        }
                    });
                })
                .body(|ui| {
                    // Any validation reason (the status word is now in the title).
                    if let LayerStatus::Invalid(why) = &status {
                        ui.colored_label(FAIL, format!("— {why}"));
                    }

                    self.layer_body(ui, layer);

                    self.layer_controls(ui, layer, &status);
                });
        });
        ui.add_space(4.0);
    }

    /// Render the on-disk artifact for `layer`.
    fn layer_body(&self, ui: &mut egui::Ui, layer: Layer) {
        let Some(c) = self.content.as_ref() else {
            return;
        };
        match layer {
            Layer::Requirements => match &c.requirements {
                None => empty(ui),
                Some(reqs) => {
                    if let Some(intro) = &reqs.introduction {
                        if !intro.trim().is_empty() {
                            ui.label(intro);
                            ui.add_space(4.0);
                        }
                    }
                    for r in &reqs.requirements {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&r.id).monospace().color(ACCENT));
                            ui.label(req_summary(r));
                        });
                        for ac in &r.acceptance_criteria {
                            ui.label(RichText::new(format!("    ◦ {ac}")).color(DIM));
                        }
                    }
                    if !c.owns.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("owns: {}", c.owns.join(", "))).color(DIM));
                    }
                }
            },
            Layer::Design => {
                if c.design.trim().is_empty() {
                    empty(ui);
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("design-scroll")
                        .max_height(280.0)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(&c.design).monospace()).wrap());
                        });
                }
            }
            Layer::Tasks => {
                if c.tasks.is_empty() {
                    empty(ui);
                } else {
                    for t in &c.tasks {
                        ui.horizontal_wrapped(|ui| {
                            let (g, col) = task_badge(&t.status);
                            ui.colored_label(col, g);
                            ui.label(RichText::new(&t.id).monospace().color(DIM));
                            ui.label(&t.title);
                        });
                    }
                }
            }
            Layer::Code => {
                if c.code_files.is_empty() {
                    if c.owns.is_empty() {
                        ui.colored_label(DIM, "spec declares no `owns` globs.");
                    } else {
                        empty(ui);
                    }
                } else {
                    for f in &c.code_files {
                        ui.label(RichText::new(f).monospace());
                    }
                }
            }
            Layer::Evals => {
                if c.eval_files.is_empty() {
                    empty(ui);
                } else {
                    for f in &c.eval_files {
                        ui.label(RichText::new(f).monospace());
                    }
                }
            }
        }
    }

    /// In-body controls for `layer`. Generate/Regenerate now lives on the header
    /// row; only the eval layer keeps an in-body "Run eval suite" button.
    fn layer_controls(&mut self, ui: &mut egui::Ui, layer: Layer, status: &LayerStatus) {
        if layer != Layer::Evals {
            return;
        }
        let spec = self
            .content
            .as_ref()
            .map(|c| c.spec.clone())
            .unwrap_or_default();
        let running = self.is_running();

        ui.add_space(6.0);
        let can_run = status.is_present() && !running;
        if ui
            .add_enabled(can_run, egui::Button::new("▶ Run eval suite"))
            .clicked()
        {
            self.launch(vec!["eval".into(), "run".into(), spec.clone()]);
        }
    }

    fn bottom_log(&mut self, ctx: &egui::Context) {
        if self.log.is_empty() {
            return;
        }
        egui::TopBottomPanel::bottom("log")
            .resizable(true)
            .default_height(180.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Output");
                    if self.is_running() {
                        ui.colored_label(ACCENT, "running…");
                        if ui.small_button("Stop").clicked() {
                            if let Some(h) = self.run.as_mut() {
                                h.stop();
                            }
                        }
                    } else if let Some(code) = self.last_exit {
                        if code == 0 {
                            ui.colored_label(OK, "✓ exit 0");
                        } else {
                            ui.colored_label(FAIL, format!("✗ exit {code}"));
                        }
                        if ui.small_button("Clear").clicked() {
                            self.log.clear();
                            self.last_exit = None;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.label(RichText::new(line).monospace());
                        }
                    });
            });
    }

    /// The generation modal: optional prompt + Proceed.
    fn generation_modal(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let layer = dialog.layer;
        let spec = dialog.spec.clone();
        let prompt_supported = matches!(layer, Layer::Requirements);

        let mut open = true;
        let mut proceed = false;
        let mut cancel = false;

        egui::Window::new(format!("Generate {} — {}", layer.label(), spec))
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(format!(
                    "Produce the {} layer for spec “{}”.",
                    layer.label(),
                    spec
                ));
                ui.add_space(6.0);

                if prompt_supported {
                    ui.label("Additional prompting (optional):");
                    ui.add(
                        egui::TextEdit::multiline(&mut dialog.prompt)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY)
                            .hint_text("Extra brief passed to `--brief`…"),
                    );
                } else {
                    ui.label("Additional prompting (optional):");
                    ui.add_enabled(
                        false,
                        egui::TextEdit::multiline(&mut String::new())
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    ui.colored_label(
                        DIM,
                        format!(
                            "The `{}` command takes no free-text prompt yet — \
                             only requirements accepts one.",
                            cli_name(layer)
                        ),
                    );
                }

                ui.add_space(6.0);
                ui.colored_label(
                    DIM,
                    format!(
                        "$ harness {}",
                        preview_command(layer, &spec, &dialog.prompt)
                    ),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Proceed").clicked() {
                        proceed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if proceed {
            let args = gen_command(layer, &spec, &dialog.prompt);
            self.dialog = None;
            self.launch(args);
        } else if cancel || !open {
            self.dialog = None;
        }
    }
}

// ── command mapping ──────────────────────────────────────────────────────────

/// The CLI args that produce `layer` for `spec`. The optional `prompt` is only
/// threaded into the requirements command (`--brief`); the four downstream
/// commands take no free-text argument.
fn gen_command(layer: Layer, spec: &str, prompt: &str) -> Vec<String> {
    match layer {
        Layer::Requirements => {
            let mut a = vec!["spec".into(), "requirements".into(), spec.into()];
            let p = prompt.trim();
            if !p.is_empty() {
                a.push("--brief".into());
                a.push(p.to_string());
            }
            a
        }
        Layer::Design => vec!["spec".into(), "design".into(), spec.into()],
        Layer::Tasks => vec!["spec".into(), "tasks".into(), spec.into()],
        Layer::Code => vec!["build".into(), spec.into()],
        Layer::Evals => vec!["eval".into(), "draft".into(), spec.into()],
    }
}

/// A human-readable preview of the command the modal will run.
fn preview_command(layer: Layer, spec: &str, prompt: &str) -> String {
    let mut args = gen_command(layer, spec, prompt);
    // Quote the brief for readability in the preview.
    if let Some(i) = args.iter().position(|a| a == "--brief") {
        if let Some(v) = args.get_mut(i + 1) {
            *v = format!("\"{v}\"");
        }
    }
    args.join(" ")
}

/// The leaf subcommand name, for the "no prompt" note.
fn cli_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Requirements => "spec requirements",
        Layer::Design => "spec design",
        Layer::Tasks => "spec tasks",
        Layer::Code => "build",
        Layer::Evals => "eval draft",
    }
}

// ── small helpers ────────────────────────────────────────────────────────────

fn empty(ui: &mut egui::Ui) {
    ui.colored_label(DIM, "— not generated yet —");
}

/// A short one-line summary of a requirement, tolerant of EARS-style records
/// that carry structured fields instead of free `text`.
fn req_summary(r: &Requirement) -> String {
    if let Some(t) = &r.text {
        if !t.trim().is_empty() {
            return t.clone();
        }
    }
    let parts: Vec<&str> = [
        r.trigger.as_deref(),
        r.system.as_deref(),
        r.response.as_deref(),
        r.feature.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.trim().is_empty())
    .collect();
    if parts.is_empty() {
        "(structured requirement)".to_string()
    } else {
        parts.join(" — ")
    }
}

fn task_badge(s: &TaskStatus) -> (&'static str, Color32) {
    match s {
        TaskStatus::Done => ("✓", OK),
        TaskStatus::InProgress => ("▶", ACCENT),
        TaskStatus::Blocked => ("✗", FAIL),
        TaskStatus::Todo => ("·", DIM),
    }
}

/// Project-relative paths of files under `evals/<spec>/`.
fn list_eval_files(root: &Path, spec: &str) -> Vec<String> {
    let dir = root.join("evals").join(spec);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                let shown = p
                    .strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .to_string();
                out.push(shown);
            }
        }
    }
    out.sort();
    out
}
