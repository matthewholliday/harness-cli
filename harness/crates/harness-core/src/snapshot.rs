//! A UI-agnostic read model of a Ralph-loop run.
//!
//! The loop persists everything it does to disk as it goes
//! (`.harness/logs/state.json`, `.harness/logs/iterations/*.json`,
//! `.harness/logs/progress.md`, and the per-spec `3-tasks.jsonl`). A [`Snapshot`]
//! is one self-contained read of all of that. It touches nothing — front-ends
//! poll it a few times a second to paint a live view, or load it once to replay a
//! finished run. Both the terminal dashboard (`harness watch`) and the desktop
//! GUI render from this same type.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::config::{load_guardrails, load_harness_config};
use crate::spec::{list_specs, load_tasks, spec_dir, Task, TaskStatus};
use crate::state::{load_state, IterationRecord, LoopState};

/// A run is considered "live" if any tracked file changed this recently.
pub const LIVE_WINDOW: Duration = Duration::from_secs(8);
/// How many recent iteration records to keep in the timeline.
pub const RECENT_ITERS: usize = 12;
/// How many trailing lines of progress.md to show.
pub const PROGRESS_TAIL: usize = 200;

#[derive(Default, Clone)]
pub struct Counts {
    pub todo: usize,
    pub in_progress: usize,
    pub blocked: usize,
    pub done: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.todo + self.in_progress + self.blocked + self.done
    }

    /// Completion ratio in `0.0..=1.0` (0 when there are no tasks).
    pub fn ratio(&self) -> f32 {
        let total = self.total();
        if total == 0 {
            0.0
        } else {
            self.done as f32 / total as f32
        }
    }
}

/// One self-contained read of everything a dashboard renders.
pub struct Snapshot {
    pub state: LoopState,
    pub tasks: Vec<Task>,
    pub counts: Counts,
    pub phase_sequence: Vec<String>,
    pub budget: u64,
    /// Global per-task retry cap (`[budgets].max_attempts_per_task`).
    pub max_attempts: u32,
    pub recent: Vec<IterationRecord>,
    pub progress_tail: Vec<String>,
    pub last_activity: Option<SystemTime>,
}

impl Snapshot {
    pub fn load(root: &Path) -> Self {
        let state = load_state(root).unwrap_or_default();
        let config = load_harness_config(root).unwrap_or_default();
        let guardrails = load_guardrails(root).unwrap_or_default();

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
            max_attempts: guardrails.budgets.max_attempts_per_task,
            recent,
            progress_tail,
            last_activity,
        }
    }

    /// Whether the loop appears to be actively running right now.
    pub fn is_live(&self) -> bool {
        if self.counts.in_progress > 0 {
            return true;
        }
        match self.last_activity {
            Some(t) => t.elapsed().map(|e| e < LIVE_WINDOW).unwrap_or(false),
            None => false,
        }
    }

    /// The most recent iteration record for a given task id, if any.
    pub fn latest_iteration_for(&self, task_id: &str) -> Option<&IterationRecord> {
        self.recent.iter().rev().find(|r| r.task_id == task_id)
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
