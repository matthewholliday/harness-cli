use crate::aclc::{AclcConfig, LoopMode, Workspace};
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
            // Pin the exact model (not a moving alias) so a model upgrade can't
            // silently change generated code out from under the spec. The pinned
            // ID is recorded in the manifest so drift is detectable.
            command: "claude --print --model claude-opus-4-8 --dangerously-skip-permissions -p {prompt_file}".to_string(),
            working_dir: None,
            reviewer_command: None,
        }
    }
}

/// Extract the model ID pinned in an agent command via `--model <id>` or
/// `--model=<id>`. Returns an empty string when no model is pinned (i.e. the
/// command relies on a server-side default alias — a determinism hazard).
pub fn model_id_from_command(cmd: &str) -> String {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        if let Some(rest) = tok.strip_prefix("--model=") {
            return rest.to_string();
        }
        if *tok == "--model" {
            if let Some(next) = tokens.get(i + 1) {
                return next.to_string();
            }
        }
    }
    String::new()
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
    /// ACLC control surface (the `[aclc]` table). When the table is absent the
    /// fields fall back to legacy `[loop]` aliases — see [`HarnessConfig::aclc`]
    /// vs. [`resolve_aclc`].
    #[serde(default)]
    pub aclc: AclcConfig,
    /// Whether `[aclc]` was explicitly present in the file. Drives legacy-alias
    /// reconciliation: when false, [`resolve_aclc`] derives the workspace axis
    /// from `reset_on_failure` and the attempt cap from guardrails.
    #[serde(skip)]
    pub aclc_present: bool,
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
    aclc: Option<AclcConfig>,
}

