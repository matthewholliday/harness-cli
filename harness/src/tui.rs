//! `harness watch` — a live, read-only terminal dashboard for a Ralph-loop run.
//!
//! The loop persists everything it does to disk as it goes (`.harness/logs/state.json`,
//! `.harness/logs/iterations/*.json`, `.harness/logs/progress.md`, and the per-spec
//! `3-tasks.jsonl`). This view never touches the loop — it just re-reads those files
//! a few times a second and paints them, so you can run it in a second terminal
//! alongside `harness run`, or open it after the fact to replay a finished run.

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Gauge, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::config::load_harness_config;
use crate::spec::{list_specs, load_tasks, spec_dir, Task, TaskStatus};
use crate::state::{load_state, IterationRecord, LoopState};

/// How often we re-read the on-disk state.
const POLL_INTERVAL: Duration = Duration::from_millis(400);
/// A run is considered "live" if any tracked file changed this recently.
const LIVE_WINDOW: Duration = Duration::from_secs(8);
/// How many recent iteration records to keep in the timeline.
const RECENT_ITERS: usize = 12;
/// How many trailing lines of progress.md to show.
const PROGRESS_TAIL: usize = 200;

// ── colours ───────────────────────────────────────────────────────────────────

const ACCENT: Color = Color::Cyan;
const OK: Color = Color::Green;
const FAIL: Color = Color::Red;
const WARN: Color = Color::Yellow;
const DIM: Color = Color::DarkGray;

fn status_color(s: &TaskStatus) -> Color {
    match s {
        TaskStatus::Done => OK,
        TaskStatus::InProgress => ACCENT,
        TaskStatus::Blocked => FAIL,
        TaskStatus::Todo => DIM,
    }
}

fn status_glyph(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Done => "✓ done",
        TaskStatus::InProgress => "▶ active",
        TaskStatus::Blocked => "✗ blocked",
        TaskStatus::Todo => "· todo",
    }
}

// ── snapshot ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Counts {
    todo: usize,
    in_progress: usize,
    blocked: usize,
    done: usize,
}

impl Counts {
    fn total(&self) -> usize {
        self.todo + self.in_progress + self.blocked + self.done
    }
}

/// One self-contained read of everything the dashboard renders.
struct Snapshot {
    state: LoopState,
    tasks: Vec<Task>,
    counts: Counts,
    phase_sequence: Vec<String>,
    budget: u64,
    recent: Vec<IterationRecord>,
    progress_tail: Vec<String>,
    last_activity: Option<SystemTime>,
}

impl Snapshot {
    fn load(root: &Path) -> Self {
        let state = load_state(root).unwrap_or_default();
        let config = load_harness_config(root).unwrap_or_default();

        let mut tasks = Vec::new();
        for spec in list_specs(root).unwrap_or_default() {
            if let Ok(ts) = load_tasks(&spec_dir(root, &spec)) {
                tasks.extend(ts);
            }
        }
        // Stable order: in-progress first, then by priority, then id.
        tasks.sort_by(|a, b| {
            let rank = |t: &Task| match t.status {
                TaskStatus::InProgress => 0,
                TaskStatus::Blocked => 1,
                TaskStatus::Todo => 2,
                TaskStatus::Done => 3,
            };
            rank(a)
                .cmp(&rank(b))
                .then(a.priority.cmp(&b.priority))
                .then(a.id.cmp(&b.id))
        });

        let mut counts = Counts::default();
        for t in &tasks {
            match t.status {
                TaskStatus::Todo => counts.todo += 1,
                TaskStatus::InProgress => counts.in_progress += 1,
                TaskStatus::Blocked => counts.blocked += 1,
                TaskStatus::Done => counts.done += 1,
            }
        }

        let budget = config.loop_config.max_iterations as u64;
        let recent = load_recent_iterations(root, RECENT_ITERS);
        let progress_tail = load_progress_tail(root, PROGRESS_TAIL);
        let last_activity = newest_mtime(root);

        Snapshot {
            state,
            tasks,
            counts,
            phase_sequence: config.loop_config.phase_sequence,
            budget,
            recent,
            progress_tail,
            last_activity,
        }
    }

    /// Whether the loop appears to be actively running right now.
    fn is_live(&self) -> bool {
        if self.counts.in_progress > 0 {
            return true;
        }
        match self.last_activity {
            Some(t) => t.elapsed().map(|e| e < LIVE_WINDOW).unwrap_or(false),
            None => false,
        }
    }
}

