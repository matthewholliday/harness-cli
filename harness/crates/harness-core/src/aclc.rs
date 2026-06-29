//! ACLC — Agentic Coding Loop Configuration (v0.1 draft).
//!
//! This module implements the *control surface* defined by the ACLC standard:
//! the orthogonal axes that govern whether the agent runs once or in a loop,
//! what state survives between attempts, and how success is decided. It owns the
//! configuration model (§3), the validator (§5/§6), the named presets (§7.1),
//! and the canonical JSON Schema (Appendix A).
//!
//! ACLC governs **one task's attempt loop**. In this harness an "attempt" is one
//! execution of the agent against a single task; the outer multi-task scheduler
//! and the SDLC phase machinery sit outside ACLC's scope (§1.1, §10).

use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Axis enums (§3.1) ─────────────────────────────────────────────────────────

/// `loop` — single attempt vs. iterate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoopMode {
    /// One attempt, end to end. The harness returns the workspace regardless of
    /// the oracle result.
    #[default]
    Off,
    /// Iterate attempts until the oracle passes or `max_attempts` is reached.
    UntilPass,
}

/// `workspace` — between attempts, reset the code (`fresh`) or retain it
/// (`continue`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    /// Reset the workspace to baseline before each attempt.
    Fresh,
    /// Leave the prior attempt's workspace in place and edit it.
    #[default]
    Continue,
}

/// `memory` — what survives a reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Memory {
    /// No memory is kept between attempts.
    #[default]
    Off,
    /// Memory becomes exactly the latest learning entry.
    Replace,
    /// Each learning entry is appended; nothing is dropped.
    Append,
    /// Append, then reconcile to at most `memory_cap` entries (§7.4).
    Compact,
}

/// `learning` — what a memory entry *is*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Learning {
    /// The failure signal verbatim (error message, failing test output).
    Raw,
    /// A forward-looking, actionable reflection on the failure.
    #[default]
    Reflection,
}

/// `on_exhaustion` — what the run returns when `max_attempts` is reached with no
/// pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnExhaustion {
    /// Return the highest-ranked attempt (§8.2). Default — guarantees a
    /// non-empty, best-effort result.
    #[default]
    KeepBest,
    /// Return the final attempt's workspace as-is.
    KeepLast,
    /// Reset the workspace to baseline and return it.
    Clean,
}

// ── Oracle (§3.1, §8.4) ───────────────────────────────────────────────────────

/// The procedure that decides success, and whether the agent can see/edit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleConfig {
    /// Shell command whose exit status decides pass/fail. Required when
    /// `loop = until_pass` (validated in [`validate`]); inert otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// When true, the oracle definition is kept outside the agent's writable
    /// workspace and never exposed to it (§8.4).
    #[serde(default = "default_protected")]
    pub protected: bool,
}

fn default_protected() -> bool {
    true
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            command: None,
            protected: default_protected(),
        }
    }
}

// ── Configuration object (§3) ─────────────────────────────────────────────────

/// The ACLC configuration object. Each field governs one orthogonal axis; the
/// named presets in §7.1 are compositions over these primitives, not primitives
/// themselves (there is deliberately no top-level "mode").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclcConfig {
    /// `loop` — single attempt vs. iterate. (`loop` is a Rust keyword, hence the
    /// field rename.)
    #[serde(rename = "loop", default)]
    pub loop_mode: LoopMode,
    /// Maximum number of attempts under `loop = until_pass`.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Whether the workspace is reset between attempts.
    #[serde(default)]
    pub workspace: Workspace,
    /// What state survives a reset.
    #[serde(default)]
    pub memory: Memory,
    /// Bound on retained entries under `memory = compact`.
    #[serde(default = "default_memory_cap")]
    pub memory_cap: u32,
    /// What a memory entry is.
    #[serde(default)]
    pub learning: Learning,
    /// What decides success.
    #[serde(default)]
    pub oracle: OracleConfig,
    /// What the run returns on exhaustion.
    #[serde(default)]
    pub on_exhaustion: OnExhaustion,
}

fn default_max_attempts() -> u32 {
    10
}

fn default_memory_cap() -> u32 {
    8
}

impl Default for AclcConfig {
    fn default() -> Self {
        Self {
            loop_mode: LoopMode::default(),
            max_attempts: default_max_attempts(),
            workspace: Workspace::default(),
            memory: Memory::default(),
            memory_cap: default_memory_cap(),
            learning: Learning::default(),
            oracle: OracleConfig::default(),
            on_exhaustion: OnExhaustion::default(),
        }
    }
}

