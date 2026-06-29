use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── PhaseConfig ───────────────────────────────────────────────────────────────

/// Configuration for a single SDLC phase (e.g. "plan", "test", "dev").
/// All fields are optional; omitted fields fall back to the harness defaults.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PhaseConfig {
    /// Override the agent command for this phase. Supports `{prompt_file}`.
    #[serde(default)]
    pub agent_command: Option<String>,
    /// Path to a phase-specific prompt template file.
    #[serde(default)]
    pub prompt_template: Option<String>,
    /// Hooks to run after the agent. If absent, falls back to task hooks then
    /// `[hooks].default`.
    #[serde(default)]
    pub hooks: Option<Vec<String>>,
}

// ── HarnessConfig ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub reviewer_command: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: "claude --print --dangerously-skip-permissions -p {prompt_file}".to_string(),
            working_dir: None,
            reviewer_command: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoopConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Commit after each successful task. Default: true. This is load-bearing,
    /// not just convenience: the protected-write and ownership guards diff the
    /// working tree against HEAD, so HEAD must advance every successful iteration
    /// for those checks to distinguish *this* iteration's writes from prior ones.
    #[serde(default = "default_commit_each_success")]
    pub commit_each_success: bool,
    #[serde(default = "default_commit_message_template")]
    pub commit_message_template: String,
    #[serde(default = "default_stop_when_no_tasks")]
    pub stop_when_no_tasks: bool,
    /// On a failed iteration, restore the working tree to the last clean commit
    /// so a broken attempt can't poison subsequent tasks. Default: true.
    #[serde(default = "default_reset_on_failure")]
    pub reset_on_failure: bool,
    /// Ordered list of phase names every task must pass through before being
    /// marked Done. Empty (the default) disables phase-based execution and
    /// restores the original single-agent-per-task behaviour.
    #[serde(default)]
    pub phase_sequence: Vec<String>,
    /// Keep at most this many iteration log JSON files. Oldest are pruned at
    /// run start. `None` (the default) means unlimited.
    #[serde(default)]
    pub max_log_files: Option<usize>,
}

fn default_reset_on_failure() -> bool {
    true
}

fn default_commit_each_success() -> bool {
    true
}

fn default_max_iterations() -> u32 {
    100
}

fn default_commit_message_template() -> String {
    "harness: complete task {task_id} ({task_title})".to_string()
}