fn iterations_dir(root: &Path) -> PathBuf {
    root.join(".harness").join("logs").join("iterations")
}

fn load_recent_iterations(root: &Path, n: usize) -> Vec<IterationRecord> {
    let dir = iterations_dir(root);
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect(),
        Err(_) => return Vec::new(),
    };
    files.sort(); // timestamp-prefixed filenames sort chronologically
    files
        .iter()
        .rev()
        .take(n)
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|s| serde_json::from_str::<IterationRecord>(&s).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev() // oldest → newest for display
        .collect()
}

fn load_progress_tail(root: &Path, n: usize) -> Vec<String> {
    let path = root.join(".harness").join("logs").join("progress.md");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// Newest modification time across the files the loop writes, used to tell
/// whether a run is currently active.
fn newest_mtime(root: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut consider = |p: PathBuf| {
        if let Ok(m) = std::fs::metadata(&p).and_then(|md| md.modified()) {
            newest = Some(match newest {
                Some(cur) if cur >= m => cur,
                _ => m,
            });
        }
    };
    consider(root.join(".harness").join("logs").join("state.json"));
    consider(root.join(".harness").join("logs").join("progress.md"));
    if let Ok(rd) = std::fs::read_dir(iterations_dir(root)) {
        for e in rd.flatten() {
            consider(e.path());
        }
    }
    newest
}

// ── app state ───────────────────────────────────────────────────────────────

struct App {
    root: PathBuf,
    snap: Snapshot,
    table: TableState,
    last_poll: Instant,
}

impl App {
    fn new(root: PathBuf) -> Self {
        let snap = Snapshot::load(&root);
        let mut table = TableState::default();
        // Default selection: the active task if there is one, else the first row.
        let initial = snap
            .tasks
            .iter()
            .position(|t| t.status == TaskStatus::InProgress)
            .or(if snap.tasks.is_empty() { None } else { Some(0) });
        table.select(initial);
        App {
            root,
            snap,
            table,
            last_poll: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        self.snap = Snapshot::load(&self.root);
        let len = self.snap.tasks.len();
        match self.table.selected() {
            Some(i) if i >= len => self.table.select(len.checked_sub(1)),
            None if len > 0 => self.table.select(Some(0)),
            _ => {}
        }
        self.last_poll = Instant::now();
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.snap.tasks.len();
        if len == 0 {
            return;
        }
        let cur = self.table.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, len as isize - 1);
        self.table.select(Some(next as usize));
    }

    fn selected_task(&self) -> Option<&Task> {
        self.table.selected().and_then(|i| self.snap.tasks.get(i))
    }

    /// The most recent iteration record for the selected task, if any.
    fn selected_iteration(&self) -> Option<&IterationRecord> {
        let task = self.selected_task()?;
        self.snap
            .recent
            .iter()
            .rev()
            .find(|r| r.task_id == task.id)
    }
}

// ── entry point ─────────────────────────────────────────────────────────────

/// Run the dashboard until the user quits. Returns a process exit code.
pub fn run(root: &Path) -> Result<i32> {
    let mut term = setup_terminal()?;
    let mut app = App::new(root.to_path_buf());
    let res = event_loop(&mut term, &mut app);
    restore_terminal(&mut term)?;
    res.map(|_| 0)
}

fn event_loop(term: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        term.draw(|f| draw(f, app))?;

        // Block for input up to the poll interval, then refresh from disk.
        if event::poll(POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('c');
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    _ if ctrl_c => break,
                    KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
                    KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
                    KeyCode::Char('g') => app.table.select(Some(0)),
                    KeyCode::Char('G') => {
                        let len = app.snap.tasks.len();
                        if len > 0 {
                            app.table.select(Some(len - 1));
                        }
                    }
                    KeyCode::Char('r') => app.refresh(),
                    _ => {}
                }
            }
        }

        if app.last_poll.elapsed() >= POLL_INTERVAL {
            app.refresh();
        }
    }
    Ok(())
}

// ── layout / rendering ──────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(3), // stats gauge
            Constraint::Min(8),    // body: tasks | detail
            Constraint::Length(8), // progress log
            Constraint::Length(1), // help bar
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_stats(f, chunks[1], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(chunks[2]);
    draw_tasks(f, body[0], app);
    draw_detail(f, body[1], app);

    draw_progress(f, chunks[3], app);
    draw_help(f, chunks[4]);
}