impl AclcConfig {
    /// True when this configuration loops (`loop = until_pass`).
    pub fn loops(&self) -> bool {
        self.loop_mode == LoopMode::UntilPass
    }

    /// True when memory is active (any mode other than `off`).
    pub fn memory_on(&self) -> bool {
        self.memory != Memory::Off
    }
}

// ── Validation (§5, §6) ───────────────────────────────────────────────────────

/// Severity of a validation [`Finding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A MUST violation (§5.1). The harness MUST refuse to start a run while any
    /// error finding is present (§6).
    Error,
    /// A SHOULD violation (§5.2–§5.3). Valid but discouraged; never blocks a run.
    Warning,
}

/// One record produced by the validator (§6): `{severity, field(s), message}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub fields: Vec<String>,
    pub message: String,
}

impl Finding {
    fn error(fields: &[&str], message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            fields: fields.iter().map(|s| s.to_string()).collect(),
            message: message.into(),
        }
    }

    fn warn(fields: &[&str], message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            fields: fields.iter().map(|s| s.to_string()).collect(),
            message: message.into(),
        }
    }
}

/// Validate a configuration, returning every error and warning (§6). Errors
/// come from §5.1; warnings from §5.2 (inert-field guard) and §5.3 (discouraged
/// combinations). A configuration is rejected iff [`has_errors`] of the result.
pub fn validate(cfg: &AclcConfig) -> Vec<Finding> {
    let mut out = Vec::new();
    let looping = cfg.loops();

    // ── §5.1 Hard constraints (MUST — reject) ──
    if looping
        && cfg
            .oracle
            .command
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        out.push(Finding::error(
            &["loop", "oracle.command"],
            "loop = until_pass requires oracle.command — a loop with no success oracle cannot terminate on success",
        ));
    }
    if cfg.max_attempts < 1 {
        out.push(Finding::error(
            &["max_attempts"],
            "max_attempts must be ≥ 1",
        ));
    }
    if cfg.memory_cap < 1 {
        out.push(Finding::error(&["memory_cap"], "memory_cap must be ≥ 1"));
    }

    // ── §5.2 Inert-field guard (SHOULD — warn) ──
    // A field set to a non-default value while its "Applies when" is false.
    if !looping {
        if cfg.max_attempts != default_max_attempts() {
            out.push(Finding::warn(
                &["max_attempts"],
                "max_attempts has no effect while loop = off",
            ));
        }
        if cfg.workspace != Workspace::default() {
            out.push(Finding::warn(
                &["workspace"],
                "workspace has no effect while loop = off",
            ));
        }
        if cfg.memory != Memory::default() {
            out.push(Finding::warn(
                &["memory"],
                "memory has no effect while loop = off — expecting learnings without a loop is a common confusion",
            ));
        }
        if cfg.on_exhaustion != OnExhaustion::default() {
            out.push(Finding::warn(
                &["on_exhaustion"],
                "on_exhaustion has no effect while loop = off",
            ));
        }
        if cfg.oracle.command.is_some() {
            out.push(Finding::warn(
                &["oracle.command"],
                "oracle.command has no effect while loop = off",
            ));
        }
    }
    if cfg.memory != Memory::Compact && cfg.memory_cap != default_memory_cap() {
        out.push(Finding::warn(
            &["memory_cap"],
            "memory_cap has no effect unless memory = compact",
        ));
    }
    if !cfg.memory_on() && cfg.learning != Learning::default() {
        out.push(Finding::warn(
            &["learning"],
            "learning has no effect while memory = off",
        ));
    }

    // ── §5.3 Discouraged combinations (SHOULD — warn) ──
    // Only meaningful while the loop (and thus these axes) is active.
    if looping {
        if cfg.memory == Memory::Append {
            out.push(Finding::warn(
                &["memory"],
                "memory = append grows unboundedly; context bloat and contradictory entries accumulate — prefer compact",
            ));
        }
        if cfg.learning == Learning::Raw && matches!(cfg.memory, Memory::Append | Memory::Compact) {
            out.push(Finding::warn(
                &["learning", "memory"],
                "learning = raw under accumulation is low-signal noise that degrades later attempts — prefer reflection",
            ));
        }
        if !cfg.oracle.protected && cfg.memory_on() {
            out.push(Finding::warn(
                &["oracle.protected", "memory"],
                "oracle.protected = false with memory on lets a reward-hacking strategy be captured as a learning and propagated across attempts",
            ));
        }
        if cfg.on_exhaustion == OnExhaustion::Clean && cfg.workspace == Workspace::Fresh {
            out.push(Finding::warn(
                &["on_exhaustion", "workspace"],
                "on_exhaustion = clean with workspace = fresh can return an empty workspace — almost never intended",
            ));
        }
        if cfg.workspace == Workspace::Fresh && cfg.memory == Memory::Off {
            out.push(Finding::warn(
                &["workspace", "memory"],
                "workspace = fresh with memory = off makes attempts i.i.d. — the loop cannot learn and only exploits sampling variance",
            ));
        }
    }

    out
}

