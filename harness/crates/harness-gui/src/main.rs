//! `harness-gui` — a desktop dashboard for a Harness Ralph-loop run.
//!
//! It renders the same on-disk state model the terminal `harness watch` view
//! uses (via [`harness_core::snapshot::Snapshot`]), and adds what a terminal
//! can't do well: launching/stopping a run, streaming live agent output, and
//! browsing specs and per-task phase progress.
//!
//! State is read from disk on a timer; runs are driven by shelling out to the
//! `harness` CLI. The GUI never owns run state — disk is the source of truth.

mod runner;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use eframe::egui;
use egui::{Color32, RichText};

use harness_core::snapshot::Snapshot;
use harness_core::spec::{load_requirements, spec_dir, RequirementsFile, Task, TaskStatus};

use runner::{RunHandle, RunOptions};

const POLL_INTERVAL: Duration = Duration::from_millis(400);

// ── colours ─────────────────────────────────────────────────────────────────
const ACCENT: Color32 = Color32::from_rgb(86, 182, 194);
const OK: Color32 = Color32::from_rgb(126, 192, 80);
const FAIL: Color32 = Color32::from_rgb(224, 108, 117);
const WARN: Color32 = Color32::from_rgb(229, 192, 123);
const DIM: Color32 = Color32::from_rgb(130, 137, 151);

fn status_color(s: &TaskStatus) -> Color32 {
    match s {
        TaskStatus::Done => OK,
        TaskStatus::InProgress => ACCENT,
        TaskStatus::Blocked => FAIL,
        TaskStatus::Todo => DIM,
    }
}

fn status_glyph(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Done => "✓",
        TaskStatus::InProgress => "▶",
        TaskStatus::Blocked => "✗",
        TaskStatus::Todo => "·",
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Detail,
    Output,
    Spec,
    Phases,
}

/// Cached spec text so we don't re-read files every frame.
struct SpecView {
    spec: String,
    requirements: Option<RequirementsFile>,
    design: String,
}

struct HarnessApp {
    root: PathBuf,
    snap: Snapshot,
    last_poll: Instant,
    selected_id: Option<String>,
    tab: Tab,

    // run controls + live output
    run: Option<RunHandle>,
    opts: RunOptions,
    log: Vec<String>,
    last_exit: Option<i32>,

    spec_cache: Option<SpecView>,
}

impl HarnessApp {
    fn new(root: PathBuf) -> Self {
        let snap = Snapshot::load(&root);
        let selected_id = snap
            .tasks
            .iter()
            .find(|t| t.status == TaskStatus::InProgress)
            .or_else(|| snap.tasks.first())
            .map(|t| t.id.clone());
        HarnessApp {
            root,
            snap,
            last_poll: Instant::now(),
            selected_id,
            tab: Tab::Detail,
            run: None,
            opts: RunOptions::default(),
            log: Vec::new(),
            last_exit: None,
            spec_cache: None,
        }
    }

    fn refresh(&mut self) {
        self.snap = Snapshot::load(&self.root);
        // Keep selection valid; default to the active task if it vanished.
        let still_present = self
            .selected_id
            .as_ref()
            .map(|id| self.snap.tasks.iter().any(|t| &t.id == id))
            .unwrap_or(false);
        if !still_present {
            self.selected_id = self
                .snap
                .tasks
                .iter()
                .find(|t| t.status == TaskStatus::InProgress)
                .or_else(|| self.snap.tasks.first())
                .map(|t| t.id.clone());
        }
        self.last_poll = Instant::now();
    }

    fn selected_task(&self) -> Option<&Task> {
        let id = self.selected_id.as_ref()?;
        self.snap.tasks.iter().find(|t| &t.id == id)
    }

    /// Best guess at the spec currently in focus.
    fn active_spec(&self) -> Option<String> {
        self.selected_task()
            .map(|t| t.spec.clone())
            .or_else(|| self.snap.state.active_spec.clone())
            .or_else(|| self.snap.tasks.first().map(|t| t.spec.clone()))
    }

    fn ensure_spec_cache(&mut self, spec: &str) {
        let fresh = self
            .spec_cache
            .as_ref()
            .map(|c| c.spec == spec)
            .unwrap_or(false);
        if fresh {
            return;
        }
        let dir = spec_dir(&self.root, spec);
        let requirements = load_requirements(&dir).ok();
        let design = std::fs::read_to_string(dir.join("2-design.md")).unwrap_or_default();
        self.spec_cache = Some(SpecView {
            spec: spec.to_string(),
            requirements,
            design,
        });
    }

