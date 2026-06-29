//! `harness-gui` — a desktop front-end for authoring a spec's five-layer
//! vertical slice.
//!
//! The window is split in two:
//!
//! * **Left column** — every spec under `.specs/`, selectable, plus a
//!   *new spec* link that drafts one from a typed description or a brief file.
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
/// Distinct accent for the project (root folder) name in the left column header.
const PROJECT: Color32 = Color32::from_rgb(198, 160, 246);
/// Fixed width for the Generate/Regenerate header button so its size doesn't
/// jump with the label length.
const GEN_BTN_W: f32 = 96.0;

fn main() -> eframe::Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let title = format!(
        "Harness — {}",
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string())
    );
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title(title),
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

// ── global settings, captured by the settings modal ───────────────────────────
/// The editable view of `[budgets]` from guardrails.toml. Seeded from disk when
/// the modal opens; written back on Save.
struct SettingsDialog {
    max_attempts: u32,
    max_iterations: u32,
    /// Whether the project manages its loop via the ACLC `[aclc]` table. When
    /// off, Save touches only `[budgets]` (legacy behaviour).
    aclc_enabled: bool,
    /// The editable ACLC control surface.
    aclc: harness_core::aclc::AclcConfig,
    /// A save error to show inline, instead of silently failing.
    error: Option<String>,
}

impl SettingsDialog {
    fn load(root: &Path) -> SettingsDialog {
        let config = harness_core::config::load_harness_config(root).unwrap_or_default();
        let guardrails = harness_core::config::load_guardrails(root).unwrap_or_default();
        let aclc = harness_core::config::resolve_aclc(&config, &guardrails);
        SettingsDialog {
            max_attempts: guardrails.budgets.max_attempts_per_task,
            max_iterations: guardrails.budgets.max_iterations,
            aclc_enabled: config.aclc_present,
            aclc,
            error: None,
        }
    }
}

// ── pending new-spec creation, captured by the modal window ───────────────────
#[derive(Default)]
struct NewSpecDialog {
    name: String,
    brief: String,
    /// When set, the brief comes from this file (`--from`) and the typed brief
    /// is ignored; clearing it returns to the text box.
    file: Option<PathBuf>,
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

    /// The open "new spec" modal, if any.
    new_spec: Option<NewSpecDialog>,

    /// The open global-settings modal, if any.
    settings: Option<SettingsDialog>,

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
            new_spec: None,
            settings: None,
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