fn block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.snap;
    let live = s.is_live();
    let (dot, label, color) = if live {
        ("●", "RUNNING", OK)
    } else if s.counts.blocked > 0 && s.counts.todo == 0 && s.counts.in_progress == 0 {
        ("■", "STOPPED (blocked)", FAIL)
    } else if s.counts.total() > 0 && s.counts.done == s.counts.total() {
        ("✓", "COMPLETE", OK)
    } else {
        ("○", "IDLE", DIM)
    };

    let spec = s.state.active_spec.clone().unwrap_or_else(|| "—".into());
    let elapsed = s
        .state
        .run_start
        .map(|start| fmt_duration((Utc::now() - start).num_seconds().max(0) as u64))
        .unwrap_or_else(|| "—".into());

    let phases = if s.phase_sequence.is_empty() {
        "single-phase".to_string()
    } else {
        s.phase_sequence.join(" → ")
    };

    let line = Line::from(vec![
        Span::styled(format!("{dot} "), Style::default().fg(color)),
        Span::styled(
            format!("{label}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   spec ", Style::default().fg(DIM)),
        Span::styled(spec, Style::default().fg(Color::White)),
        Span::styled("   iter ", Style::default().fg(DIM)),
        Span::styled(
            format!("{}/{}", s.state.iteration_count, s.budget),
            Style::default().fg(Color::White),
        ),
        Span::styled("   elapsed ", Style::default().fg(DIM)),
        Span::styled(elapsed, Style::default().fg(Color::White)),
        Span::styled("   phases ", Style::default().fg(DIM)),
        Span::styled(phases, Style::default().fg(WARN)),
    ]);

    let p = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if live { OK } else { DIM }))
            .title(Span::styled(
                " harness ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(p, area);
}

fn draw_stats(f: &mut Frame, area: Rect, app: &App) {
    let c = &app.snap.counts;
    let total = c.total().max(1);
    let ratio = c.done as f64 / total as f64;
    let label = format!(
        "{} done · {} active · {} todo · {} blocked   ({}/{})",
        c.done,
        c.in_progress,
        c.todo,
        c.blocked,
        c.done,
        c.total()
    );
    let gauge = Gauge::default()
        .block(block("progress"))
        .gauge_style(Style::default().fg(if c.blocked > 0 { WARN } else { OK }))
        .ratio(ratio)
        .label(label);
    f.render_widget(gauge, area);
}

fn draw_tasks(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.snap;
    let phased = !s.phase_sequence.is_empty();

    let rows = s.tasks.iter().map(|t| {
        let sc = status_color(&t.status);
        let attempts = if t.attempts > 0 {
            format!("{}/{}", t.attempts, t.max_attempts)
        } else {
            "—".to_string()
        };
        let phase_cell = if phased {
            let seq = if t.phases.is_empty() {
                &s.phase_sequence
            } else {
                &t.phases
            };
            let done = seq.iter().filter(|p| t.completed_phases.contains(p)).count();
            format!("{}/{}", done, seq.len())
        } else {
            "—".to_string()
        };
        Row::new(vec![
            Cell::from(Span::styled(
                status_glyph(&t.status),
                Style::default().fg(sc).add_modifier(
                    if t.status == TaskStatus::InProgress {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    },
                ),
            )),
            Cell::from(t.id.clone()),
            Cell::from(truncate(&t.title, 40)),
            Cell::from(attempts),
            Cell::from(phase_cell),
        ])
    });

    let header = Row::new(vec!["status", "id", "title", "att", "ph"])
        .style(Style::default().fg(DIM).add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(20),
            Constraint::Length(5),
            Constraint::Length(4),
        ],
    )
    .header(header)
    .block(block(&format!("tasks ({})", s.tasks.len())))
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(40, 44, 52))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▌");

    let mut state = app.table.clone();
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    match app.selected_task() {
        None => {
            lines.push(Line::from(Span::styled(
                "No tasks found under .specs/.",
                Style::default().fg(DIM),
            )));
        }
        Some(t) => {
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", t.id), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(status_glyph(&t.status), Style::default().fg(status_color(&t.status))),
            ]));
            lines.push(Line::from(Span::styled(
                t.title.clone(),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(vec![
                Span::styled("spec ", Style::default().fg(DIM)),
                Span::raw(t.spec.clone()),
                Span::styled("   priority ", Style::default().fg(DIM)),
                Span::raw(t.priority.to_string()),
            ]));
            if !t.depends_on.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("deps ", Style::default().fg(DIM)),
                    Span::raw(t.depends_on.join(", ")),
                ]));
            }
            if !app.snap.phase_sequence.is_empty() {
                let seq = if t.phases.is_empty() {
                    &app.snap.phase_sequence
                } else {
                    &t.phases
                };
                let spans: Vec<Span> = seq
                    .iter()
                    .flat_map(|p| {
                        let done = t.completed_phases.contains(p);
                        vec![
                            Span::styled(
                                if done { format!("{p}✓ ") } else { format!("{p} ") },
                                Style::default().fg(if done { OK } else { DIM }),
                            ),
                        ]
                    })
                    .collect();
                let mut l = vec![Span::styled("phases ", Style::default().fg(DIM))];
                l.extend(spans);
                lines.push(Line::from(l));
            }

            // Latest iteration for this task: hook outcomes.
            lines.push(Line::from(""));
            match app.selected_iteration() {
                None => lines.push(Line::from(Span::styled(
                    "no iteration recorded yet for this task",
                    Style::default().fg(DIM),
                ))),
                Some(rec) => {
                    let agent_ok = rec.agent_exit_status == 0;
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("iter {} ", rec.iteration),
                            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("agent ", Style::default().fg(DIM)),
                        Span::styled(
                            if agent_ok { "exit 0".to_string() } else { format!("exit {}", rec.agent_exit_status) },
                            Style::default().fg(if agent_ok { OK } else { FAIL }),
                        ),
                        Span::styled(format!("  → {}", rec.task_status_after), Style::default().fg(DIM)),
                    ]));
                    if let Some(p) = &rec.phase {
                        lines.push(Line::from(vec![
                            Span::styled("phase ", Style::default().fg(DIM)),
                            Span::raw(p.clone()),
                        ]));
                    }
                    for h in &rec.hook_results {
                        let (glyph, col) = if h.passed {
                            ("✓", OK)
                        } else if h.blocking {
                            ("✗", FAIL)
                        } else {
                            ("•", WARN)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!("  {glyph} "), Style::default().fg(col)),
                            Span::styled(format!("{:18}", truncate(&h.name, 18)), Style::default().fg(Color::White)),
                            Span::styled(
                                format!("exit {:<4} {}ms", h.exit_code, h.duration_ms),
                                Style::default().fg(DIM),
                            ),
                            if h.blocking {
                                Span::raw("")
                            } else {
                                Span::styled(" (non-blocking)", Style::default().fg(DIM))
                            },
                        ]));
                    }
                    if let Some(sha) = &rec.git_commit_sha {
                        lines.push(Line::from(vec![
                            Span::styled("commit ", Style::default().fg(DIM)),
                            Span::styled(truncate(sha, 12), Style::default().fg(OK)),
                        ]));
                    }
                }
            }

            // Last failure note, if any.
            if let Some(notes) = &t.notes {
                if let Some(last) = notes.lines().last().filter(|l| !l.is_empty()) {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        truncate(last, 80),
                        Style::default().fg(if t.status == TaskStatus::Blocked { FAIL } else { DIM }),
                    )));
                }
            }
        }
    }

    let p = Paragraph::new(lines)
        .block(block("task detail"))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn draw_progress(f: &mut Frame, area: Rect, app: &App) {
    let tail = &app.snap.progress_tail;
    // Show only the lines that fit, newest at the bottom.
    let inner_h = area.height.saturating_sub(2) as usize;
    let start = tail.len().saturating_sub(inner_h);
    let lines: Vec<Line> = tail[start..]
        .iter()
        .map(|l| {
            let color = if l.contains("DONE") || l.contains("✓") {
                OK
            } else if l.contains("BLOCKED") || l.contains("failed") {
                FAIL
            } else if l.contains("retry") || l.contains("reset") {
                WARN
            } else {
                Color::Gray
            };
            Line::from(Span::styled(l.clone(), Style::default().fg(color)))
        })
        .collect();
    let p = Paragraph::new(lines).block(block("progress log"));
    f.render_widget(p, area);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" ↑/↓ ", Style::default().fg(ACCENT)),
        Span::styled("select  ", Style::default().fg(DIM)),
        Span::styled("g/G ", Style::default().fg(ACCENT)),
        Span::styled("top/bottom  ", Style::default().fg(DIM)),
        Span::styled("r ", Style::default().fg(ACCENT)),
        Span::styled("refresh  ", Style::default().fg(DIM)),
        Span::styled("q ", Style::default().fg(ACCENT)),
        Span::styled("quit  ", Style::default().fg(DIM)),
        Span::styled("· auto-refreshes every 0.4s", Style::default().fg(DIM)),
    ]);
    f.render_widget(Paragraph::new(line).alignment(Alignment::Left), area);
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

