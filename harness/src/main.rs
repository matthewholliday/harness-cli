mod config;
mod hooks;
mod loop_runner;
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
        SpecCmd::Draft { name, brief, from, .. } => {
            cmd_spec_draft(root, &name, brief, from)
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
        SpecCmd::Sync { name, write, regen_tasks, .. } => {
            cmd_sync(root, &name, write, regen_tasks)?;
            Ok(0)
        }
    }
}

fn cmd_spec_draft(
    root: &Path,
    name: &str,
    brief: Option<String>,
    from: Option<PathBuf>,
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
        (None, None) => {
            // Fall back to interactive: read from stdin with a prompt.
            eprintln!("Brief (describe what this spec should do; end with Ctrl-D):");
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("failed to read brief from stdin")?;
            if s.trim().is_empty() {
                anyhow::bail!(
                    "no brief supplied — use --brief \"...\" or --from <file> or pipe to stdin"
                );
            }
            s
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

    Ok(if ok { 0 } else { 1 })
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

const GUARDRAILS_TOML: &str = r#"[budgets]
max_attempts_per_task = 3
max_iterations = 50

[writes]
allow = ["src/**", "tests/**", "docs/**"]
deny = [
  ".specs/**",              # spec files are harness-managed; agents must not alter them
  ".harness/harness.toml",
  ".harness/guardrails/**",
  ".harness/prompts/**",
  ".harness/scripts/**",
  ".git/**",
  "**/secrets*",
  "**/.env*",
]

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