/// True if any [`Finding`] in the list is an error (§6 — the harness MUST refuse
/// to start a run while any error is present).
pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

// ── Presets (§7.1) ────────────────────────────────────────────────────────────

/// A named composition of §3 primitives. The names are normative: a conforming
/// harness that exposes a preset MUST give it these semantics (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// One agent, end to end.
    SinglePass,
    /// Re-roll from scratch until one passes. Cannot learn.
    Resample,
    /// Keep the code, fix it in place. The common "coding loop."
    Refine,
    /// Recommended default: iterate on code; carry a bounded, reconciled set of
    /// actionable lessons.
    RefineNotes,
    /// Clean slate each attempt, but lessons survive the reset.
    ResampleNotes,
}

impl Preset {
    /// Parse a preset from its snake_case name. Accepts the canonical names plus
    /// the legacy "Ralph Loop" alias (§7.1).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "single_pass" | "single" => Some(Preset::SinglePass),
            "resample" | "ralph" | "ralph_loop" => Some(Preset::Resample),
            "refine" => Some(Preset::Refine),
            "refine_notes" => Some(Preset::RefineNotes),
            "resample_notes" => Some(Preset::ResampleNotes),
            _ => None,
        }
    }

    /// The configuration this preset expands to. Fields not part of the preset's
    /// definition take their ACLC defaults (§7.1: "exact defaults inside a preset
    /// are an application choice except where a field's value is part of the
    /// preset's definition"). `oracle.command` is left unset for the caller to
    /// supply (it resolves per-spec to the protected eval suite).
    pub fn config(self) -> AclcConfig {
        let base = AclcConfig::default();
        match self {
            Preset::SinglePass => AclcConfig {
                loop_mode: LoopMode::Off,
                ..base
            },
            Preset::Resample => AclcConfig {
                loop_mode: LoopMode::UntilPass,
                workspace: Workspace::Fresh,
                memory: Memory::Off,
                ..base
            },
            Preset::Refine => AclcConfig {
                loop_mode: LoopMode::UntilPass,
                workspace: Workspace::Continue,
                memory: Memory::Off,
                ..base
            },
            Preset::RefineNotes => AclcConfig {
                loop_mode: LoopMode::UntilPass,
                workspace: Workspace::Continue,
                memory: Memory::Compact,
                learning: Learning::Reflection,
                ..base
            },
            Preset::ResampleNotes => AclcConfig {
                loop_mode: LoopMode::UntilPass,
                workspace: Workspace::Fresh,
                memory: Memory::Compact,
                learning: Learning::Reflection,
                ..base
            },
        }
    }

    /// The canonical snake_case name.
    pub fn name(self) -> &'static str {
        match self {
            Preset::SinglePass => "single_pass",
            Preset::Resample => "resample",
            Preset::Refine => "refine",
            Preset::RefineNotes => "refine_notes",
            Preset::ResampleNotes => "resample_notes",
        }
    }

    /// All presets, in display order.
    pub fn all() -> [Preset; 5] {
        [
            Preset::SinglePass,
            Preset::Resample,
            Preset::Refine,
            Preset::RefineNotes,
            Preset::ResampleNotes,
        ]
    }

    /// If `cfg` matches a preset's defining fields exactly, return that preset;
    /// otherwise `None` ("Custom"). Used by UIs to label the current config.
    pub fn matching(cfg: &AclcConfig) -> Option<Preset> {
        Preset::all().into_iter().find(|p| {
            let pc = p.config();
            pc.loop_mode == cfg.loop_mode
                && (pc.loop_mode == LoopMode::Off
                    || (pc.workspace == cfg.workspace
                        && pc.memory == cfg.memory
                        && (pc.memory == Memory::Off || pc.learning == cfg.learning)))
        })
    }
}

// ── JSON Schema (Appendix A) ──────────────────────────────────────────────────

