use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoopState {
    pub active_spec: Option<String>,
    pub iteration_count: u64,
    pub last_task_id: Option<String>,
    pub last_task_status: Option<String>,
    pub run_start: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    pub name: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub blocking: bool,
    pub passed: bool,
    pub truncated_output: String,
    pub full_log_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationRecord {
    pub iteration: u64,
    pub task_id: String,
    pub spec_name: String,
    pub prompt_hash: String,
    pub agent_exit_status: i32,
    pub hook_results: Vec<HookResult>,
    pub git_commit_sha: Option<String>,
    pub task_status_after: String,
    pub timestamp: DateTime<Utc>,
}

impl IterationRecord {
    #[allow(dead_code)] // helper; prompt hashing currently happens in prompt::write_prompt_file
    pub fn prompt_hash_from(prompt: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(prompt.as_bytes());
        hex::encode(hasher.finalize())
    }
}

fn state_path(root: &Path) -> std::path::PathBuf {
    root.join(".harness").join("logs").join("state.json")
}

pub fn load_state(root: &Path) -> Result<LoopState> {
    let path = state_path(root);
    if !path.exists() {
        return Ok(LoopState::default());
    }
    let data = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read state file: {}", path.display()))?;
    let state: LoopState = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse state file: {}", path.display()))?;
    Ok(state)
}

pub fn save_state(root: &Path, state: &LoopState) -> Result<()> {
    let path = state_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(state).context("Failed to serialize state")?;
    fs::write(&path, data)
        .with_context(|| format!("Failed to write state file: {}", path.display()))?;
    Ok(())
}

pub fn save_iteration_record(root: &Path, record: &IterationRecord) -> Result<()> {
    let iterations_dir = root.join(".harness").join("logs").join("iterations");
    fs::create_dir_all(&iterations_dir)
        .with_context(|| format!("Failed to create iterations directory: {}", iterations_dir.display()))?;

    let ts = record.timestamp.format("%Y%m%dT%H%M%SZ").to_string();
    let filename = format!("{}-{}.json", ts, record.iteration);
    let path = iterations_dir.join(&filename);

    let data = serde_json::to_string_pretty(record)
        .context("Failed to serialize iteration record")?;
    fs::write(&path, data)
        .with_context(|| format!("Failed to write iteration record: {}", path.display()))?;
    Ok(())
}

pub fn append_progress(root: &Path, text: &str) -> Result<()> {
    let path = root.join(".harness").join("logs").join("progress.md");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open progress log: {}", path.display()))?;
    let line = if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    };
    file.write_all(line.as_bytes())
        .with_context(|| format!("Failed to write to progress log: {}", path.display()))?;
    Ok(())
}

pub fn read_progress(root: &Path) -> Result<String> {
    let path = root.join(".harness").join("logs").join("progress.md");
    if !path.exists() {
        return Ok(String::new());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read progress log: {}", path.display()))?;
    Ok(content)
}
