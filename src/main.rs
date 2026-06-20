mod config;
mod hooks;
mod loop_runner;
mod prompt;
mod spec;
mod state;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::config::{find_project_root, load_harness_config};
use crate::hooks::{run_hook, HookInvocation};
use crate::loop_runner::{run, RunOptions};
use crate::spec::{list_specs, load_requirements, load_tasks, spec_dir, TaskStatus};
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
    /// Inspect iteration/hook logs.
    Logs {
        #[arg(long)]
        iteration: Option<u64>,
        #[arg(long)]
        follow: bool,
    },
    /// Validate config, hooks, agent adapter, git.
    Doctor,
}

#[derive(Subcommand)]
enum SpecCmd {
    List,
    Draft {
        name: String,
        #[arg(long)]
        interactive: bool,
        #[arg(long)]
        from: Option<PathBuf>,
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
        Commands::Logs { iteration, follow } => {
            let root = find_project_root()?;
            cmd_logs(&root, iteration, follow)
        }
        Commands::Doctor => {
            let root = find_project_root()?;
            cmd_doctor(&root)
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
        SpecCmd::Draft { name, .. } => {
            println!(
                "Spec drafting is agent-assisted and not yet automated in this build.\n\
                 To draft '{name}' manually:\n  \
                 1. mkdir -p .specs/{name}\n  \
                 2. create 1-requirements.json, 2-design.md, 3-tasks.jsonl\n  \
                 3. run `harness spec validate {name}`"
            );
            Ok(0)
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
        SpecCmd::Sync { name, .. } => {
            cmd_sync(root, &name)?;
            Ok(0)
        }
    }
}

fn validate_spec(root: &Path, name: &str) -> Result<()> {
    let dir = spec_dir(root, name);

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
    }
    detect_cycle(&tasks)?;
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
    ) -> Result<()> {
        color.insert(node, 1);
        if let Some(deps) = graph.get(node) {
            for d in deps.iter() {
                match color.get(d.as_str()).copied().unwrap_or(0) {
                    1 => anyhow::bail!("dependency cycle detected at task {d}"),
                    0 => {
                        if let Some((k, _)) = graph.get_key_value(d.as_str()) {
                            visit(k, graph, color)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        color.insert(node, 2);
        Ok(())
    }
    let mut color: HashMap<&str, u8> = HashMap::new();
    let keys: Vec<&str> = graph.keys().copied().collect();
    for k in keys {
        if color.get(k).copied().unwrap_or(0) == 0 {
            visit(k, &graph, &mut color)?;
        }
    }
    Ok(())
}

fn cmd_sync(root: &Path, name: &str) -> Result<()> {
    let dir = spec_dir(root, name);
    let reqs = load_requirements(&dir)?;
    let tasks = load_tasks(&dir)?;

    let req_ids: std::collections::HashSet<String> =
        reqs.requirements.iter().map(|r| r.id.clone()).collect();
    let covered: std::collections::HashSet<String> =
        tasks.iter().flat_map(|t| t.requirements.clone()).collect();

    println!("Drift report for '{name}':");
    let mut clean = true;
    for id in &req_ids {
        if !covered.contains(id) {
            println!("  ! requirement {id} has no task");
            clean = false;
        }
    }
    for t in &tasks {
        for r in &t.requirements {
            if !req_ids.contains(r) {
                println!("  ! task {} references unknown requirement {r}", t.id);
                clean = false;
            }
        }
    }
    if clean {
        println!("  (no drift detected)");
    }
    println!("\nNote: sync is read-only in this build; --write/--regen-tasks not yet implemented.");
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
    println!("active spec:     {:?}", state.active_spec);
    println!("iteration count: {}", state.iteration_count);
    println!(
        "last task:       {:?} ({:?})",
        state.last_task_id, state.last_task_status
    );

    let (mut todo, mut prog, mut blocked, mut done) = (0, 0, 0, 0);
    for s in list_specs(root)? {
        for t in load_tasks(&spec_dir(root, &s))? {
            match t.status {
                TaskStatus::Todo => todo += 1,
                TaskStatus::InProgress => prog += 1,
                TaskStatus::Blocked => blocked += 1,
                TaskStatus::Done => done += 1,
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
        let hooks_dir = root.join(".harness").join("scripts").join("hooks");
        for h in &c.hooks.default {
            let exists = [h.clone(), format!("{h}.ps1"), format!("{h}.cmd"), format!("{h}.bat")]
                .iter()
                .any(|cand| hooks_dir.join(cand).exists());
            check!(format!("hook '{h}' present"), exists, "create the hook stub");
        }
    }

    let specs = list_specs(root).unwrap_or_default();
    check!("at least one spec", !specs.is_empty(), "run `harness spec draft <name>`");

    check!("git repository", root.join(".git").exists(), "run `git init` for rollback safety");

    Ok(if ok { 0 } else { 1 })
}

// ─── scaffold templates ──────────────────────────────────────────────────────

/// Claude Code subagent definition, installed to .claude/agents/ by `init`.
const SETUP_AGENT: &str = include_str!("../templates/harness-setup.md");

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

[prompts]
loop = ".harness/prompts/loop.md"
init = ".harness/prompts/init.md"

[hooks]
default = ["run_build", "run_lint", "run_unit_tests", "run_update_docs"]
default_timeout_secs = 600
"#;

const GUARDRAILS_TOML: &str = r#"[budgets]
max_attempts_per_task = 3
max_iterations = 50

[writes]
allow = ["src/**", "tests/**", "docs/**", ".specs/**"]
deny = [".harness/guardrails/**", ".git/**", "**/secrets*", "**/.env*"]

[operations]
deny_destructive = true

[hooks.run_e2e_tests]
blocking = false
timeout_secs = 1800
"#;

const RULES_MD: &str = r#"# Project Constraints

- Only modify files within the allowed write paths.
- Do not modify .harness/guardrails/ or any secrets files.
- Leave the project buildable after every change.
- Do not commit directly; the harness handles commits.
"#;

const LOOP_MD: &str = r#"# Harness Loop Prompt

## Constraints
{rules}

## Progress so far
{progress}

## Current task
**ID:** {task_id}
**Spec:** {spec_name}
**Title:** {task_title}

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