fn fmt_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

// ── terminal setup/teardown ─────────────────────────────────────────────────

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let term = Terminal::new(backend).context("failed to create terminal")?;
    Ok(term)
}

fn restore_terminal(term: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    io::stdout().execute(LeaveAlternateScreen).ok();
    term.show_cursor().ok();
    Ok(())
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    /// Build a throwaway harness project on disk so the loaders have real files
    /// to read, then render one frame into an off-screen buffer and assert the
    /// data made it onto the screen. Exercises load → snapshot → draw end to end.
    fn fixture_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("harness-tui-test-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let logs = dir.join(".harness").join("logs");
        std::fs::create_dir_all(logs.join("iterations")).unwrap();
        let spec = dir.join(".specs").join("demo");
        std::fs::create_dir_all(&spec).unwrap();

        std::fs::write(
            logs.join("state.json"),
            r#"{"active_spec":"demo","iteration_count":3,"last_task_id":"T-002","last_task_status":"done","run_start":"2026-06-22T12:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            logs.join("progress.md"),
            "# Progress\n\n- [t] iter 0: task T-001 DONE — first\n- [t] iter 1: task T-002 BLOCKED — run_build (exit 1)\n",
        )
        .unwrap();
        std::fs::write(
            logs.join("iterations").join("20260622T120100Z-1.json"),
            r#"{"iteration":1,"task_id":"T-002","spec_name":"demo","phase":null,"prompt_hash":"abc","agent_exit_status":0,"hook_results":[{"name":"run_build","exit_code":1,"duration_ms":1200,"blocking":true,"passed":false,"truncated_output":"boom","full_log_path":""}],"git_commit_sha":null,"task_status_after":"blocked","timestamp":"2026-06-22T12:01:00Z"}"#,
        )
        .unwrap();

        let t1 = r#"{"id":"T-001","spec":"demo","title":"First task","status":"done","priority":1,"depends_on":[],"created_at":"2026-06-22T11:00:00Z","updated_at":"2026-06-22T12:00:00Z"}"#;
        let t2 = r#"{"id":"T-002","spec":"demo","title":"Second task that is blocked","status":"blocked","priority":2,"attempts":3,"max_attempts":3,"notes":"[iter 1] failed: run_build (exit 1)","created_at":"2026-06-22T11:00:00Z","updated_at":"2026-06-22T12:01:00Z"}"#;
        let t3 = r#"{"id":"T-003","spec":"demo","title":"Third task in progress","status":"inprogress","priority":3,"depends_on":[],"created_at":"2026-06-22T11:00:00Z","updated_at":"2026-06-22T12:02:00Z"}"#;
        std::fs::write(spec.join("3-tasks.jsonl"), format!("{t1}\n{t2}\n{t3}\n")).unwrap();

        dir
    }

    #[test]
    fn snapshot_counts_and_ordering() {
        let root = fixture_root("counts");
        let snap = Snapshot::load(&root);
        assert_eq!(snap.counts.done, 1);
        assert_eq!(snap.counts.blocked, 1);
        assert_eq!(snap.counts.in_progress, 1);
        assert_eq!(snap.counts.total(), 3);
        // In-progress task is sorted to the top.
        assert_eq!(snap.tasks.first().unwrap().id, "T-003");
        // The blocked task's most recent iteration is recoverable.
        assert_eq!(snap.recent.len(), 1);
        assert_eq!(snap.state.iteration_count, 3);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Not a real test — `cargo test -- --ignored --nocapture dump_frame`
    /// prints a rendered frame so you can eyeball the layout.
    #[test]
    #[ignore]
    fn dump_frame() {
        let root = fixture_root("dump");
        let mut app = App::new(root.clone());
        app.table.select(Some(1)); // select the blocked task to show hook detail
        let backend = TestBackend::new(110, 28);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let w = buf.area().width as usize;
        let mut out = String::new();
        for (i, c) in buf.content().iter().enumerate() {
            out.push_str(c.symbol());
            if (i + 1) % w == 0 {
                out.push('\n');
            }
        }
        println!("\n{out}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn renders_a_frame_with_key_data() {
        let root = fixture_root("render");
        let app = App::new(root.clone());
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();

        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();

        assert!(text.contains("harness"), "header title missing");
        assert!(text.contains("T-003"), "task id missing");
        assert!(text.contains("RUNNING"), "live status missing (in_progress task present)");
        assert!(text.contains("run_build"), "hook name missing from detail");
        std::fs::remove_dir_all(&root).ok();
    }
}