/// Resolve the effective ACLC configuration, reconciling the legacy `[loop]` and
/// `[budgets]` fields when no explicit `[aclc]` table is present.
///
/// The new fields always win when `[aclc]` is given. Otherwise the workspace
/// axis is derived from `reset_on_failure` (`true → fresh`, `false → continue`)
/// and the attempt cap from `guardrails.budgets.max_attempts_per_task`, so
/// existing projects keep their behaviour without an `[aclc]` block.
pub fn resolve_aclc(cfg: &HarnessConfig, guardrails: &GuardrailsConfig) -> AclcConfig {
    if cfg.aclc_present {
        return cfg.aclc.clone();
    }
    let mut a = cfg.aclc.clone();
    a.workspace = if cfg.loop_config.reset_on_failure {
        Workspace::Fresh
    } else {
        Workspace::Continue
    };
    a.max_attempts = guardrails.budgets.max_attempts_per_task;
    // Legacy projects retry tasks in place but have no success oracle wired, so
    // they remain single-pass at the ACLC layer until an `[aclc]` table opts in.
    a.loop_mode = LoopMode::Off;
    a
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

    let aclc_present = raw.aclc.is_some();
    Ok(HarnessConfig {
        agent: raw.agent.unwrap_or_default(),
        loop_config: raw.loop_config.unwrap_or_default(),
        prompts: raw.prompts.unwrap_or_default(),
        hooks: raw.hooks.unwrap_or_default(),
        phases: raw.phases,
        aclc: raw.aclc.unwrap_or_default(),
        aclc_present,
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

/// Write the global `[budgets]` settings into guardrails.toml, preserving every
/// other key, table, and comment in the file. Creates the file (and its parent
/// directory) with just a `[budgets]` table if it does not yet exist.
pub fn save_guardrail_budgets(
    root: &Path,
    max_attempts_per_task: u32,
    max_iterations: u32,
) -> Result<()> {
    use toml_edit::{value, DocumentMut, Item, Table};

    let path = root
        .join(".harness")
        .join("guardrails")
        .join("guardrails.toml");

    let mut doc = if path.exists() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        DocumentMut::new()
    };

    if !doc.contains_key("budgets") {
        doc["budgets"] = Item::Table(Table::new());
    }
    let budgets = doc["budgets"]
        .as_table_mut()
        .context("`budgets` in guardrails.toml is not a table")?;
    budgets["max_attempts_per_task"] = value(max_attempts_per_task as i64);
    budgets["max_iterations"] = value(max_iterations as i64);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Write the `[aclc]` table into harness.toml, preserving every other key,
/// table, and comment. Creates the file (and `.harness/`) if absent. Inert
/// fields are still written so the file is a faithful, round-trippable record of
/// the chosen configuration.
pub fn save_aclc_config(root: &Path, aclc: &AclcConfig) -> Result<()> {
    use toml_edit::{value, DocumentMut, Item, Table};

    let path = root.join(".harness").join("harness.toml");
    let mut doc = if path.exists() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        DocumentMut::new()
    };

    if !doc.contains_key("aclc") {
        doc["aclc"] = Item::Table(Table::new());
    }
    let t = doc["aclc"]
        .as_table_mut()
        .context("`aclc` in harness.toml is not a table")?;

    // serde-serialize the enums to their canonical snake_case strings via a
    // throwaway TOML round-trip, then copy primitive values across.
    let s = toml::to_string(aclc).context("failed to serialize aclc config")?;
    let parsed: toml::Value = toml::from_str(&s).context("failed to re-parse aclc config")?;
    let tbl = parsed
        .as_table()
        .context("aclc did not serialize to a table")?;

    for (k, v) in tbl {
        if k == "oracle" {
            continue;
        }
        match v {
            toml::Value::Integer(i) => t[k] = value(*i),
            toml::Value::String(st) => t[k] = value(st.as_str()),
            toml::Value::Boolean(b) => t[k] = value(*b),
            _ => {}
        }
    }

    if !t.contains_key("oracle") {
        t["oracle"] = Item::Table(Table::new());
    }
    let ot = t["oracle"]
        .as_table_mut()
        .context("`aclc.oracle` is not a table")?;
    match &aclc.oracle.command {
        Some(cmd) => ot["command"] = value(cmd.as_str()),
        None => {
            ot.remove("command");
        }
    }
    ot["protected"] = value(aclc.oracle.protected);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Saving the budgets edits only the two integer values, leaving every other
    /// table, key, and comment in guardrails.toml intact.
    #[test]
    fn save_budgets_preserves_other_keys_and_comments() {
        let dir = std::env::temp_dir().join(format!("harness-cfg-keep-{}", std::process::id()));
        let gdir = dir.join(".harness").join("guardrails");
        std::fs::create_dir_all(&gdir).unwrap();
        let path = gdir.join("guardrails.toml");
        std::fs::write(
            &path,
            "[budgets]\nmax_attempts_per_task = 3\nmax_iterations = 50\n\n\
             # keep me\n[operations]\ndeny_destructive = true\n",
        )
        .unwrap();

        save_guardrail_budgets(&dir, 7, 200).unwrap();

        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("max_attempts_per_task = 7"), "{out}");
        assert!(out.contains("max_iterations = 200"), "{out}");
        assert!(out.contains("# keep me"), "{out}");
        assert!(out.contains("deny_destructive = true"), "{out}");

        let g = load_guardrails(&dir).unwrap();
        assert_eq!(g.budgets.max_attempts_per_task, 7);
        assert_eq!(g.budgets.max_iterations, 200);
        assert!(g.operations.deny_destructive);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Writing then loading an [aclc] table round-trips, sets `aclc_present`, and
    /// preserves unrelated tables/comments in harness.toml.
    #[test]
    fn save_aclc_round_trips_and_preserves() {
        use crate::aclc::Preset;
        let dir = std::env::temp_dir().join(format!("harness-aclc-rt-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join(".harness")).unwrap();
        std::fs::write(
            dir.join(".harness").join("harness.toml"),
            "# top comment\n[agent]\ncommand = \"claude -p {prompt_file}\"\n",
        )
        .unwrap();

        let mut a = Preset::RefineNotes.config();
        a.oracle.command = Some("pytest -q && mypy .".into());
        save_aclc_config(&dir, &a).unwrap();

        let raw = std::fs::read_to_string(dir.join(".harness").join("harness.toml")).unwrap();
        assert!(raw.contains("# top comment"), "{raw}");
        assert!(
            raw.contains("command = \"claude -p {prompt_file}\""),
            "{raw}"
        );

        let cfg = load_harness_config(&dir).unwrap();
        assert!(cfg.aclc_present);
        let resolved = resolve_aclc(&cfg, &GuardrailsConfig::default());
        assert_eq!(resolved.loop_mode, crate::aclc::LoopMode::UntilPass);
        assert_eq!(resolved.memory, crate::aclc::Memory::Compact);
        assert_eq!(
            resolved.oracle.command.as_deref(),
            Some("pytest -q && mypy .")
        );
        assert!(resolved.oracle.protected);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// With no guardrails.toml present, saving creates one with a [budgets] table.
    #[test]
    fn save_budgets_creates_file_when_absent() {
        let dir = std::env::temp_dir().join(format!("harness-cfg-new-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();

        save_guardrail_budgets(&dir, 5, 99).unwrap();

        let g = load_guardrails(&dir).unwrap();
        assert_eq!(g.budgets.max_attempts_per_task, 5);
        assert_eq!(g.budgets.max_iterations, 99);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn model_id_extracted_from_command_forms() {
        assert_eq!(
            model_id_from_command("claude -p --model claude-opus-4-8 < {prompt_file}"),
            "claude-opus-4-8"
        );
        assert_eq!(
            model_id_from_command("claude --model=claude-sonnet-4-6 -p {prompt_file}"),
            "claude-sonnet-4-6"
        );
        // No model pinned → empty (a determinism hazard the harness can flag).
        assert_eq!(model_id_from_command("claude -p {prompt_file}"), "");
        // Dangling --model with no following token → empty, not a panic.
        assert_eq!(model_id_from_command("claude -p {prompt_file} --model"), "");
    }

    #[test]
    fn default_agent_command_pins_a_model() {
        let cmd = AgentConfig::default().command;
        assert!(
            !model_id_from_command(&cmd).is_empty(),
            "default agent command should pin an explicit model: {cmd}"
        );
    }
}