    /// The display name for the current root — the final path component, or the
    /// full path when there isn't one (e.g. a filesystem root).
    fn root_name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// Switch the whole app to a different project directory. Any running job is
    /// dropped (its child is killed on drop), the log is cleared, and the spec
    /// list / selection / content are reloaded from the new root. The OS window
    /// title is updated to include the new folder name.
    fn set_root(&mut self, root: PathBuf, ctx: &egui::Context) {
        self.root = root;
        self.run = None;
        self.log.clear();
        self.last_exit = None;
        self.dialog = None;
        self.new_spec = None;
        self.settings = None;
        self.set_all_open = None;
        self.specs = list_specs(&self.root).unwrap_or_default();
        self.selected = self.specs.first().cloned();
        self.content = self.selected.clone().map(|s| Content::load(&self.root, &s));
        self.last_load = Instant::now();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "Harness — {}",
            self.root_name()
        )));
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
        self.new_spec_modal(ctx);
        self.settings_modal(ctx);

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
    /// Open a native folder picker (seeded at the current root). Returns the
    /// chosen folder when it differs from the current root. Blocks the UI thread
    /// while the dialog is open, which is fine for a one-shot picker.
    fn pick_new_root(&self) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new();
        if self.root.is_dir() {
            dialog = dialog.set_directory(&self.root);
        }
        dialog.pick_folder().filter(|p| *p != self.root)
    }

    fn left_panel(&mut self, ctx: &egui::Context) {
        // Deferred until after the panel closure: `change` opens a picker and
        // re-roots the app, which needs `&mut self` + `ctx` without the borrow
        // the closure holds.
        let mut new_root: Option<PathBuf> = None;
        let mut reload_requested = false;
        egui::SidePanel::left("specs")
            .resizable(false)
            .exact_width(220.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                // Single-row header. Controls are added first (right-to-left,
                // far right) so the title gets only the remaining width and
                // truncates with an ellipsis instead of pushing them off the
                // fixed-width column.
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .link("refresh")
                            .on_hover_text("Reload from disk")
                            .clicked()
                        {
                            reload_requested = true;
                        }
                        ui.colored_label(DIM, "·");
                        // Switching projects while a job runs would orphan it.
                        let running = self.is_running();
                        let change = ui.add_enabled(!running, egui::Link::new("change"));
                        if running {
                            change.on_hover_text("Stop the running job before switching projects.");
                        } else if change
                            .on_hover_text("Open a different project folder")
                            .clicked()
                        {
                            new_root = self.pick_new_root();
                        }
                        // The project (root folder) name fills the remaining
                        // space, left-aligned and truncated with "…" when long.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(self.root_name())
                                        .size(15.0)
                                        .strong()
                                        .color(PROJECT),
                                )
                                .truncate(),
                            )
                            .on_hover_text(self.root.display().to_string());
                        });
                    });
                });
                ui.separator();
                // "New spec" — pinned to the bottom of the column, below the
                // list (and still shown when there are no specs yet, so a fresh
                // project can bootstrap its first one). Disabled while a job
                // runs, since creation shells out to a CLI job and only one can
                // run at a time.
                egui::TopBottomPanel::bottom("new_spec_footer")
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        let running = self.is_running();
                        let new_link = ui.add_enabled(
                            !running,
                            egui::Link::new(RichText::new("+ new spec").color(if running {
                                DIM
                            } else {
                                ACCENT
                            })),
                        );
                        if running {
                            new_link.on_hover_text("Wait for the running job to finish.");
                        } else if new_link
                            .on_hover_text("Draft a new spec from a description or a file")
                            .clicked()
                        {
                            self.new_spec = Some(NewSpecDialog::default());
                        }
                        ui.add_space(2.0);
                        // Global run budgets live in guardrails.toml, not per
                        // spec — editable any time, since the loop only reads
                        // them at the start of a run.
                        if ui
                            .link("settings")
                            .on_hover_text("Edit global run budgets (max attempts, max iterations)")
                            .clicked()
                        {
                            self.settings = Some(SettingsDialog::load(&self.root));
                        }
                    });
                // The spec list fills the space above the footer. A justified
                // layout makes every button the full column width (with padding)
                // so they all match instead of hugging their label.
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    if self.specs.is_empty() {
                        ui.add_space(8.0);
                        ui.colored_label(DIM, "No specs under .specs/.");
                        return;
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                            let specs = self.specs.clone();
                            for spec in specs {
                                let selected = self.selected.as_deref() == Some(spec.as_str());
                                if ui.selectable_label(selected, &spec).clicked() && !selected {
                                    self.select(&spec);
                                }
                            }
                        });
                    });
                });
            });

        if let Some(root) = new_root {
            self.set_root(root, ctx);
        } else if reload_requested {
            self.reload();
        }
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

    /// The "new spec" modal: a name plus either a typed description or a picked
    /// brief file, run through `harness spec new`.
    fn new_spec_modal(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.new_spec.as_mut() else {
            return;
        };
        // rfd's picker is seeded at the project root; clone so the panel closure
        // doesn't need to borrow `self` while `dialog` is borrowed mutably.
        let root = self.root.clone();

        let mut open = true;
        let mut create = false;
        let mut cancel = false;

        let name_ok = is_valid_spec_name(dialog.name.trim());
        let has_brief = dialog.file.is_some() || !dialog.brief.trim().is_empty();
        let can_create = name_ok && has_brief;

        egui::Window::new("New spec")
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(440.0);

                ui.label("Spec name:");
                ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(f32::INFINITY)
                        .hint_text("lowercase-with-dashes, e.g. calculator"),
                );
                if !dialog.name.trim().is_empty() && !name_ok {
                    ui.colored_label(FAIL, "Name must match ^[a-z0-9][a-z0-9-]*$.");
                }
                ui.add_space(8.0);

                // Either a typed description or a brief file — never both. The
                // text box greys out once a file is chosen.
                let from_file = dialog.file.is_some();
                ui.label("Description:");
                ui.add_enabled(
                    !from_file,
                    egui::TextEdit::multiline(&mut dialog.brief)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("Describe what this spec should do…"),
                );

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Choose file…").clicked() {
                        if let Some(p) = pick_brief_file(&root) {
                            dialog.file = Some(p);
                        }
                    }
                    match &dialog.file {
                        Some(p) => {
                            ui.add(
                                egui::Label::new(RichText::new(file_label(p)).color(ACCENT))
                                    .truncate(),
                            )
                            .on_hover_text(p.display().to_string());
                            if ui.link("clear").clicked() {
                                dialog.file = None;
                            }
                        }
                        None => {
                            ui.colored_label(DIM, "or load the description from a file.");
                        }
                    }
                });

                ui.add_space(8.0);
                ui.colored_label(DIM, format!("$ harness {}", new_spec_preview(dialog)));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let create_btn = ui.add_enabled(can_create, egui::Button::new("Create"));
                    if create_btn
                        .on_hover_text(if can_create {
                            "Run `harness spec new` and stream its output below."
                        } else {
                            "Enter a valid name and a description (or pick a file) first."
                        })
                        .clicked()
                    {
                        create = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if create && can_create {
            let args = new_spec_command(dialog);
            let name = dialog.name.trim().to_string();
            self.new_spec = None;
            // Keep the new spec selected so the accordion focuses it once the
            // job finishes and the spec list reloads from disk.
            self.selected = Some(name);
            self.launch(args);
        } else if cancel || !open {
            self.new_spec = None;
        }
    }

    /// The settings modal: the global `[budgets]` values plus the ACLC loop
    /// control surface (`[aclc]`), written straight to disk — the loop reads them
    /// fresh at the next run.
    fn settings_modal(&mut self, ctx: &egui::Context) {
        use harness_core::aclc::{
            self, Learning, LoopMode, Memory, OnExhaustion, Preset, Severity, Workspace,
        };

        let Some(dialog) = self.settings.as_mut() else {
            return;
        };
        // Cloned so the save call below doesn't collide with the borrow of
        // `self.settings` that `dialog` holds.
        let root = self.root.clone();
        let warn_color = egui::Color32::from_rgb(0xd2, 0x99, 0x22);

        let mut open = true;
        let mut save = false;
        let mut cancel = false;

        // Validate up front so we can both surface findings and gate Save.
        let findings = aclc::validate(&dialog.aclc);
        let has_errors = aclc::has_errors(&findings);

        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(440.0);
                ui.heading("Run budgets");
                ui.label(
                    "Global limits for `harness build`, stored under [budgets] in \
                     .harness/guardrails/guardrails.toml.",
                );
                ui.add_space(8.0);
                egui::Grid::new("settings-grid")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("Max attempts per task (legacy)");
                        ui.add(egui::DragValue::new(&mut dialog.max_attempts).range(1..=100));
                        ui.end_row();

                        ui.label("Max iterations");
                        ui.add(egui::DragValue::new(&mut dialog.max_iterations).range(1..=100_000));
                        ui.end_row();
                    });

                ui.add_space(14.0);
                ui.separator();
                ui.heading("ACLC loop control");
                ui.checkbox(
                    &mut dialog.aclc_enabled,
                    "Manage this project's loop with ACLC ([aclc] in harness.toml)",
                );
                ui.colored_label(
                    DIM,
                    "Orthogonal axes governing whether the agent loops, what survives \
                     between attempts, and how success is decided.",
                );
                ui.add_space(8.0);

                ui.add_enabled_ui(dialog.aclc_enabled, |ui| {
                    let a = &mut dialog.aclc;
                    let looping = a.loop_mode == LoopMode::UntilPass;

                    // Preset selector — sets the defining axes, preserving oracle.
                    let current_preset = Preset::matching(a)
                        .map(|p| p.name().to_string())
                        .unwrap_or_else(|| "custom".to_string());
                    egui::Grid::new("aclc-grid")
                        .num_columns(2)
                        .spacing([12.0, 9.0])
                        .show(ui, |ui| {
                            ui.label("Preset");
                            egui::ComboBox::from_id_salt("aclc-preset")
                                .selected_text(&current_preset)
                                .show_ui(ui, |ui| {
                                    for p in Preset::all() {
                                        if ui
                                            .selectable_label(current_preset == p.name(), p.name())
                                            .clicked()
                                        {
                                            let oracle = a.oracle.clone();
                                            *a = p.config();
                                            a.oracle = oracle;
                                        }
                                    }
                                    let _ =
                                        ui.selectable_label(current_preset == "custom", "custom");
                                });
                            ui.end_row();

                            ui.label("loop");
                            egui::ComboBox::from_id_salt("aclc-loop")
                                .selected_text(if looping { "until_pass" } else { "off" })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut a.loop_mode, LoopMode::Off, "off");
                                    ui.selectable_value(
                                        &mut a.loop_mode,
                                        LoopMode::UntilPass,
                                        "until_pass",
                                    );
                                });
                            ui.end_row();

                            // workspace — applies when looping.
                            ui.add_enabled_ui(looping, |ui| ui.label("workspace"));
                            ui.add_enabled_ui(looping, |ui| {
                                egui::ComboBox::from_id_salt("aclc-ws")
                                    .selected_text(match a.workspace {
                                        Workspace::Fresh => "fresh",
                                        Workspace::Continue => "continue",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut a.workspace,
                                            Workspace::Continue,
                                            "continue",
                                        );
                                        ui.selectable_value(
                                            &mut a.workspace,
                                            Workspace::Fresh,
                                            "fresh",
                                        );
                                    });
                            });
                            ui.end_row();

                            // memory — applies when looping.
                            ui.add_enabled_ui(looping, |ui| ui.label("memory"));
                            ui.add_enabled_ui(looping, |ui| {
                                egui::ComboBox::from_id_salt("aclc-mem")
                                    .selected_text(match a.memory {
                                        Memory::Off => "off",
                                        Memory::Replace => "replace",
                                        Memory::Append => "append",
                                        Memory::Compact => "compact",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut a.memory, Memory::Off, "off");
                                        ui.selectable_value(
                                            &mut a.memory,
                                            Memory::Replace,
                                            "replace",
                                        );
                                        ui.selectable_value(
                                            &mut a.memory,
                                            Memory::Append,
                                            "append",
                                        );
                                        ui.selectable_value(
                                            &mut a.memory,
                                            Memory::Compact,
                                            "compact",
                                        );
                                    });
                            });
                            ui.end_row();

                            // learning — applies when memory != off.
                            let learning_on = a.memory != Memory::Off;
                            ui.add_enabled_ui(learning_on, |ui| ui.label("learning"));
                            ui.add_enabled_ui(learning_on, |ui| {
                                egui::ComboBox::from_id_salt("aclc-learn")
                                    .selected_text(match a.learning {
                                        Learning::Raw => "raw",
                                        Learning::Reflection => "reflection",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut a.learning,
                                            Learning::Reflection,
                                            "reflection",
                                        );
                                        ui.selectable_value(&mut a.learning, Learning::Raw, "raw");
                                    });
                            });
                            ui.end_row();

                            // memory_cap — applies when memory == compact.
                            let cap_on = a.memory == Memory::Compact;
                            ui.add_enabled_ui(cap_on, |ui| ui.label("memory_cap"));
                            ui.add_enabled_ui(cap_on, |ui| {
                                ui.add(egui::DragValue::new(&mut a.memory_cap).range(1..=100))
                            });
                            ui.end_row();

                            // max_attempts — applies when looping.
                            ui.add_enabled_ui(looping, |ui| ui.label("max_attempts"));
                            ui.add_enabled_ui(looping, |ui| {
                                ui.add(egui::DragValue::new(&mut a.max_attempts).range(1..=100))
                            });
                            ui.end_row();

                            // on_exhaustion — applies when looping.
                            ui.add_enabled_ui(looping, |ui| ui.label("on_exhaustion"));
                            ui.add_enabled_ui(looping, |ui| {
                                egui::ComboBox::from_id_salt("aclc-exh")
                                    .selected_text(match a.on_exhaustion {
                                        OnExhaustion::KeepBest => "keep_best",
                                        OnExhaustion::KeepLast => "keep_last",
                                        OnExhaustion::Clean => "clean",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut a.on_exhaustion,
                                            OnExhaustion::KeepBest,
                                            "keep_best",
                                        );
                                        ui.selectable_value(
                                            &mut a.on_exhaustion,
                                            OnExhaustion::KeepLast,
                                            "keep_last",
                                        );
                                        ui.selectable_value(
                                            &mut a.on_exhaustion,
                                            OnExhaustion::Clean,
                                            "clean",
                                        );
                                    });
                            });
                            ui.end_row();

                            // oracle — applies when looping.
                            ui.add_enabled_ui(looping, |ui| ui.label("oracle.command"));
                            ui.add_enabled_ui(looping, |ui| {
                                let mut cmd = a.oracle.command.clone().unwrap_or_default();
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut cmd)
                                        .hint_text("e.g. harness eval run <spec>")
                                        .desired_width(220.0),
                                );
                                if resp.changed() {
                                    a.oracle.command = if cmd.trim().is_empty() {
                                        None
                                    } else {
                                        Some(cmd)
                                    };
                                }
                            });
                            ui.end_row();

                            ui.add_enabled_ui(looping, |ui| ui.label("oracle.protected"));
                            ui.add_enabled_ui(looping, |ui| {
                                ui.checkbox(&mut a.oracle.protected, "")
                            });
                            ui.end_row();
                        });
                });

                // Live validation output (§6).
                if dialog.aclc_enabled && !findings.is_empty() {
                    ui.add_space(8.0);
                    for f in &findings {
                        let (color, tag) = match f.severity {
                            Severity::Error => (FAIL, "error"),
                            Severity::Warning => (warn_color, "warning"),
                        };
                        ui.colored_label(
                            color,
                            format!("{tag} [{}]: {}", f.fields.join(", "), f.message),
                        );
                    }
                }

                if let Some(err) = &dialog.error {
                    ui.add_space(6.0);
                    ui.colored_label(FAIL, err);
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let save_block = dialog.aclc_enabled && has_errors;
                    if ui
                        .add_enabled(!save_block, egui::Button::new("Save"))
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if save_block {
                        ui.colored_label(FAIL, "fix errors before saving");
                    }
                });
            });

        // Resolve the action after the panel closure so the mutable borrow of
        // `self.settings` (via `dialog`) is the only one live during the save.
        let mut done = cancel || !open;
        if save {
            let mut result = harness_core::config::save_guardrail_budgets(
                &root,
                dialog.max_attempts,
                dialog.max_iterations,
            );
            if result.is_ok() && dialog.aclc_enabled {
                result = harness_core::config::save_aclc_config(&root, &dialog.aclc);
            }
            match result {
                Ok(()) => done = true,
                Err(e) => dialog.error = Some(format!("Failed to save: {e}")),
            }
        }
        if done {
            self.settings = None;
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

/// The `spec new` args for the dialog. A picked file (`--from`) wins over the
/// typed description (`--brief`); the name is trimmed.
fn new_spec_command(dialog: &NewSpecDialog) -> Vec<String> {
    let mut a = vec!["spec".into(), "new".into(), dialog.name.trim().to_string()];
    if let Some(path) = &dialog.file {
        a.push("--from".into());
        a.push(path.display().to_string());
    } else {
        a.push("--brief".into());
        a.push(dialog.brief.trim().to_string());
    }
    a
}

/// A human-readable preview of the `spec new` command the modal will run, with
/// the brief collapsed to a single quoted, truncated line.
fn new_spec_preview(dialog: &NewSpecDialog) -> String {
    let name = match dialog.name.trim() {
        "" => "<name>",
        n => n,
    };
    if let Some(path) = &dialog.file {
        format!("spec new {name} --from {}", path.display())
    } else {
        let brief = dialog
            .brief
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if brief.is_empty() {
            format!("spec new {name} --brief …")
        } else {
            format!("spec new {name} --brief \"{}\"", ellipsize(&brief, 48))
        }
    }
}

/// Mirror of the CLI's `validate_spec_name`, plus a non-empty check, so the
/// modal can gate the Create button before shelling out.
fn is_valid_spec_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Open a native file picker (seeded at the project root) for a brief file.
/// Blocks the UI thread while open, which is fine for a one-shot picker.
fn pick_brief_file(start: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().add_filter("text", &["md", "txt"]);
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_file()
}

/// The file name of a path for display, falling back to the full path.
fn file_label(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Truncate `s` to at most `max` characters, appending an ellipsis when cut.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
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
