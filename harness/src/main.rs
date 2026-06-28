mod config;
mod hooks;
mod loop_runner;
mod manifest;
mod prompt;
mod spec;
mod state;
mod tui;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};

use crate::config::{find_project_root, load_harness_config};
use crate::hooks::{run_hook, HookInvocation};
use crate::loop_runner::{run, RunOptions};
use crate::manifest::{check_spec, record_spec, DriftKind};
use crate::spec::{list_specs, load_requirements, load_tasks, save_tasks, spec_dir, Task, TaskStatus};
use crate::state::load_state;

#[derive(Parser)]
#[command(name = "harness", version, about = "A project-agnostic Ralph-loop agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold .harness/ in the current directory.
    Init {
        #[arg(long)]
        from_specs: bool,
        #[arg(long)]
        force: bool,
    },
    /// Manage specs.
    Spec {
        #[command(subcommand)]
        cmd: SpecCmd,
    },
    /// Run the Ralph loop.
    Run {
        #[arg(long)]
        spec: Option<String>,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        max_iterations: Option<u64>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Inspect/run hooks.
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    /// Show current loop status.
    Status,
    /// Live terminal dashboard that watches a run as it happens.
    Watch,
    /// Inspect iteration/hook logs.
    Logs {
        #[arg(long)]
        iteration: Option<u64>,
        #[arg(long)]
        follow: bool,
    },
    /// Validate config, hooks, agent adapter, git.
    Doctor,
    /// Manage the spec ownership manifest (Phase 0).
    Manifest {
        #[command(subcommand)]
        cmd: ManifestCmd,
    },
    /// Regenerate owned artifacts from a spec (Phase 3 — burn & rebuild).
    Regen {
        /// Spec to regenerate.
        spec: String,
        /// Only burn/rebuild files matching this glob (subset of spec ownership).
        #[arg(long)]
        component: Option<String>,
        /// Regenerate twice and compare eval results for a determinism probe.
        #[arg(long)]
        twice: bool,
        /// Bypass the pace_layer="never" guard.
        #[arg(long)]
        force_boundary: bool,
    },
}

#[derive(Subcommand)]
enum SpecCmd {
    List,
    Draft {
        name: String,
        /// Inline brief: what the spec should do (quoted string).
        #[arg(long)]
        brief: Option<String>,
        /// Path to a brief file (.md, .txt, or any text). Use `-` for stdin.
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long)]
        interactive: bool,
    },
    Edit {
        name: String,
        #[arg(long)]
        requirements: bool,
        #[arg(long)]
        design: bool,
        #[arg(long)]
        tasks: bool,
    },
    Validate {
        name: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Sync {
        name: String,
        #[arg(long)]
        regen_tasks: bool,
        #[arg(long)]
        against_code: bool,
        #[arg(long)]
        write: bool,
    },
}