/// The canonical draft 2020-12 JSON Schema for the configuration object
/// (Appendix A). Conditional applicability (§3.1) and the hard constraints
/// (§5.1) are encoded; the SHOULD-level warnings of §5.3 are intentionally not,
/// since they must not block a run.
pub const JSON_SCHEMA: &str = r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://example.com/aclc/0.1/config.schema.json",
  "title": "ACLC Configuration",
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "loop": { "enum": ["off", "until_pass"], "default": "off" },
    "max_attempts": { "type": "integer", "minimum": 1, "default": 10 },
    "workspace": { "enum": ["fresh", "continue"], "default": "continue" },
    "memory": {
      "enum": ["off", "replace", "append", "compact"],
      "default": "off"
    },
    "memory_cap": { "type": "integer", "minimum": 1, "default": 8 },
    "learning": { "enum": ["raw", "reflection"], "default": "reflection" },
    "oracle": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "command": { "type": "string" },
        "protected": { "type": "boolean", "default": true }
      },
      "required": ["command"]
    },
    "on_exhaustion": {
      "enum": ["keep_best", "keep_last", "clean"],
      "default": "keep_best"
    }
  },
  "required": ["loop"],
  "allOf": [
    {
      "if": { "properties": { "loop": { "const": "until_pass" } } },
      "then": { "required": ["oracle"] }
    },
    {
      "if": { "properties": { "memory": { "const": "compact" } } },
      "then": { "required": ["memory_cap"] }
    }
  ]
}
"##;