fn default_stop_when_no_tasks() -> bool {
    true
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            commit_each_success: default_commit_each_success(),
            commit_message_template: default_commit_message_template(),
            stop_when_no_tasks: default_stop_when_no_tasks(),
            reset_on_failure: default_reset_on_failure(),
            phase_sequence: Vec::new(),
            max_log_files: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PromptsConfig {
    #[serde(rename = "loop", default = "default_loop_prompt")]
    pub loop_prompt: String,
    #[serde(default = "default_init_prompt")]
    pub init: String,
    /// Hard cap on the assembled prompt size in characters. When the composed
    /// prompt exceeds this limit the middle is trimmed and a truncation marker
    /// is inserted. `None` (the default) means unlimited.
    #[serde(default)]
    pub max_prompt_chars: Option<usize>,
}

fn default_loop_prompt() -> String {
    ".harness/prompts/loop.md".to_string()
}

fn default_init_prompt() -> String {
    ".harness/prompts/init.md".to_string()
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            loop_prompt: default_loop_prompt(),
            init: default_init_prompt(),
            max_prompt_chars: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub default: Vec<String>,
    #[serde(default = "default_hook_timeout")]
    pub default_timeout_secs: u64,
}

fn default_hook_timeout() -> u64 {
    30
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            default: Vec::new(),
            default_timeout_secs: default_hook_timeout(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct HarnessConfig {
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub loop_config: LoopConfig,
    #[serde(default)]
    pub prompts: PromptsConfig,
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Per-phase configuration, keyed by phase name (e.g. "plan", "test", "dev").
    #[serde(default)]
    pub phases: HashMap<String, PhaseConfig>,
}

// TOML uses [loop] but "loop" is a Rust keyword, so we use an intermediate raw struct.
#[derive(Debug, Deserialize)]
struct RawHarnessConfig {
    agent: Option<AgentConfig>,
    #[serde(rename = "loop")]
    loop_config: Option<LoopConfig>,
    prompts: Option<PromptsConfig>,
    hooks: Option<HooksConfig>,
    #[serde(default)]
    phases: HashMap<String, PhaseConfig>,
}

// ── GuardrailsConfig ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BudgetsConfig {
    #[serde(default = "default_max_attempts_per_task")]
    pub max_attempts_per_task: u32,
    #[serde(default = "default_guardrail_max_iterations")]
    pub max_iterations: u32,
}

fn default_max_attempts_per_task() -> u32 {
    3
}

fn default_guardrail_max_iterations() -> u32 {
    500
}

impl Default for BudgetsConfig {
    fn default() -> Self {
        Self {
            max_attempts_per_task: default_max_attempts_per_task(),
            max_iterations: default_guardrail_max_iterations(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct WritesConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OperationsConfig {
    #[serde(default = "default_deny_destructive")]
    pub deny_destructive: bool,
}

fn default_deny_destructive() -> bool {
    true
}

impl Default for OperationsConfig {
    fn default() -> Self {
        Self {
            deny_destructive: default_deny_destructive(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HookGuardrail {
    #[serde(default = "default_blocking")]
    pub blocking: bool,
    #[serde(default = "default_hook_guardrail_timeout")]
    pub timeout_secs: u64,
}

fn default_enforce_ownership() -> bool {
    true
}

fn default_blocking() -> bool {
    true
}

fn default_hook_guardrail_timeout() -> u64 {
    30
}

impl Default for HookGuardrail {
    fn default() -> Self {
        Self {
            blocking: default_blocking(),
            timeout_secs: default_hook_guardrail_timeout(),
        }
    }
}

/// Paths that are always off-limits for agent writes, regardless of other config.
/// `.specs/**`, `evals/**`, `.harness/guardrails/**`, `.harness/manifest.json`,
/// and `.git/**` are always protected; this block adds project-specific extras.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProtectedConfig {
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct GuardrailsConfig {
    #[serde(default)]
    pub budgets: BudgetsConfig,
    #[serde(default)]
    pub writes: WritesConfig,
    #[serde(default)]
    pub operations: OperationsConfig,
    /// Paths always off-limits for agent writes (in addition to the built-in set).
    #[serde(default)]
    pub protected: ProtectedConfig,
    /// Per-hook overrides keyed by hook name, from [hooks.<name>] table.
    #[serde(default)]
    pub hooks: HashMap<String, HookGuardrail>,
    /// When true and a task has non-empty files_hint, any file the agent changes
    /// that falls outside files_hint ∪ writes.allow causes the iteration to fail.
    #[serde(default = "default_enforce_ownership")]
    pub enforce_ownership: bool,
}

// ── Loaders ───────────────────────────────────────────────────────────────────

pub fn load_harness_config(root: &Path) -> Result<HarnessConfig> {
    let path = root.join(".harness").join("harness.toml");

    if !path.exists() {
        return Ok(HarnessConfig::default());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let raw: RawHarnessConfig =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(HarnessConfig {
        agent: raw.agent.unwrap_or_default(),
        loop_config: raw.loop_config.unwrap_or_default(),
        prompts: raw.prompts.unwrap_or_default(),
        hooks: raw.hooks.unwrap_or_default(),
        phases: raw.phases,
    })
}

pub fn load_guardrails(root: &Path) -> Result<GuardrailsConfig> {
    let path = root
        .join(".harness")
        .join("guardrails")
        .join("guardrails.toml");

    if !path.exists() {
        return Ok(GuardrailsConfig::default());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let config: GuardrailsConfig =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(config)
}

pub fn find_project_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to get current directory")?;

    let mut dir: &Path = &cwd;
    loop {
        if dir.join(".harness").is_dir() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => {
                anyhow::bail!(
                    "could not find .harness/ directory — run `harness init` first or cd into a harness project"
                );
            }
        }
    }
}
