use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::HarnessConfig;
use crate::spec::{load_requirements, spec_dir, Task};
use crate::state::read_progress;

/// Truncate text to `max_chars` total, keeping `head_chars` from the start
/// and `tail_chars` from the end with a `[...]` marker in the middle.
/// Cut points snap to the nearest newline boundary so no line is split.
fn truncate_at_newlines(
    text: &str,
    max_chars: usize,
    head_chars: usize,
    tail_chars: usize,
) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // Head: snap backward to the last newline at or before head_chars.
    let head_raw: String = chars[..head_chars.min(chars.len())].iter().collect();
    let head_end = head_raw
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(head_chars.min(chars.len()));
    // Tail: snap forward to the next newline at or after the tail start.
    let tail_start_abs = chars.len().saturating_sub(tail_chars);
    let tail_raw: String = chars[tail_start_abs..].iter().collect();
    let tail_start = tail_start_abs + tail_raw.find('\n').unwrap_or(0);
    let head: String = chars[..head_end].iter().collect();
    let tail: String = chars[tail_start..].iter().collect();
    format!("{}\n\n[...]\n\n{}", head, tail)
}

pub fn substitute(template: &str, vars: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        let placeholder = format!("{{{}}}", key);
        result = result.replace(&placeholder, value);
    }
    result
}

/// Select and load the prompt template for this iteration.
///
/// Priority:
///   1. `phase_template` path (from `[phases.<name>].prompt_template`)
///   2. `init.md` on the very first iteration (when no phase template is set)
///   3. `loop.md` otherwise
fn load_template(
    root: &Path,
    is_first_iteration: bool,
    phase_template: Option<&str>,
) -> Result<String> {
    let prompts_dir = root.join(".harness").join("prompts");

    if let Some(rel) = phase_template {
        let path = root.join(rel);
        return fs::read_to_string(&path)
            .with_context(|| format!("Failed to read phase template at {:?}", path));
    }

    let init_path = prompts_dir.join("init.md");
    if is_first_iteration && init_path.exists() {
        return fs::read_to_string(&init_path)
            .with_context(|| format!("Failed to read init.md at {:?}", init_path));
    }

    let loop_path = prompts_dir.join("loop.md");
    fs::read_to_string(&loop_path)
        .with_context(|| format!("Failed to read loop.md at {:?}", loop_path))
}

pub fn compose_prompt(
    root: &Path,
    _config: &HarnessConfig,
    task: &Task,
    spec_name: &str,
    is_first_iteration: bool,
    // Active SDLC phase name, or None when phases are disabled.
    phase_name: Option<&str>,
    // Relative path to the phase-specific prompt template, if configured.
    phase_template: Option<&str>,
) -> Result<String> {
    let template = load_template(root, is_first_iteration, phase_template)?;

    let progress = read_progress(root).unwrap_or_default();

    let rules_path = root.join(".harness").join("guardrails").join("rules.md");
    let rules = if rules_path.exists() {
        fs::read_to_string(&rules_path)
            .with_context(|| format!("Failed to read rules.md at {:?}", rules_path))?
    } else {
        String::new()
    };

    // Load the spec's requirements and keep only those this task references.
    let requirements_json = match load_requirements(&spec_dir(root, spec_name)) {
        Ok(reqs) => {
            let matching: Vec<_> = reqs
                .requirements
                .into_iter()
                .filter(|r| task.requirements.contains(&r.id))
                .collect();
            serde_json::to_string_pretty(&matching).unwrap_or_else(|_| "[]".to_string())
        }
        Err(_) => "[]".to_string(),
    };

    let design_path = root.join(".specs").join(spec_name).join("2-design.md");
    let design_excerpt = if design_path.exists() {
        let design = fs::read_to_string(&design_path)
            .with_context(|| format!("Failed to read design.md at {:?}", design_path))?;
        truncate_at_newlines(&design, 4000, 2000, 500)
    } else {
        String::new()
    };

    let task_acceptance = task.acceptance.join("\n");
    let task_files_hint = task.files_hint.join(", ");
    let phase_name_str = phase_name.unwrap_or("");

    // Surface the prior attempt's failure (gate output or agent error), if any,
    // so the agent can self-correct rather than repeat the same mistake. Capped
    // so a noisy gate log can't dominate the prompt.
    let last_failure = match &task.last_failure {
        Some(f) if !f.trim().is_empty() => truncate_at_newlines(f, 3000, 2400, 400),
        _ => String::new(),
    };

    let vars: &[(&str, &str)] = &[
        ("task_id", &task.id),
        ("task_title", &task.title),
        ("task_acceptance", &task_acceptance),
        ("task_files_hint", &task_files_hint),
        ("spec_name", spec_name),
        ("progress", &progress),
        ("rules", &rules),
        ("requirements", &requirements_json),
        ("design_excerpt", &design_excerpt),
        ("phase_name", phase_name_str),
        ("last_failure", &last_failure),
    ];

    let mut body = substitute(&template, vars);

    let phase_header = if let Some(p) = phase_name {
        format!("\n**Phase:** {p}")
    } else {
        String::new()
    };
    let retry_section = if last_failure.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n## Previous attempt failed — fix this first\nThis task was already attempted and failed. Read the failure below, diagnose the root cause, and address it in this attempt. Do not simply retry the same approach.\n\n{last_failure}"
        )
    };
    let footer = format!(
        "{retry_section}\n\n## Your task\nID: {}{}\nTitle: {}\nAcceptance:\n{}\n\nDo ONLY this task. Leave the project buildable. Update .harness/logs/progress.md with what you did. Then stop.\n\n**Do not modify any file under `.specs/` or `.harness/` (other than `.harness/logs/progress.md`). The harness will fail this iteration if you do.**",
        task.id,
        phase_header,
        task.title,
        task_acceptance,
    );
    body.push_str(&footer);

    // Apply the global prompt size cap, trimming from the middle to preserve
    // the task directive at the end.
    if let Some(max_chars) = _config.prompts.max_prompt_chars {
        let total = body.chars().count();
        if total > max_chars {
            let head_chars = max_chars * 7 / 10;
            let tail_chars = max_chars / 10;
            let chars: Vec<char> = body.chars().collect();
            // Snap head to the nearest preceding newline.
            let head_str: String = chars[..head_chars].iter().collect();
            let head_end = head_str.rfind('\n').map(|i| i + 1).unwrap_or(head_chars);
            // Snap tail to the nearest following newline.
            let tail_start_abs = chars.len().saturating_sub(tail_chars);
            let tail_str: String = chars[tail_start_abs..].iter().collect();
            let tail_start = tail_start_abs + tail_str.find('\n').unwrap_or(0);
            let head: String = chars[..head_end].iter().collect();
            let tail: String = chars[tail_start..].iter().collect();
            body = format!(
                "{}\n\n[... prompt truncated: {total} chars exceeded max_prompt_chars={max_chars} ...]\n\n{}",
                head, tail
            );
        }
    }

    Ok(body)
}

pub fn write_prompt_file(prompt: &str) -> Result<(PathBuf, String)> {
    use std::io::Write;

    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    let hash = hex::encode(hasher.finalize());

    let tmp_path = std::env::temp_dir().join(format!("harness-prompt-{}.md", &hash[..16]));
    let mut file = fs::File::create(&tmp_path)
        .with_context(|| format!("Failed to create prompt temp file at {:?}", tmp_path))?;
    file.write_all(prompt.as_bytes())
        .with_context(|| "Failed to write prompt to temp file")?;

    Ok((tmp_path, hash))
}
