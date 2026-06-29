use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Blocked,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub spec: String,
    pub title: String,
    #[serde(default)]
    pub requirements: Vec<String>,
    pub status: TaskStatus,
    pub priority: i64,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub files_hint: Vec<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub notes: Option<String>,
    /// Managed by the harness. Captures the failing gate output (or agent error)
    /// from the most recent failed attempt, so the retry prompt can show the
    /// agent exactly what went wrong. Cleared when the task succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    /// Optional task-specific phase sequence. When non-empty, overrides
    /// `[loop].phase_sequence` from harness.toml for this task only.
    #[serde(default)]
    pub phases: Vec<String>,
    /// Managed by the harness. Records which phases have passed for this task.
    /// Reset to [] when the task is reset to Todo after a full failure.
    #[serde(default)]
    pub completed_phases: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_max_attempts() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    #[serde(rename = "type", default)]
    pub type_: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub precondition: Option<String>,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub feature: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub derived_from: Vec<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementsFile {
    pub spec: String,
    pub version: String,
    #[serde(default)]
    pub introduction: Option<String>,
    #[serde(default)]
    pub glossary: HashMap<String, String>,
    pub requirements: Vec<Requirement>,
    /// Glob patterns (relative to project root) for files this spec owns.
    /// These are the only files the agent may write during a normal run
    /// and the only files that are deleted/rebuilt during `harness regen`.
    #[serde(default)]
    pub owns: Vec<String>,
    /// Stability tier: "weekly" | "monthly" | "yearly" | "never".
    /// `harness regen` refuses to burn a "never" layer without --force-boundary.
    #[serde(default)]
    pub pace_layer: Option<String>,
    /// When true, regeneration requires an independent cross-model review pass
    /// before the result is accepted (Phase 5).
    #[serde(default)]
    pub public_interface: bool,
}

pub fn spec_dir(root: &Path, spec_name: &str) -> PathBuf {
    root.join(".specs").join(spec_name)
}

pub fn list_specs(root: &Path) -> Result<Vec<String>> {
    let specs_dir = root.join(".specs");
    if !specs_dir.exists() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&specs_dir)
        .with_context(|| format!("reading .specs dir at {:?}", specs_dir))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

pub fn load_tasks(spec_dir: &Path) -> Result<Vec<Task>> {
    let path = spec_dir.join("3-tasks.jsonl");
    let file =
        std::fs::File::open(&path).with_context(|| format!("opening tasks file {:?}", path))?;
    let reader = std::io::BufReader::new(file);
    let mut tasks = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let task: Task = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing task on line {} of {:?}", i + 1, path))?;
        tasks.push(task);
    }
    Ok(tasks)
}

pub fn save_tasks(spec_dir: &Path, tasks: &[Task]) -> Result<()> {
    let path = spec_dir.join("3-tasks.jsonl");
    let mut buf = String::new();
    for task in tasks {
        let line =
            serde_json::to_string(task).with_context(|| format!("serializing task {}", task.id))?;
        buf.push_str(&line);
        buf.push('\n');
    }
    // Atomic write: this file is rewritten after every iteration, so a
    // truncate-then-write would leave a constant window where Ctrl-C corrupts
    // the entire task list.
    crate::util::atomic_write_str(&path, &buf)
}

pub fn load_requirements(spec_dir: &Path) -> Result<RequirementsFile> {
    let path = spec_dir.join("1-requirements.json");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading requirements file {:?}", path))?;
    let reqs: RequirementsFile = serde_json::from_str(&content)
        .with_context(|| format!("parsing requirements file {:?}", path))?;
    Ok(reqs)
}

#[allow(dead_code)] // public selection helper; the loop uses a cross-spec variant
pub fn select_next_task(tasks: &[Task]) -> Option<usize> {
    let done_ids: std::collections::HashSet<&str> = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Done))
        .map(|t| t.id.as_str())
        .collect();

    let mut best: Option<(i64, usize)> = None;

    for (i, task) in tasks.iter().enumerate() {
        if !matches!(task.status, TaskStatus::Todo) {
            continue;
        }
        let deps_satisfied = task
            .depends_on
            .iter()
            .all(|dep| done_ids.contains(dep.as_str()));
        if !deps_satisfied {
            continue;
        }
        match best {
            None => best = Some((task.priority, i)),
            Some((best_priority, _)) if task.priority < best_priority => {
                best = Some((task.priority, i));
            }
            _ => {}
        }
    }

    best.map(|(_, i)| i)
}