#[derive(Subcommand)]
enum ManifestCmd {
    /// Recompute and store the manifest for a spec (or all specs with --all).
    Record {
        spec: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Check for ownership drift; exits non-zero if any drift is found.
    Check {
        spec: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum HooksCmd {
    List,
    Run {
        hook: String,
        #[arg(long)]
        task: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Commands::Init { from_specs, force } => {
            let root = std::env::current_dir()?;
            cmd_init(&root, from_specs, force)?;
            Ok(0)
        }
        Commands::Run {
            spec,
            once,
            max_iterations,
            dry_run,
        } => {
            let root = find_project_root()?;
            run(
                &root,
                RunOptions {
                    spec_filter: spec,
                    once,
                    max_iterations,
                    dry_run,
                },
            )
        }
        Commands::Spec { cmd } => {
            let root = find_project_root()?;
            cmd_spec(&root, cmd)
        }
        Commands::Hooks { cmd } => {
            let root = find_project_root()?;
            cmd_hooks(&root, cmd)
        }
        Commands::Status => {
            let root = find_project_root()?;
            cmd_status(&root)
        }
        Commands::Watch => {
            let root = find_project_root()?;
            tui::run(&root)
        }
        Commands::Logs { iteration, follow } => {
            let root = find_project_root()?;
            cmd_logs(&root, iteration, follow)
        }
        Commands::Doctor => {
            let root = find_project_root()?;
            cmd_doctor(&root)
        }
        Commands::Manifest { cmd } => {
            let root = find_project_root()?;
            cmd_manifest(&root, cmd)
        }
        Commands::Regen { spec, component, twice, force_boundary } => {
            let root = find_project_root()?;
            cmd_regen(&root, &spec, component.as_deref(), twice, force_boundary)
        }
    }
}

// ─── init ──────────────────────────────────────────────────────────────────

fn cmd_init(root: &Path, _from_specs: bool, force: bool) -> Result<()> {
    let harness = root.join(".harness");
    let scaffold: &[(&str, &str)] = &[
        ("harness.toml", HARNESS_TOML),
        ("guardrails/guardrails.toml", GUARDRAILS_TOML),
        ("guardrails/rules.md", RULES_MD),
        ("prompts/loop.md", LOOP_MD),
        ("prompts/init.md", INIT_MD),
        ("prompts/draft-spec.md", DRAFT_SPEC_PROMPT),
        ("logs/progress.md", "# Progress\n\n"),
        ("logs/state.json", "{\"active_spec\":null,\"iteration_count\":0,\"last_task_id\":null,\"last_task_status\":null,\"run_start\":null}\n"),
    ];

    for (rel, body) in scaffold {
        let path = harness.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if path.exists() && !force {
            println!("  skip   {} (exists)", path.display());
            continue;
        }
        std::fs::write(&path, body)?;
        println!("  create {}", path.display());
    }

    // Hook stubs.
    let hooks_dir = harness.join("scripts").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    for name in ["run_build", "run_unit_tests", "run_e2e_tests", "run_lint", "run_update_docs"] {
        let path = hooks_dir.join(name);
        if path.exists() && !force {
            println!("  skip   {} (exists)", path.display());
            continue;
        }
        let body = HOOK_STUB.replace("<NAME>", name);
        std::fs::write(&path, body)?;
        make_executable(&path)?;
        println!("  create {}", path.display());
    }
    std::fs::create_dir_all(harness.join("scripts").join("lib"))?;
    std::fs::create_dir_all(harness.join("logs").join("iterations"))?;
    std::fs::create_dir_all(harness.join("logs").join("hooks"))?;
    std::fs::create_dir_all(harness.join("roundtrip"))?;

    // Phase 0: install a git pre-commit hook that runs `harness manifest check --all`.
    let git_hooks_dir = root.join(".git").join("hooks");
    if git_hooks_dir.is_dir() {
        let pre_commit = git_hooks_dir.join("pre-commit");
        if pre_commit.exists() && !force {
            println!("  skip   {} (exists)", pre_commit.display());
        } else {
            std::fs::write(&pre_commit, PRE_COMMIT_HOOK)?;
            make_executable(&pre_commit)?;
            println!("  create {} (manifest drift gate)", pre_commit.display());
        }
    }

    // Phase 2: create the top-level evals/ directory.
    let evals_dir = root.join("evals");
    if !evals_dir.exists() {
        std::fs::create_dir_all(&evals_dir)?;
        println!("  create evals/  (spec-independent oracle root)");
    }

    // Optional Claude Code setup agent the user can run to configure hooks.
    let agent_path = root.join(".claude").join("agents").join("harness-setup.md");
    if let Some(parent) = agent_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if agent_path.exists() && !force {
        println!("  skip   {} (exists)", agent_path.display());
    } else {
        std::fs::write(&agent_path, SETUP_AGENT)?;
        println!("  create {}", agent_path.display());
    }

    println!("\nInitialized harness at {}", harness.display());
    println!(
        "\nOptional: in Claude Code, run the `harness-setup` agent to wire the\n\
         hooks to this project's build/test/lint commands."
    );
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

// ─── spec ──────────────────────────────────────────────────────────────────

fn cmd_spec(root: &Path, cmd: SpecCmd) -> Result<i32> {
    match cmd {
        SpecCmd::List => {
            let specs = list_specs(root)?;
            if specs.is_empty() {
                println!("No specs found under .specs/");
            } else {
                for s in specs {
                    println!("{s}");
                }
            }
            Ok(0)
        }
        SpecCmd::Draft { name, brief, from, interactive } => {
            cmd_spec_draft(root, &name, brief, from, interactive)
        }
        SpecCmd::Edit {
            name,
            requirements,
            design,
            tasks,
        } => {
            let dir = spec_dir(root, &name);
            let file = if requirements {
                dir.join("1-requirements.json")
            } else if design {
                dir.join("2-design.md")
            } else if tasks {
                dir.join("3-tasks.jsonl")
            } else {
                dir.join("1-requirements.json")
            };
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
            if let Err(e) = Command::new(&editor).arg(&file).status() {
                eprintln!("failed to launch editor '{editor}': {e}");
            }
            match validate_spec(root, &name) {
                Ok(_) => Ok(0),
                Err(e) => {
                    eprintln!("✗ {name}: {e:#}");
                    Ok(1)
                }
            }
        }
        SpecCmd::Validate { name, all } => {
            if all {
                let mut ok = true;
                for s in list_specs(root)? {
                    if let Err(e) = validate_spec(root, &s) {
                        eprintln!("✗ {s}: {e:#}");
                        ok = false;
                    } else {
                        println!("✓ {s}");
                    }
                }
                Ok(if ok { 0 } else { 1 })
            } else if let Some(n) = name {
                match validate_spec(root, &n) {
                    Ok(_) => {
                        println!("✓ {n} is valid");
                        Ok(0)
                    }
                    Err(e) => {
                        eprintln!("✗ {n}: {e:#}");
                        Ok(1)
                    }
                }
            } else {
                eprintln!("provide a spec name or --all");
                Ok(1)
            }
        }
        SpecCmd::Sync { name, write, regen_tasks, against_code } => {
            if against_code {
                cmd_sync_against_code(root, &name)?;
            } else {
                cmd_sync(root, &name, write, regen_tasks)?;
            }
            Ok(0)
        }
    }
}

fn cmd_spec_draft(
    root: &Path,
    name: &str,
    brief: Option<String>,
    from: Option<PathBuf>,
    interactive: bool,
) -> Result<i32> {
    use std::io::Read as _;

    // ── 1. Validate the spec name slug ────────────────────────────────────────
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || name.starts_with('-')
    {
        anyhow::bail!("spec name must match ^[a-z0-9][a-z0-9-]*$ — got '{name}'");
    }

    // ── 2. Read the brief ─────────────────────────────────────────────────────
    let brief_text = match (brief, from) {
        (Some(b), _) => b,
        (None, Some(path)) if path == PathBuf::from("-") => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("failed to read brief from stdin")?;
            s
        }
        (None, Some(path)) => std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read brief from {}", path.display()))?,
        (None, None) if interactive => {
            // Explicit opt-in: read from stdin with a prompt. Only the
            // `--interactive` flag enables this blocking read so the command
            // never hangs waiting on an EOF that may never come.
            eprintln!("Brief (describe what this spec should do; end with Ctrl-D):");
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("failed to read brief from stdin")?;
            if s.trim().is_empty() {
                anyhow::bail!("no brief supplied on stdin");
            }
            s
        }
        (None, None) => {
            anyhow::bail!(
                "no brief supplied — pass --brief \"...\", --from <file>, \
                 --from - to read stdin, or --interactive to type one"
            );
        }
    };

    if brief_text.trim().is_empty() {
        anyhow::bail!("brief is empty");
    }

    // ── 3. Ensure the spec dir exists ─────────────────────────────────────────
    let dir = spec_dir(root, name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;

    // ── 4. Compose the drafting prompt ────────────────────────────────────────
    // Prefer the project-local override so users can customise without
    // rebuilding the binary; fall back to the compiled-in template.
    let local_template_path = root.join(".harness").join("prompts").join("draft-spec.md");
    let template = if local_template_path.exists() {
        std::fs::read_to_string(&local_template_path).unwrap_or_else(|_| DRAFT_SPEC_PROMPT.to_string())
    } else {
        DRAFT_SPEC_PROMPT.to_string()
    };
    let prompt = template
        .replace("{spec_name}", name)
        .replace("{brief}", brief_text.trim());

    // Write the prompt to a temp file.
    let prompt_path = std::env::temp_dir().join(format!("harness-draft-{name}.md"));
    std::fs::write(&prompt_path, &prompt)
        .with_context(|| format!("failed to write draft prompt to {}", prompt_path.display()))?;

    // ── 5. Run the agent ──────────────────────────────────────────────────────
    let config = load_harness_config(root)?;
    let cmd_str = config
        .agent
        .command
        .replace("{prompt_file}", &prompt_path.to_string_lossy());
    let working_dir = config.agent.working_dir.as_deref().unwrap_or(".");
    let wd = root.join(working_dir);

    println!("Drafting spec '{name}' — running agent…");
    println!("(The agent will write .specs/{name}/1-requirements.json, 2-design.md, 3-tasks.jsonl)\n");

    let status = if cfg!(windows) {
        Command::new("cmd").arg("/C").arg(&cmd_str).current_dir(&wd).status()
    } else {
        Command::new("sh").arg("-c").arg(&cmd_str).current_dir(&wd).status()
    }
    .with_context(|| format!("failed to launch agent: {cmd_str}"))?;

    let _ = std::fs::remove_file(&prompt_path);

    let agent_exit = status.code().unwrap_or(-1);
    if agent_exit != 0 {
        anyhow::bail!("agent exited {agent_exit} — check agent adapter config");
    }

    // ── 6. Validate what the agent wrote ─────────────────────────────────────
    println!("\nValidating generated spec…");
    match validate_spec(root, name) {
        Ok(()) => {
            println!("✓ .specs/{name}/ is valid\n");
            println!("Next steps:");
            println!("  harness spec validate {name}   # re-check any edits");
            println!("  harness spec sync {name}       # drift report");
            println!("  harness run --spec {name} --dry-run --once");
            println!("  harness run --spec {name}");
            Ok(0)
        }
        Err(e) => {
            eprintln!("✗ validation failed: {e:#}");
            eprintln!("\nFix the issues above and re-run:");
            eprintln!("  harness spec validate {name}");
            Ok(1)
        }
    }
}

fn validate_spec(root: &Path, name: &str) -> Result<()> {
    let dir = spec_dir(root, name);
    let config = load_harness_config(root).unwrap_or_default();

    let reqs = load_requirements(&dir).with_context(|| "1-requirements.json failed to parse")?;
    let req_ids: std::collections::HashSet<String> =
        reqs.requirements.iter().map(|r| r.id.clone()).collect();
    for r in &reqs.requirements {
        if r.acceptance_criteria.is_empty() {
            anyhow::bail!("requirement {} has no acceptance criteria (not testable)", r.id);
        }
    }

    let design_path = dir.join("2-design.md");
    if design_path.exists() {
        let design = std::fs::read_to_string(&design_path)?;
        for heading in [
            "## Context",
            "## Architecture",
            "## Data Model",
            "## Interfaces",
            "## Flows",
            "## Decisions",
            "## Risks",
            "## Requirement Coverage",
        ] {
            if !design.contains(heading) {
                anyhow::bail!("2-design.md missing required heading: {heading}");
            }
        }
    }

    let tasks = load_tasks(&dir).with_context(|| "3-tasks.jsonl failed to parse")?;
    let task_ids: std::collections::HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
    let hooks_dir = root.join(".harness").join("scripts").join("hooks");

    // All known phase names: global sequence + any explicitly configured phases.
    let known_phases: std::collections::HashSet<&str> = config
        .loop_config
        .phase_sequence
        .iter()
        .map(|s| s.as_str())
        .chain(config.phases.keys().map(|s| s.as_str()))
        .collect();

    for t in &tasks {
        for r in &t.requirements {
            if !req_ids.contains(r) {
                anyhow::bail!("task {} references unknown requirement {}", t.id, r);
            }
        }
        for d in &t.depends_on {
            if !task_ids.contains(d) {
                anyhow::bail!("task {} depends on unknown task {}", t.id, d);
            }
        }
        for h in &t.hooks {
            let exists = [h.clone(), format!("{h}.ps1"), format!("{h}.cmd"), format!("{h}.bat")]
                .iter()
                .any(|c| hooks_dir.join(c).exists());
            if !exists {
                anyhow::bail!("task {} references missing hook script '{}'", t.id, h);
            }
        }
        // Validate task-level phase overrides against the known phase set.
        if !t.phases.is_empty() && !known_phases.is_empty() {
            for p in &t.phases {
                if !known_phases.contains(p.as_str()) {
                    anyhow::bail!(
                        "task {} references phase '{}' which is not defined in \
                         [loop].phase_sequence or [phases.*] in harness.toml",
                        t.id, p
                    );
                }
            }
        }
    }
    detect_cycle(&tasks)?;

    // Phase 2: require that every requirement maps to at least one eval script.
    // Only enforced when the evals/<spec>/ directory exists (opt-in).
    let evals_dir = root.join("evals").join(name);
    if evals_dir.is_dir() {
        for req in &reqs.requirements {
            // Look for any eval file that references this requirement id.
            let has_eval = std::fs::read_dir(&evals_dir)
                .ok()
                .map(|entries| {
                    entries.filter_map(|e| e.ok()).any(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .contains(req.id.as_str())
                            || std::fs::read_to_string(e.path())
                                .ok()
                                .map(|c| c.contains(req.id.as_str()))
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if !has_eval {
                anyhow::bail!(
                    "requirement {} has no eval in evals/{name}/ — \
                     add an eval script or stub referencing '{}'",
                    req.id, req.id
                );
            }
        }
    }

    Ok(())
}

fn detect_cycle(tasks: &[spec::Task]) -> Result<()> {
    use std::collections::HashMap;
    let mut graph: HashMap<&str, &Vec<String>> = HashMap::new();
    for t in tasks {
        graph.insert(t.id.as_str(), &t.depends_on);
    }
    fn visit<'a>(
        node: &'a str,
        graph: &HashMap<&'a str, &'a Vec<String>>,
        color: &mut HashMap<&'a str, u8>,
        path: &mut Vec<&'a str>,
    ) -> Result<()> {
        color.insert(node, 1);
        path.push(node);
        if let Some(deps) = graph.get(node) {
            for d in deps.iter() {
                match color.get(d.as_str()).copied().unwrap_or(0) {
                    1 => {
                        // Show the full cycle chain for easy debugging.
                        let cycle_start = path.iter().position(|&n| n == d.as_str()).unwrap_or(0);
                        let mut chain: Vec<&str> = path[cycle_start..].to_vec();
                        chain.push(d.as_str());
                        anyhow::bail!("dependency cycle: {}", chain.join(" → "));
                    }
                    0 => {
                        if let Some((k, _)) = graph.get_key_value(d.as_str()) {
                            visit(k, graph, color, path)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        path.pop();
        color.insert(node, 2);
        Ok(())
    }
    let mut color: HashMap<&str, u8> = HashMap::new();
    let keys: Vec<&str> = graph.keys().copied().collect();
    for k in keys {
        if color.get(k).copied().unwrap_or(0) == 0 {
            visit(k, &graph, &mut color, &mut Vec::new())?;
        }
    }
    Ok(())
}

fn cmd_sync(root: &Path, name: &str, write: bool, regen_tasks: bool) -> Result<()> {
    let dir = spec_dir(root, name);
    let reqs = load_requirements(&dir)?;
    let mut tasks = load_tasks(&dir)?;

    let req_ids: std::collections::HashSet<String> =
        reqs.requirements.iter().map(|r| r.id.clone()).collect();
    let covered: std::collections::HashSet<String> =
        tasks.iter().flat_map(|t| t.requirements.clone()).collect();

    let uncovered: Vec<_> = reqs.requirements.iter().filter(|r| !covered.contains(&r.id)).collect();
    let orphaned: Vec<(String, String)> = tasks
        .iter()
        .flat_map(|t| {
            t.requirements
                .iter()
                .filter(|r| !req_ids.contains(*r))
                .map(|r| (t.id.clone(), r.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    println!("Drift report for '{name}':");
    let mut clean = true;
    for r in &uncovered {
        let text = r.text.as_deref().unwrap_or("(no text)");
        println!("  ! {} has no task — {}", r.id, text);
        clean = false;
    }
    for (tid, rid) in &orphaned {
        println!("  ! task {tid} references unknown requirement {rid}");
        clean = false;
    }
    if clean {
        println!("  (no drift detected)");
        return Ok(());
    }

    if !write && !regen_tasks {
        println!("\nRe-run with --write to generate task stubs for uncovered requirements.");
        return Ok(());
    }

    if write && !uncovered.is_empty() {
        let max_num: u32 = tasks
            .iter()
            .filter_map(|t| t.id.strip_prefix("T-").and_then(|n| n.parse::<u32>().ok()))
            .max()
            .unwrap_or(0);

        let now = Utc::now();
        let mut next_num = max_num + 1;

        for req in &uncovered {
            let title = req
                .text
                .as_deref()
                .unwrap_or(&format!("Implement {}", req.id))
                .chars()
                .take(80)
                .collect::<String>();
            let task = Task {
                id: format!("T-{:03}", next_num),
                spec: name.to_string(),
                title,
                requirements: vec![req.id.clone()],
                status: TaskStatus::Todo,
                priority: 100,
                depends_on: vec![],
                hooks: vec![],
                acceptance: req.acceptance_criteria.clone(),
                files_hint: vec![],
                attempts: 0,
                max_attempts: 3,
                notes: None,
                phases: vec![],
                completed_phases: vec![],
                created_at: now,
                updated_at: now,
            };
            println!("  + T-{:03} for {}", next_num, req.id);
            tasks.push(task);
            next_num += 1;
        }
        save_tasks(&dir, &tasks)?;
        println!("  wrote {} new task stub(s) to .specs/{name}/3-tasks.jsonl", uncovered.len());
    }

    if regen_tasks {
        eprintln!("note: --regen-tasks (full task regeneration from requirements) is not yet implemented.");
    }

    Ok(())
}

// ─── hooks ─────────────────────────────────────────────────────────────────

fn cmd_hooks(root: &Path, cmd: HooksCmd) -> Result<i32> {
    match cmd {
        HooksCmd::List => {
            let dir = root.join(".harness").join("scripts").join("hooks");
            if !dir.is_dir() {
                println!("no hooks directory at {}", dir.display());
                return Ok(0);
            }
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                if entry.path().is_file() {
                    println!("{}", entry.file_name().to_string_lossy());
                }
            }
            Ok(0)
        }
        HooksCmd::Run { hook, task } => {
            let (task_json, task_id, spec_name) = match task {
                Some(tid) => match find_task_json(root, &tid)? {
                    Some((j, s)) => (j, tid, s),
                    None => ("{}".to_string(), tid, String::new()),
                },
                None => ("{}".to_string(), String::new(), String::new()),
            };
            let config = load_harness_config(root)?;
            let inv = HookInvocation {
                hook_name: hook.clone(),
                task_id,
                spec_name,
                iteration: 0,
                attempt: 0,
            };
            let outcome = run_hook(root, &inv, &task_json, config.hooks.default_timeout_secs)?;
            print!("{}", outcome.stdout);
            eprint!("{}", outcome.stderr);
            println!(
                "\n[hook '{}' exit {} in {}ms]",
                hook, outcome.exit_code, outcome.duration_ms
            );
            Ok(outcome.exit_code)
        }
    }
}

fn find_task_json(root: &Path, task_id: &str) -> Result<Option<(String, String)>> {
    for s in list_specs(root)? {
        let tasks = load_tasks(&spec_dir(root, &s))?;
        if let Some(t) = tasks.iter().find(|t| t.id == task_id) {
            return Ok(Some((serde_json::to_string(t)?, s)));
        }
    }
    Ok(None)
}

// ─── status / logs / doctor ──────────────────────────────────────────────────

fn cmd_status(root: &Path) -> Result<i32> {
    let state = load_state(root)?;
    let config = load_harness_config(root).unwrap_or_default();
    println!("active spec:     {:?}", state.active_spec);
    println!("iteration count: {}", state.iteration_count);
    println!(
        "last task:       {:?} ({:?})",
        state.last_task_id, state.last_task_status
    );

    let has_phases = !config.loop_config.phase_sequence.is_empty();
    let (mut todo, mut prog, mut blocked, mut done) = (0, 0, 0, 0);
    for s in list_specs(root)? {
        for t in load_tasks(&spec_dir(root, &s))? {
            match t.status {
                TaskStatus::Todo => todo += 1,
                TaskStatus::InProgress => prog += 1,
                TaskStatus::Blocked => blocked += 1,
                TaskStatus::Done => done += 1,
            }
            // Show phase detail for tasks with partial phase completion.
            if has_phases && !t.completed_phases.is_empty() && t.status != TaskStatus::Done {
                let phase_seq: &Vec<String> = if t.phases.is_empty() {
                    &config.loop_config.phase_sequence
                } else {
                    &t.phases
                };
                let phases_display: Vec<String> = phase_seq
                    .iter()
                    .map(|p| {
                        if t.completed_phases.contains(p) {
                            format!("{p} ✓")
                        } else {
                            p.clone()
                        }
                    })
                    .collect();
                println!("  {} [{:?}] phases: {}", t.id, t.status, phases_display.join(", "));
            }
        }
    }
    println!("\ntasks: todo={todo} in_progress={prog} blocked={blocked} done={done}");
    Ok(0)
}

fn cmd_logs(root: &Path, iteration: Option<u64>, _follow: bool) -> Result<i32> {
    let dir = root.join(".harness").join("logs").join("iterations");
    if !dir.is_dir() {
        println!("no iteration logs yet");
        return Ok(0);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    entries.sort();

    match iteration {
        Some(n) => {
            for p in &entries {
                let body = std::fs::read_to_string(p)?;
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                    if v.get("iteration").and_then(|i| i.as_u64()) == Some(n) {
                        println!("{}", serde_json::to_string_pretty(&v)?);
                        return Ok(0);
                    }
                }
            }
            println!("no record for iteration {n}");
        }
        None => {
            for p in &entries {
                println!("{}", p.file_name().unwrap().to_string_lossy());
            }
        }
    }
    Ok(0)
}

fn cmd_doctor(root: &Path) -> Result<i32> {
    let mut ok = true;

    macro_rules! check {
        ($label:expr, $pass:expr, $hint:expr) => {{
            if $pass {
                println!("✓ {}", $label);
            } else {
                println!("✗ {} — {}", $label, $hint);
                ok = false;
            }
        }};
    }

    check!(".harness/ exists", root.join(".harness").is_dir(), "run `harness init`");

    let cfg = load_harness_config(root);
    check!("harness.toml parses", cfg.is_ok(), "fix TOML syntax");

    if let Ok(c) = &cfg {
        check!(
            "agent.command is set",
            !c.agent.command.trim().is_empty(),
            "set [agent].command"
        );

        // Verify the agent binary is reachable on PATH by extracting the base
        // command (first whitespace-delimited token, ignoring redirect syntax).
        let base_cmd = c
            .agent
            .command
            .split(|ch: char| ch.is_whitespace() || ch == '<')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !base_cmd.is_empty() && !base_cmd.contains('{') {
            let on_path = Command::new(&base_cmd)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok();
            check!(
                format!("agent '{base_cmd}' reachable on PATH"),
                on_path,
                format!("'{base_cmd}' not found — verify [agent].command in harness.toml")
            );
        }

        let hooks_dir = root.join(".harness").join("scripts").join("hooks");
        for h in &c.hooks.default {
            let exists = [h.clone(), format!("{h}.ps1"), format!("{h}.cmd"), format!("{h}.bat")]
                .iter()
                .any(|cand| hooks_dir.join(cand).exists());
            check!(format!("hook '{h}' present"), exists, "create the hook stub");
        }

        if !c.loop_config.phase_sequence.is_empty() {
            println!("phases: {}", c.loop_config.phase_sequence.join(" → "));
            for phase_name in &c.loop_config.phase_sequence {
                if let Some(phase_cfg) = c.phases.get(phase_name) {
                    if let Some(hooks) = &phase_cfg.hooks {
                        for h in hooks {
                            let exists =
                                [h.clone(), format!("{h}.ps1"), format!("{h}.cmd"), format!("{h}.bat")]
                                    .iter()
                                    .any(|cand| hooks_dir.join(cand).exists());
                            check!(
                                format!("phase '{phase_name}' hook '{h}' present"),
                                exists,
                                "create the hook stub"
                            );
                        }
                    }
                    if let Some(tmpl) = &phase_cfg.prompt_template {
                        check!(
                            format!("phase '{phase_name}' prompt template present"),
                            root.join(tmpl).exists(),
                            format!("create {tmpl}")
                        );
                    }
                }
            }
        }
    }

    let specs = list_specs(root).unwrap_or_default();
    check!("at least one spec", !specs.is_empty(), "run `harness spec draft <name>`");
    check!("git repository", root.join(".git").exists(), "run `git init` for rollback safety");

    // Phase 0: every spec must declare `owns`; then check manifest drift.
    for spec_name in &specs {
        let dir = spec_dir(root, spec_name);
        match load_requirements(&dir).ok().map(|r| r.owns.is_empty()) {
            Some(true) | None => {
                ok = false;
                println!(
                    "✗ spec '{spec_name}' has no 'owns' declaration — \
                     add an 'owns' glob list to .specs/{spec_name}/1-requirements.json"
                );
                continue;
            }
            Some(false) => {}
        }
        match check_spec(root, spec_name) {
            Ok(result) if result.is_clean() => {
                println!("✓ manifest: spec '{spec_name}' — clean");
            }
            Ok(result) => {
                ok = false;
                for drift in &result.drifts {
                    match drift {
                        DriftKind::Unrecorded { .. } => {
                            println!("✗ manifest: spec '{spec_name}' — never recorded; run `harness manifest record {spec_name}`");
                        }
                        DriftKind::StaleCode { .. } => {
                            println!("✗ manifest: spec '{spec_name}' — spec changed since last regen; run `harness regen {spec_name}`");
                        }
                        DriftKind::CodeDrift { path } => {
                            println!("✗ manifest: spec '{spec_name}' — hand-edit detected: {path}");
                        }
                        DriftKind::Missing { path } => {
                            println!("✗ manifest: spec '{spec_name}' — owned file missing: {path}");
                        }
                    }
                }
            }
            Err(e) => {
                ok = false;
                println!("✗ manifest: spec '{spec_name}' — check failed: {e:#}");
            }
        }
    }

    Ok(if ok { 0 } else { 1 })
}

// ─── manifest ─────────────────────────────────────────────────────────────────

fn cmd_manifest(root: &Path, cmd: ManifestCmd) -> Result<i32> {
    match cmd {
        ManifestCmd::Record { spec, all } => {
            let specs_to_record: Vec<String> = if all {
                list_specs(root)?
            } else if let Some(name) = spec {
                vec![name]
            } else {
                anyhow::bail!("provide a spec name or --all");
            };
            for name in &specs_to_record {
                record_spec(root, name)
                    .with_context(|| format!("recording manifest for '{name}'"))?;
                println!("✓ recorded manifest for '{name}'");
            }
            Ok(0)
        }
        ManifestCmd::Check { spec, all } => {
            let specs_to_check: Vec<String> = if all {
                list_specs(root)?
            } else if let Some(name) = spec {
                vec![name]
            } else {
                anyhow::bail!("provide a spec name or --all");
            };
            let mut clean = true;
            for name in &specs_to_check {
                match check_spec(root, name) {
                    Ok(result) if result.is_clean() => {
                        println!("✓ {name}: clean");
                    }
                    Ok(result) => {
                        clean = false;
                        for drift in &result.drifts {
                            match drift {
                                DriftKind::Unrecorded { .. } => {
                                    println!("✗ {name}: no manifest entry — run `harness manifest record {name}`");
                                }
                                DriftKind::StaleCode { .. } => {
                                    println!("✗ {name}: spec changed, code not regenerated — run `harness regen {name}`");
                                }
                                DriftKind::CodeDrift { path } => {
                                    println!("✗ {name}: hand-edit detected in owned file: {path}");
                                }
                                DriftKind::Missing { path } => {
                                    println!("✗ {name}: owned file missing: {path}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        clean = false;
                        println!("✗ {name}: check error: {e:#}");
                    }
                }
            }
            Ok(if clean { 0 } else { 1 })
        }
    }
}

// ─── regen ────────────────────────────────────────────────────────────────────

/// Phase 3 — burn-and-rebuild a spec's owned artifacts from scratch.
fn cmd_regen(
    root: &Path,
    spec_name: &str,
    component: Option<&str>,
    twice: bool,
    force_boundary: bool,
) -> Result<i32> {
    let dir = spec_dir(root, spec_name);
    if !dir.exists() {
        anyhow::bail!("spec '{spec_name}' not found");
    }

    let reqs = load_requirements(&dir)?;

    // Respect pace_layer: "never" means hands-off unless forced.
    if let Some(ref layer) = reqs.pace_layer {
        if layer == "never" && !force_boundary {
            anyhow::bail!(
                "spec '{spec_name}' has pace_layer=\"never\" — pass --force-boundary to override"
            );
        }
    }

    let owns = &reqs.owns;
    if owns.is_empty() {
        anyhow::bail!(
            "spec '{spec_name}' has no 'owns' declaration — \
             add an 'owns' glob list to 1-requirements.json before regenerating"
        );
    }

    // Build the set of files to burn.
    let all_owned = crate::manifest::expand_owned_paths(root, owns)?;
    let to_burn: Vec<String> = if let Some(comp_glob) = component {
        let comp_set = crate::manifest::build_owns_globset(&[comp_glob.to_string()])?;
        all_owned.into_iter().filter(|p| comp_set.is_match(p)).collect()
    } else {
        all_owned
    };

    if to_burn.is_empty() {
        println!("No owned files matched — nothing to regenerate.");
        return Ok(0);
    }

    let run_regen = |attempt_label: &str| -> Result<(i32, Vec<String>)> {
        // 1. Git checkpoint: record HEAD so we can roll back on failure.
        let head_sha = git_head_sha(root).unwrap_or_default();
        println!("[{attempt_label}] HEAD checkpoint: {}", if head_sha.is_empty() { "(no commit)" } else { &head_sha });

        // 2. Delete owned files (they're ashes; the spec is the source).
        println!("[{attempt_label}] Burning {} owned file(s)…", to_burn.len());
        for rel in &to_burn {
            let abs = root.join(rel);
            if abs.exists() {
                std::fs::remove_file(&abs)
                    .with_context(|| format!("deleting owned file {rel}"))?;
                println!("  del {rel}");
            }
        }

        // 3. Compose a regeneration prompt and run the agent.
        let config = load_harness_config(root)?;
        let regen_prompt = compose_regen_prompt(root, spec_name, &reqs, &to_burn)?;
        let (prompt_file, _) = crate::prompt::write_prompt_file(&regen_prompt)?;

        let cmd_str = config.agent.command.replace("{prompt_file}", &prompt_file.to_string_lossy());
        let working_dir = config.agent.working_dir.as_deref().unwrap_or(".");
        let wd = root.join(working_dir);

        println!("[{attempt_label}] Running agent for regeneration…");
        let status = if cfg!(windows) {
            Command::new("cmd").arg("/C").arg(&cmd_str).current_dir(&wd).status()
        } else {
            Command::new("sh").arg("-c").arg(&cmd_str).current_dir(&wd).status()
        }
        .with_context(|| format!("failed to launch agent: {cmd_str}"))?;
        let _ = std::fs::remove_file(&prompt_file);

        let agent_exit = status.code().unwrap_or(-1);
        if agent_exit != 0 {
            println!("[{attempt_label}] Agent exited {agent_exit} — rolling back.");
            let _ = git_reset_workdir_basic(root);
            return Ok((agent_exit, vec![]));
        }

        // 4. Run hooks (including evals from evals/<spec>/).
        let guardrails = crate::config::load_guardrails(root)?;
        let hooks_to_run = if config.hooks.default.is_empty() { vec![] } else { config.hooks.default.clone() };

        // Also collect eval scripts for this spec.
        let eval_dir = root.join("evals").join(spec_name);
        let mut eval_hooks: Vec<String> = Vec::new();
        if eval_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&eval_dir) {
                for e in entries.filter_map(|e| e.ok()) {
                    if e.path().is_file() {
                        if let Some(name) = e.file_name().to_str() {
                            eval_hooks.push(format!("evals/{spec_name}/{name}"));
                        }
                    }
                }
                eval_hooks.sort();
            }
        }

        let mut all_passed = true;
        let mut hook_log: Vec<String> = Vec::new();

        // Run standard hooks.
        let dummy_task = serde_json::json!({"id": "regen", "spec": spec_name}).to_string();
        for hook_name in &hooks_to_run {
            let inv = HookInvocation {
                hook_name: hook_name.clone(),
                task_id: "regen".to_string(),
                spec_name: spec_name.to_string(),
                iteration: 0,
                attempt: 0,
            };
            let timeout = crate::hooks::hook_timeout(&guardrails, hook_name, config.hooks.default_timeout_secs);
            let blocking = crate::hooks::is_hook_blocking(&guardrails, hook_name);
            match run_hook(root, &inv, &dummy_task, timeout) {
                Ok(outcome) => {
                    let passed = outcome.exit_code == 0 && !outcome.timed_out;
                    let entry = format!("  hook {hook_name}: {}", if passed { "PASS" } else { "FAIL" });
                    println!("{entry}");
                    hook_log.push(entry);
                    if blocking && !passed {
                        all_passed = false;
                        break;
                    }
                }
                Err(e) => {
                    println!("  hook {hook_name}: ERROR — {e:#}");
                    hook_log.push(format!("  hook {hook_name}: ERROR"));
                    if crate::hooks::is_hook_blocking(&guardrails, hook_name) {
                        all_passed = false;
                        break;
                    }
                }
            }
        }

        // Run eval scripts (Phase 2 oracle).
        if all_passed {
            for eval_path in &eval_hooks {
                let abs_eval = root.join(eval_path);
                let eval_status = Command::new(&abs_eval)
                    .current_dir(root)
                    .env("HARNESS_SPEC", spec_name)
                    .env("HARNESS_ROOT", root.to_string_lossy().to_string())
                    .status();
                match eval_status {
                    Ok(s) if s.success() => {
                        println!("  eval {eval_path}: PASS");
                        hook_log.push(format!("  eval {eval_path}: PASS"));
                    }
                    Ok(s) => {
                        let code = s.code().unwrap_or(-1);
                        println!("  eval {eval_path}: FAIL (exit {code}) — spec may be ambiguous");
                        hook_log.push(format!("  eval {eval_path}: FAIL"));
                        all_passed = false;
                        break;
                    }
                    Err(e) => {
                        println!("  eval {eval_path}: ERROR — {e:#}");
                        all_passed = false;
                        break;
                    }
                }
            }
        }

        if !all_passed {
            println!("[{attempt_label}] Gates failed — rolling back.");
            let _ = git_reset_workdir_basic(root);
            return Ok((-1, hook_log));
        }

        // 5. Phase 5: cross-model review gate for public_interface specs.
        if reqs.public_interface {
            if let Some(ref reviewer_cmd) = config.agent.reviewer_command {
                if !reviewer_cmd.is_empty() {
                    println!("[{attempt_label}] Running cross-model reviewer (public_interface spec)…");
                    let review_prompt = compose_review_prompt(spec_name, &reqs, &to_burn);
                    let (review_file, _) = crate::prompt::write_prompt_file(&review_prompt)?;
                    let rcmd = reviewer_cmd.replace("{prompt_file}", &review_file.to_string_lossy());
                    let rstatus = if cfg!(windows) {
                        Command::new("cmd").arg("/C").arg(&rcmd).current_dir(root).status()
                    } else {
                        Command::new("sh").arg("-c").arg(&rcmd).current_dir(root).status()
                    }
                    .with_context(|| "failed to launch reviewer")?;
                    let _ = std::fs::remove_file(&review_file);
                    if rstatus.code().unwrap_or(-1) != 0 {
                        println!("[{attempt_label}] Reviewer rejected regeneration — rolling back.");
                        let _ = git_reset_workdir_basic(root);
                        return Ok((-1, hook_log));
                    }
                    println!("[{attempt_label}] Reviewer accepted.");
                }
            }
        }

        // 6. Success: record manifest and commit.
        record_spec(root, spec_name)
            .unwrap_or_else(|e| eprintln!("warning: manifest record failed: {e:#}"));
        let commit_msg = format!("regen: {spec_name}");
        let _ = git_commit_all(root, &commit_msg);
        println!("[{attempt_label}] Regeneration complete.");
        Ok((0, hook_log))
    };

    let (exit1, _log1) = run_regen("regen")?;
    if exit1 != 0 {
        return Ok(exit1);
    }

    if twice {
        // Phase 5 determinism probe: regenerate a second time and compare eval results.
        println!("\n[regen-2] Second pass (determinism probe)…");
        let (exit2, log2) = run_regen("regen-2")?;
        if exit2 != 0 {
            eprintln!("warning: second regeneration failed — spec may be underspecified");
            println!("Determinism verdict: UNDERSPECIFIED (second pass failed)");
            return Ok(exit2);
        }
        println!("Determinism verdict: CONVERGED (both passes passed evals)");
        if !log2.is_empty() {
            println!("Second pass hooks:");
            for l in &log2 { println!("{l}"); }
        }
    }

    Ok(0)
}

fn git_head_sha(root: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn git_reset_workdir_basic(root: &Path) -> Result<()> {
    let has_head = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_head {
        return Ok(());
    }
    let _ = Command::new("git")
        .args(["checkout", "HEAD", "--", "."])
        .current_dir(root)
        .status();
    let _ = Command::new("git")
        .args(["clean", "-fd"])
        .current_dir(root)
        .status();
    Ok(())
}

fn git_commit_all(root: &Path, message: &str) -> Result<()> {
    let _ = Command::new("git").args(["add", "-A"]).current_dir(root).status();
    let _ = Command::new("git").args(["commit", "-m", message]).current_dir(root).status();
    Ok(())
}

fn compose_regen_prompt(
    root: &Path,
    spec_name: &str,
    reqs: &crate::spec::RequirementsFile,
    to_burn: &[String],
) -> Result<String> {
    let req_json = serde_json::to_string_pretty(&reqs.requirements).unwrap_or_default();
    let design_path = spec_dir(root, spec_name).join("2-design.md");
    let design = if design_path.exists() {
        std::fs::read_to_string(&design_path).unwrap_or_default()
    } else {
        String::new()
    };
    let files_list = to_burn.join("\n");
    Ok(format!(r#"# Regeneration Task: {spec_name}

> **This is a REGENERATION run. The files below have been deleted. Recreate them
> from scratch using ONLY the spec and design below. Do NOT consult any prior
> implementation — there is none. Do NOT modify .specs/, evals/, or .harness/.**

## Files to create
{files_list}

## Requirements
```json
{req_json}
```

## Design
{design}

## Rules
- Implement every requirement listed above.
- Create only the files listed in "Files to create".
- Leave the project buildable.
- Do not commit — the harness handles that.
"#))
}

fn compose_review_prompt(
    spec_name: &str,
    reqs: &crate::spec::RequirementsFile,
    regenerated_files: &[String],
) -> String {
    let req_json = serde_json::to_string_pretty(&reqs.requirements).unwrap_or_default();
    let files_list = regenerated_files.join("\n");
    format!(r#"# Cross-Model Review: {spec_name}

You are an independent reviewer. Check the regenerated files listed below against
the requirements. Exit 0 if the implementation satisfies every requirement.
Exit non-zero with a clear explanation if any requirement is unmet or the
implementation has correctness bugs.

## Regenerated files
{files_list}

## Requirements
```json
{req_json}
```

Be rigorous. The spec is the contract; the code must satisfy it exactly.
"#)
}

// ─── spec sync --against-code (Phase 4) ──────────────────────────────────────

/// Drive an agent to reconstruct the spec from owned code, then diff against
/// the canonical spec to produce a convergence verdict.
fn cmd_sync_against_code(root: &Path, spec_name: &str) -> Result<()> {
    let dir = spec_dir(root, spec_name);
    let reqs = load_requirements(&dir)?;

    if reqs.owns.is_empty() {
        eprintln!("note: spec '{spec_name}' has no 'owns' declaration — nothing to reconstruct from.");
        return Ok(());
    }

    let owned_files = crate::manifest::expand_owned_paths(root, &reqs.owns)?;
    if owned_files.is_empty() {
        println!("No owned files found on disk for spec '{spec_name}'.");
        return Ok(());
    }

    let out_dir = root.join(".harness").join("roundtrip");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(format!("{spec_name}.reconstructed.md"));

    // Compose the reconstruction prompt.
    let files_list = owned_files.join("\n");
    let prompt = format!(r#"# Spec Reconstruction: {spec_name}

Read the source files listed below and reconstruct a spec for them in Markdown.
Do NOT read or reference any existing spec files under .specs/.
Write the reconstructed spec to: {out}

Your output must cover:
1. What each file does and why.
2. The public interfaces and data contracts.
3. Any behavior that is NOT documented in the obvious function/type names.
4. Edge cases and invariants you observe.

## Files to read and analyse
{files_list}

Write ONLY to the output path above. Do not modify any other file.
"#, out = out_path.display());

    let config = load_harness_config(root)?;
    let (prompt_file, _) = crate::prompt::write_prompt_file(&prompt)?;
    let cmd_str = config.agent.command.replace("{prompt_file}", &prompt_file.to_string_lossy());
    let working_dir = config.agent.working_dir.as_deref().unwrap_or(".");
    let wd = root.join(working_dir);

    println!("Running reconstruction agent for '{spec_name}'…");
    let status = if cfg!(windows) {
        Command::new("cmd").arg("/C").arg(&cmd_str).current_dir(&wd).status()
    } else {
        Command::new("sh").arg("-c").arg(&cmd_str).current_dir(&wd).status()
    }
    .with_context(|| "failed to launch reconstruction agent")?;
    let _ = std::fs::remove_file(&prompt_file);

    if status.code().unwrap_or(-1) != 0 {
        anyhow::bail!("reconstruction agent failed");
    }

    if !out_path.exists() {
        anyhow::bail!(
            "agent did not write reconstruction to {}",
            out_path.display()
        );
    }

    println!("Reconstruction written to {}", out_path.display());

    // Diff the reconstruction against the canonical spec.
    let canonical_req = std::fs::read_to_string(dir.join("1-requirements.json")).unwrap_or_default();
    let canonical_design = std::fs::read_to_string(dir.join("2-design.md")).unwrap_or_default();
    let reconstructed = std::fs::read_to_string(&out_path).unwrap_or_default();

    // Simple heuristic convergence check: look for requirement IDs in the reconstruction.
    let req_ids: Vec<&str> = reqs.requirements.iter()
        .map(|r| r.id.as_str())
        .collect();

    let mut missing_in_reconstruction: Vec<&str> = Vec::new();
    for id in &req_ids {
        if !reconstructed.contains(id) {
            missing_in_reconstruction.push(id);
        }
    }

    // Look for concepts in reconstruction not in the canonical spec (hidden behavior).
    // A simple proxy: paragraphs in reconstruction that don't reference any req ID.
    let has_hidden_behavior = reconstructed.lines().any(|line| {
        let mentions_req = req_ids.iter().any(|id| line.contains(id));
        !mentions_req && !line.trim().is_empty() && !line.starts_with('#')
    });

    println!("\n=== Convergence Report: {spec_name} ===");
    if missing_in_reconstruction.is_empty() && !has_hidden_behavior {
        println!("Verdict: CONVERGED");
        println!("  All requirement IDs appear in the reconstruction.");
        println!("  No obvious hidden behavior detected.");
    } else {
        println!("Verdict: DRIFT/UNDERSPECIFIED");
        if !missing_in_reconstruction.is_empty() {
            println!("  Implementation drift — code does not reflect these requirements:");
            for id in &missing_in_reconstruction {
                println!("    {id}");
            }
        }
        if has_hidden_behavior {
            println!("  Hidden behavior — reconstruction describes things the spec doesn't:");
            println!("    Review {} for undocumented behavior.", out_path.display());
        }
    }

    // Remind the user that this is advisory.
    if !canonical_req.is_empty() && !canonical_design.is_empty() {
        println!(
            "\nFull diff: diff {:?} {:?} {:?}",
            dir.join("1-requirements.json"),
            dir.join("2-design.md"),
            out_path
        );
    }
    println!("Note: reconstruction output is advisory — human review required before --write.");
    Ok(())
}

// ─── scaffold templates ──────────────────────────────────────────────────────

/// Claude Code subagent definition, installed to .claude/agents/ by `init`.
const SETUP_AGENT: &str = include_str!("../templates/harness-setup.md");

/// Prompt template used by `spec draft` to drive the agent.
const DRAFT_SPEC_PROMPT: &str = include_str!("../templates/draft-spec.md");

const HARNESS_TOML: &str = r#"[agent]
command = "claude -p --dangerously-skip-permissions < {prompt_file}"
working_dir = "."
reviewer_command = ""

[loop]
max_iterations = 50
commit_each_success = true
commit_message_template = "harness: {task_id} {task_title}"
stop_when_no_tasks = true
reset_on_failure = true       # restore the tree to last commit after a failed iteration

# Optional: enable multi-phase SDLC execution. When set, each task passes
# through these phases in order before it is marked Done. Remove or leave
# empty to use the original single-phase behaviour.
#
# phase_sequence = ["plan", "test", "dev"]

[prompts]
loop = ".harness/prompts/loop.md"
init = ".harness/prompts/init.md"

[hooks]
default = ["run_build", "run_lint", "run_unit_tests", "run_update_docs"]
default_timeout_secs = 600

# ── Phase-specific config (only needed when phase_sequence is set above) ──────
# Each [phases.<name>] section overrides the agent command, prompt template,
# and/or hook list for that phase. All fields are optional; omitted fields
# fall back to the defaults above.
#
# [phases.plan]
# prompt_template = ".harness/prompts/plan.md"
# hooks            = ["validate_plan"]
#
# [phases.test]
# prompt_template = ".harness/prompts/test.md"
# hooks            = ["run_lint", "validate_tests"]
#
# [phases.dev]
# hooks            = ["run_build", "run_lint", "run_unit_tests"]
#
# To use a different agent model per phase, set agent_command:
# [phases.plan]
# agent_command = "claude -p --model claude-opus-4-8 --dangerously-skip-permissions < {prompt_file}"
"#;

const PRE_COMMIT_HOOK: &str = r#"#!/usr/bin/env sh
# Installed by `harness init` — gates commits on manifest cleanliness.
# Fails if any spec-owned file was hand-edited or the spec changed without regen.
harness manifest check --all
"#;

const GUARDRAILS_TOML: &str = r#"[budgets]
max_attempts_per_task = 3
max_iterations = 50

[writes]
allow = ["src/**", "tests/**", "docs/**"]
deny = [
  ".specs/**",              # spec files are harness-managed; agents must not alter them
  "evals/**",               # eval oracles are human-authored; never agent-writable
  ".harness/manifest.json", # manifest is maintained by the harness
  ".harness/harness.toml",
  ".harness/guardrails/**",
  ".harness/prompts/**",
  ".harness/scripts/**",
  ".git/**",
  "**/secrets*",
  "**/.env*",
]

# Additional paths that are always off-limits for agent writes.
# [protected]
# paths = []

# Set to true to enforce task-level write boundaries when tasks declare files_hint.
# When enabled, any file the agent writes outside files_hint ∪ writes.allow fails the iteration.
enforce_ownership = false

[operations]
deny_destructive = true

[hooks.run_e2e_tests]
blocking = false
timeout_secs = 1800
"#;

const RULES_MD: &str = r#"# Project Constraints

- Only modify files within the allowed write paths (src/, tests/, docs/).
- **Never modify files under `.specs/`** — spec files are the harness's source of
  truth and are managed exclusively by the harness operator, not by agents.
- **Never modify files under `.harness/`** except appending to
  `.harness/logs/progress.md` as instructed. Do not touch harness.toml,
  guardrails/, prompts/, or scripts/.
- Do not modify .git/ or any secrets / .env files.
- Leave the project buildable after every change.
- Do not commit directly; the harness handles commits.
"#;

const LOOP_MD: &str = r#"# Harness Loop Prompt

## Constraints
{rules}

> **HARD RULE — DO NOT TOUCH SPEC OR HARNESS FILES**
> You must never create, edit, or delete any file under `.specs/` or `.harness/`
> (except appending a summary line to `.harness/logs/progress.md`).
> The harness enforces this mechanically: any write to those paths will cause the
> iteration to fail and your changes to be reverted.

## Progress so far
{progress}

## Current task
**ID:** {task_id}
**Spec:** {spec_name}
**Title:** {task_title}
{phase_name}
**Acceptance criteria:**
{task_acceptance}

**Files hint:** {task_files_hint}

## Requirements context
{requirements}

## Design context
{design_excerpt}

## Your task
Do ONLY the task above. Leave the project in a buildable state when you stop.
Update .harness/logs/progress.md with a short summary of what you did.
Do not run tests or builds yourself — the harness will validate your work.
"#;

const INIT_MD: &str = r#"# Harness Initializer Prompt

This is the first iteration of a fresh harness. Set up any environment
prerequisites (dependencies, tooling) needed for subsequent iterations.

{rules}

## Current task
**ID:** {task_id}
**Title:** {task_title}

{task_acceptance}
"#;

const HOOK_STUB: &str = r#"#!/usr/bin/env sh
# Hook: <NAME>
# Exit 0 to pass, non-zero to fail the iteration.
# Env: HARNESS_HOOK, HARNESS_TASK_ID, HARNESS_SPEC, HARNESS_ITERATION, HARNESS_ATTEMPT, HARNESS_ROOT
# Stdin: JSON task payload.

echo "Hook <NAME> not yet implemented" >&2
exit 0
"#;