    fn start_run(&mut self) {
        if self.run.as_ref().map(|r| r.is_running()).unwrap_or(false) {
            return;
        }
        self.log.clear();
        self.last_exit = None;
        match RunHandle::spawn(&self.root, &self.opts) {
            Ok(h) => {
                self.log
                    .push(format!("$ harness build  ({})", self.root.display()));
                self.run = Some(h);
                self.tab = Tab::Output;
            }
            Err(e) => {
                self.log.push(format!("failed to launch: {e}"));
            }
        }
    }

    fn stop_run(&mut self) {
        if let Some(h) = self.run.as_mut() {
            h.stop();
            self.log.push("— stopped —".to_string());
        }
    }

    /// Pull any new output and reap the child when it finishes.
    fn pump_run(&mut self) {
        let mut finished_code = None;
        if let Some(h) = self.run.as_mut() {
            let lines = h.poll();
            self.log.extend(lines);
            if !h.is_running() {
                finished_code = Some(h.exit_code());
            }
        }
        if let Some(code) = finished_code {
            self.last_exit = code;
            self.log
                .push(format!("— exited (code {}) —", code.unwrap_or(-1)));
            self.run = None;
        }
    }

    fn is_running(&self) -> bool {
        self.run.as_ref().map(|r| r.is_running()).unwrap_or(false)
    }
}

// ── header ────────────────────────────────────────────────────────────────

struct Status {
    label: &'static str,
    color: Color32,
}

fn run_status(app: &HarnessApp) -> Status {
    if app.is_running() || app.snap.is_live() {
        Status {
            label: "RUNNING",
            color: OK,
        }
    } else if app.snap.counts.total() > 0 && app.snap.counts.done == app.snap.counts.total() {
        Status {
            label: "COMPLETE",
            color: ACCENT,
        }
    } else if app.snap.counts.blocked > 0 {
        Status {
            label: "BLOCKED",
            color: FAIL,
        }
    } else {
        Status {
            label: "IDLE",
            color: DIM,
        }
    }
}

fn elapsed_str(app: &HarnessApp) -> String {
    let Some(start) = app.snap.state.run_start else {
        return "—".to_string();
    };
    let secs = (Utc::now() - start).num_seconds().max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else {
        format!("{m}m{s:02}s")
    }
}

impl eframe::App for HarnessApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Stream child output every frame; reload disk state on the poll timer.
        self.pump_run();
        if self.last_poll.elapsed() >= POLL_INTERVAL {
            self.refresh();
        }
        ctx.request_repaint_after(if self.is_running() {
            Duration::from_millis(200)
        } else {
            POLL_INTERVAL
        });

        self.header(ctx);
        self.task_list(ctx);
        self.progress_log(ctx);
        self.central(ctx);
    }
}