/// Write the canonical JSON Schema to `.harness/schema/aclc-0.1.schema.json`
/// under `root`, creating parent directories as needed.
pub fn write_json_schema(root: &Path) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context;
    let dir = root.join(".harness").join("schema");
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join("aclc-0.1.schema.json");
    std::fs::write(&path, JSON_SCHEMA)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_spec_3_1() {
        let c = AclcConfig::default();
        assert_eq!(c.loop_mode, LoopMode::Off);
        assert_eq!(c.max_attempts, 10);
        assert_eq!(c.workspace, Workspace::Continue);
        assert_eq!(c.memory, Memory::Off);
        assert_eq!(c.memory_cap, 8);
        assert_eq!(c.learning, Learning::Reflection);
        assert!(c.oracle.protected);
        assert_eq!(c.oracle.command, None);
        assert_eq!(c.on_exhaustion, OnExhaustion::KeepBest);
    }

    #[test]
    fn default_single_pass_is_valid() {
        // The default (loop = off) is a valid Single Pass with no findings.
        assert!(validate(&AclcConfig::default()).is_empty());
    }

    // ── §5.1 errors ──

    #[test]
    fn until_pass_without_oracle_is_error() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            ..Default::default()
        };
        let f = validate(&c);
        assert!(has_errors(&f));
        assert!(f
            .iter()
            .any(|x| x.fields.contains(&"oracle.command".to_string())));
    }

    #[test]
    fn until_pass_with_blank_oracle_is_error() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: OracleConfig {
                command: Some("   ".into()),
                protected: true,
            },
            ..Default::default()
        };
        assert!(has_errors(&validate(&c)));
    }

    #[test]
    fn zero_attempts_or_cap_is_error() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: OracleConfig {
                command: Some("pytest".into()),
                protected: true,
            },
            max_attempts: 0,
            memory_cap: 0,
            ..Default::default()
        };
        let f = validate(&c);
        assert!(
            f.iter()
                .any(|x| x.fields == vec!["max_attempts".to_string()]
                    && x.severity == Severity::Error)
        );
        assert!(f
            .iter()
            .any(|x| x.fields == vec!["memory_cap".to_string()] && x.severity == Severity::Error));
    }

    fn oracle() -> OracleConfig {
        OracleConfig {
            command: Some("pytest -q".into()),
            protected: true,
        }
    }

    // ── §5.2 inert-field guard ──

    #[test]
    fn memory_set_while_loop_off_warns_inert() {
        let c = AclcConfig {
            loop_mode: LoopMode::Off,
            memory: Memory::Append,
            ..Default::default()
        };
        let f = validate(&c);
        assert!(!has_errors(&f));
        assert!(f
            .iter()
            .any(|x| x.fields.contains(&"memory".to_string()) && x.severity == Severity::Warning));
    }

    #[test]
    fn memory_cap_inert_unless_compact() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: oracle(),
            memory: Memory::Append,
            memory_cap: 4,
            learning: Learning::Reflection,
            ..Default::default()
        };
        let f = validate(&c);
        assert!(f.iter().any(|x| x.fields == vec!["memory_cap".to_string()]));
    }

    #[test]
    fn learning_inert_when_memory_off() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: oracle(),
            workspace: Workspace::Continue,
            memory: Memory::Off,
            learning: Learning::Raw,
            ..Default::default()
        };
        let f = validate(&c);
        assert!(f.iter().any(|x| x.fields == vec!["learning".to_string()]));
    }

    // ── §5.3 discouraged combinations ──

    #[test]
    fn append_without_cap_warns() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: oracle(),
            memory: Memory::Append,
            learning: Learning::Reflection,
            ..Default::default()
        };
        assert!(validate(&c)
            .iter()
            .any(|x| x.fields == vec!["memory".to_string()] && x.severity == Severity::Warning));
    }

    #[test]
    fn raw_under_accumulation_warns() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: oracle(),
            memory: Memory::Compact,
            learning: Learning::Raw,
            ..Default::default()
        };
        assert!(validate(&c)
            .iter()
            .any(|x| x.fields.contains(&"learning".to_string())));
    }

    #[test]
    fn unprotected_oracle_with_memory_warns() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: OracleConfig {
                command: Some("pytest".into()),
                protected: false,
            },
            memory: Memory::Compact,
            learning: Learning::Reflection,
            ..Default::default()
        };
        assert!(validate(&c)
            .iter()
            .any(|x| x.fields.contains(&"oracle.protected".to_string())));
    }

    #[test]
    fn clean_with_fresh_warns() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: oracle(),
            workspace: Workspace::Fresh,
            memory: Memory::Compact,
            learning: Learning::Reflection,
            on_exhaustion: OnExhaustion::Clean,
            ..Default::default()
        };
        assert!(validate(&c)
            .iter()
            .any(|x| x.fields.contains(&"on_exhaustion".to_string())
                && x.fields.contains(&"workspace".to_string())));
    }

    #[test]
    fn fresh_with_memory_off_warns() {
        let c = AclcConfig {
            loop_mode: LoopMode::UntilPass,
            oracle: oracle(),
            workspace: Workspace::Fresh,
            memory: Memory::Off,
            ..Default::default()
        };
        assert!(validate(&c)
            .iter()
            .any(|x| x.fields.contains(&"workspace".to_string())
                && x.fields.contains(&"memory".to_string())));
    }

    // ── presets ──

    #[test]
    fn presets_have_normative_semantics() {
        assert_eq!(Preset::SinglePass.config().loop_mode, LoopMode::Off);

        let r = Preset::Resample.config();
        assert_eq!(r.loop_mode, LoopMode::UntilPass);
        assert_eq!(r.workspace, Workspace::Fresh);
        assert_eq!(r.memory, Memory::Off);

        let rf = Preset::Refine.config();
        assert_eq!(rf.workspace, Workspace::Continue);
        assert_eq!(rf.memory, Memory::Off);

        let rn = Preset::RefineNotes.config();
        assert_eq!(rn.workspace, Workspace::Continue);
        assert_eq!(rn.memory, Memory::Compact);
        assert_eq!(rn.learning, Learning::Reflection);

        let sn = Preset::ResampleNotes.config();
        assert_eq!(sn.workspace, Workspace::Fresh);
        assert_eq!(sn.memory, Memory::Compact);
    }

    #[test]
    fn ralph_loop_aliases_resample() {
        assert_eq!(Preset::from_name("ralph_loop"), Some(Preset::Resample));
        assert_eq!(Preset::from_name("Resample"), Some(Preset::Resample));
        assert_eq!(Preset::from_name("nope"), None);
    }

    #[test]
    fn matching_round_trips_each_preset() {
        for p in Preset::all() {
            assert_eq!(Preset::matching(&p.config()), Some(p), "{}", p.name());
        }
    }

    #[test]
    fn toml_round_trip_uses_snake_case_and_loop_key() {
        let c = Preset::RefineNotes.config();
        let mut c = c;
        c.oracle.command = Some("pytest -q && mypy .".into());
        let s = toml::to_string(&c).unwrap();
        assert!(s.contains("loop = \"until_pass\""), "{s}");
        assert!(s.contains("memory = \"compact\""), "{s}");
        let back: AclcConfig = toml::from_str(&s).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn schema_is_valid_json() {
        let v: serde_json::Value = serde_json::from_str(JSON_SCHEMA).unwrap();
        assert_eq!(v["title"], "ACLC Configuration");
    }
}
