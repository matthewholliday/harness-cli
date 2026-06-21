use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::HarnessConfig;
use crate::spec::{spec_dir, Task, load_requirements};
use crate::state::read_progress;

pub fn substitute(template: &str, vars: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        let placeholder = format!("{{{}}}", key);
        result = result.replace(&placeholder, value);
    }
    result
}

pub fn compose_prompt(
    root: &Path,
    _config: &HarnessConfig,
    task: &Task,
    spec_name: &str,
    is_first_iteration: bool,
) -> Result<String> {
    let prompts_dir = root.join(".harness").join("prompts");
    let init_path = prompts_dir.join("init.md");
    let loop_path = prompts_dir.join("loop.md");

    let template = if is_first_iteration && init_path.exists() {
        fs::read_to_string(&init_path)
            .with_context(|| format!("Failed to read init.md at {:?}", init_path))?
    } else {
        fs::read_to_string(&loop_path)
            .with_context(|| format!("Failed to read loop.md at {:?}", loop_path))?
    };

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

    let design_path = root
        .join(".specs")
        .join(spec_name)
        .join("2-design.md");
    let design_excerpt = if design_path.exists() {
        let design = fs::read_to_string(&design_path)
            .with_context(|| format!("Failed to read design.md at {:?}", design_path))?;
        if design.chars().count() < 4000 {
            design
        } else {
            let chars: Vec<char> = design.chars().collect();
            let first: String = chars[..2000].iter().collect();
            let last: String = chars[chars.len().saturating_sub(500)..].iter().collect();
            format!("{}\n\n[...]\n\n{}", first, last)
        }
    } else {
        String::new()
    };

    let task_acceptance = task.acceptance.join("\n");
    let task_files_hint = task.files_hint.join(", ");

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
    ];

    let mut body = substitute(&template, vars);

    let footer = format!(
        "\n\n## Your task\nID: {}\nTitle: {}\nAcceptance:\n{}\n\nDo ONLY this task. Leave the project buildable. Update .harness/logs/progress.md with what you did. Then stop.\n\n**Do not modify any file under `.specs/` or `.harness/` (other than `.harness/logs/progress.md`). The harness will fail this iteration if you do.**",
        task.id,
        task.title,
        task_acceptance,
    );
    body.push_str(&footer);

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