impl HarnessApp {
    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let st = run_status(self);
                ui.label(
                    RichText::new(format!("● {}", st.label))
                        .color(st.color)
                        .strong(),
                );
                ui.separator();
                let spec = self
                    .snap
                    .state
                    .active_spec
                    .clone()
                    .unwrap_or_else(|| "—".into());
                ui.label(RichText::new("spec").color(DIM));
                ui.label(RichText::new(spec).strong());
                ui.separator();
                ui.label(RichText::new("iter").color(DIM));
                ui.label(format!(
                    "{}/{}",
                    self.snap.state.iteration_count, self.snap.budget
                ));
                ui.separator();
                ui.label(RichText::new("elapsed").color(DIM));
                ui.label(elapsed_str(self));
                if !self.snap.phase_sequence.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("phases").color(DIM));
                    ui.label(self.snap.phase_sequence.join(" → "));
                }
            });

            ui.add_space(4.0);
            // Stats bar.
            let c = &self.snap.counts;
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(c.ratio())
                        .desired_width(260.0)
                        .text(format!("{}/{} done", c.done, c.total())),
                );
                ui.separator();
                ui.label(RichText::new(format!("✓ {}", c.done)).color(OK));
                ui.label(RichText::new(format!("▶ {}", c.in_progress)).color(ACCENT));
                ui.label(RichText::new(format!("· {}", c.todo)).color(DIM));
                ui.label(RichText::new(format!("✗ {}", c.blocked)).color(FAIL));
            });

            ui.add_space(4.0);
            // Run controls.
            ui.horizontal(|ui| {
                let running = self.is_running();
                if running {
                    if ui.button(RichText::new("■ Stop").color(FAIL)).clicked() {
                        self.stop_run();
                    }
                } else if ui.button(RichText::new("▶ Start").color(OK)).clicked() {
                    self.start_run();
                }
                ui.separator();
                ui.add_enabled_ui(!running, |ui| {
                    ui.label("spec");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.opts.spec)
                            .hint_text("all")
                            .desired_width(120.0),
                    );
                    ui.checkbox(&mut self.opts.once, "--once");
                    ui.checkbox(&mut self.opts.dry_run, "--dry-run");
                    ui.label("max");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.opts.max)
                            .hint_text("∞")
                            .desired_width(48.0),
                    );
                });
                if let Some(code) = self.last_exit {
                    ui.separator();
                    let c = if code == 0 { OK } else { FAIL };
                    ui.label(RichText::new(format!("last exit: {code}")).color(c));
                }
            });
            ui.add_space(4.0);
        });
    }

    fn task_list(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("tasks")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("TASKS").color(DIM).strong());
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Clone the minimal data we need so we don't borrow self.snap
                    // while mutating self.selected_id.
                    let rows: Vec<(String, String, TaskStatus, u32, usize, usize)> = self
                        .snap
                        .tasks
                        .iter()
                        .map(|t| {
                            (
                                t.id.clone(),
                                t.title.clone(),
                                t.status.clone(),
                                t.attempts,
                                t.completed_phases.len(),
                                t.phases.len(),
                            )
                        })
                        .collect();
                    for (id, title, status, attempts, done_ph, tot_ph) in rows {
                        let selected = self.selected_id.as_deref() == Some(id.as_str());
                        let glyph = status_glyph(&status);
                        let mut label = format!("{glyph} {id}  {title}");
                        if tot_ph > 0 {
                            label.push_str(&format!("  [{done_ph}/{tot_ph}]"));
                        }
                        if attempts > 0 {
                            label.push_str(&format!("  ×{attempts}"));
                        }
                        let text = RichText::new(label).color(status_color(&status));
                        if ui.selectable_label(selected, text).clicked() {
                            self.selected_id = Some(id.clone());
                            self.tab = Tab::Detail;
                        }
                    }
                });
            });
    }

    fn progress_log(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("progress")
            .resizable(true)
            .default_height(150.0)
            .show(ctx, |ui| {
                ui.add_space(2.0);
                ui.label(RichText::new("PROGRESS").color(DIM).strong());
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for line in &self.snap.progress_tail {
                            ui.label(RichText::new(line).color(progress_color(line)).monospace());
                        }
                    });
            });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Detail, "Detail");
                ui.selectable_value(&mut self.tab, Tab::Output, "Live output");
                ui.selectable_value(&mut self.tab, Tab::Spec, "Spec");
                ui.selectable_value(&mut self.tab, Tab::Phases, "Phases");
            });
            ui.separator();
            match self.tab {
                Tab::Detail => self.detail_tab(ui),
                Tab::Output => self.output_tab(ui),
                Tab::Spec => self.spec_tab(ui),
                Tab::Phases => self.phases_tab(ui),
            }
        });
    }

    fn detail_tab(&mut self, ui: &mut egui::Ui) {
        let Some(task) = self.selected_task().cloned() else {
            ui.label(RichText::new("No task selected.").color(DIM));
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(
                    RichText::new(format!("{}  {}", task.id, task.title))
                        .color(status_color(&task.status)),
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new("status").color(DIM));
                    ui.label(
                        RichText::new(format!("{:?}", task.status))
                            .color(status_color(&task.status)),
                    );
                    ui.label(RichText::new("  priority").color(DIM));
                    ui.label(task.priority.to_string());
                    ui.label(RichText::new("  attempts").color(DIM));
                    ui.label(format!("{}/{}", task.attempts, task.max_attempts));
                });
                if !task.depends_on.is_empty() {
                    ui.label(format!("depends on: {}", task.depends_on.join(", ")));
                }
                if !task.requirements.is_empty() {
                    ui.label(format!("requirements: {}", task.requirements.join(", ")));
                }
                if !task.phases.is_empty() {
                    ui.label(format!(
                        "phases: {} (done: {})",
                        task.phases.join(" → "),
                        task.completed_phases.join(", ")
                    ));
                }
                if let Some(note) = &task.last_failure {
                    ui.add_space(6.0);
                    ui.label(RichText::new("last failure").color(FAIL).strong());
                    ui.label(RichText::new(note).color(FAIL).monospace());
                }

                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("LATEST ITERATION").color(DIM).strong());
                match self.snap.latest_iteration_for(&task.id) {
                    None => {
                        ui.label(RichText::new("no iteration record yet").color(DIM));
                    }
                    Some(rec) => {
                        ui.horizontal(|ui| {
                            ui.label(format!("iter {}", rec.iteration));
                            let c = if rec.agent_exit_status == 0 { OK } else { FAIL };
                            ui.label(
                                RichText::new(format!("agent exit {}", rec.agent_exit_status))
                                    .color(c),
                            );
                            if let Some(sha) = &rec.git_commit_sha {
                                ui.label(
                                    RichText::new(format!("commit {}", &sha[..sha.len().min(8)]))
                                        .color(ACCENT),
                                );
                            }
                        });
                        for h in &rec.hook_results {
                            let c = if h.passed { OK } else { FAIL };
                            let mark = if h.passed { "✓" } else { "✗" };
                            ui.label(
                                RichText::new(format!(
                                    "{mark} {}  (exit {}, {}ms)",
                                    h.name, h.exit_code, h.duration_ms
                                ))
                                .color(c)
                                .monospace(),
                            );
                            if !h.passed && !h.truncated_output.is_empty() {
                                ui.label(RichText::new(&h.truncated_output).color(DIM).monospace());
                            }
                        }
                    }
                }
            });
    }

    fn output_tab(&mut self, ui: &mut egui::Ui) {
        if self.log.is_empty() {
            ui.label(
                RichText::new("No run output yet. Press Start to launch `harness build`.")
                    .color(DIM),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.log {
                    ui.label(RichText::new(line).monospace());
                }
            });
    }

    fn spec_tab(&mut self, ui: &mut egui::Ui) {
        let Some(spec) = self.active_spec() else {
            ui.label(RichText::new("No spec available.").color(DIM));
            return;
        };
        self.ensure_spec_cache(&spec);
        let Some(view) = &self.spec_cache else { return };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(&view.spec);
                ui.add_space(6.0);
                ui.label(RichText::new("REQUIREMENTS").color(DIM).strong());
                match &view.requirements {
                    None => {
                        ui.label(RichText::new("no 1-requirements.json").color(DIM));
                    }
                    Some(rf) => {
                        for r in &rf.requirements {
                            let text = r
                                .text
                                .clone()
                                .or_else(|| r.response.clone())
                                .unwrap_or_default();
                            ui.label(RichText::new(format!("• {}  {}", r.id, text)).strong());
                            for ac in &r.acceptance_criteria {
                                ui.label(RichText::new(format!("    - {ac}")).color(DIM));
                            }
                        }
                    }
                }
                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("DESIGN").color(DIM).strong());
                if view.design.is_empty() {
                    ui.label(RichText::new("no 2-design.md").color(DIM));
                } else {
                    ui.label(RichText::new(&view.design).monospace());
                }
            });
    }

    fn phases_tab(&mut self, ui: &mut egui::Ui) {
        if self.snap.phase_sequence.is_empty() {
            ui.label(RichText::new("No phase sequence configured (single-phase loop).").color(DIM));
            return;
        }
        let phases = self.snap.phase_sequence.clone();
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("phase-grid").striped(true).show(ui, |ui| {
                    ui.label(RichText::new("task").color(DIM).strong());
                    for p in &phases {
                        ui.label(RichText::new(p).color(DIM).strong());
                    }
                    ui.end_row();

                    for t in &self.snap.tasks {
                        ui.label(
                            RichText::new(format!("{} {}", status_glyph(&t.status), t.id))
                                .color(status_color(&t.status)),
                        );
                        for p in &phases {
                            let active = t.phases.contains(p);
                            let done = t.completed_phases.contains(p);
                            let (mark, color) = if !active {
                                ("–", DIM)
                            } else if done {
                                ("✓", OK)
                            } else {
                                ("·", WARN)
                            };
                            ui.label(RichText::new(mark).color(color));
                        }
                        ui.end_row();
                    }
                });
            });
    }
}

fn progress_color(line: &str) -> Color32 {
    let l = line.to_ascii_lowercase();
    if l.contains("done") || l.contains('✓') {
        OK
    } else if l.contains("blocked") || l.contains("fail") || l.contains("error") {
        FAIL
    } else if l.contains("retry") || l.contains("reset") {
        WARN
    } else {
        Color32::GRAY
    }
}

fn resolve_root() -> PathBuf {
    // Optional positional arg: a project root. Otherwise search upward like the CLI.
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    harness_core::config::find_project_root()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn main() -> eframe::Result<()> {
    let root = resolve_root();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_title("Harness"),
        ..Default::default()
    };
    eframe::run_native(
        "harness-gui",
        options,
        Box::new(move |_cc| Ok(Box::new(HarnessApp::new(root)))),
    )
}
