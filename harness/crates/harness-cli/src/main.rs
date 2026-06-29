mod tui;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};

use harness_core::config::{find_project_root, load_harness_config};
use harness_core::hooks::{run_hook, HookInvocation};
use harness_core::loop_runner::{run, RunOptions};
use harness_core::manifest::{check_spec, record_spec, DriftKind};
use harness_core::prompt::compose_prompt;
use harness_core::spec::{
    list_specs, load_requirements, load_tasks, save_tasks, spec_dir, Task, TaskStatus,
};
use harness_core::state::load_state;

#[derive(Parser)]
#[command(
    name = "harness",
    version,
    about = "A project-agnostic Ralph-loop agent harness"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold .harness/ in the current directory.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Render code from a spec, task by task (incremental, non-destructive).
    Build {
        /// Spec to build. Omit (or use --all) to build every spec.
        spec: Option<String>,
        #[arg(long)]
        all: bool,
        /// Run a single iteration; surface agent failure as exit 3.
        #[arg(long)]
        once: bool,
        /// Maximum number of iterations.
        #[arg(long)]
        max: Option<u64>,
        /// Preview task selection without invoking the agent.
        #[arg(long)]
        dry_run: bool,
    },
    /// Burn a spec's owned files and re-render from the spec (destructive, eval-gated).
    Rebuild {
        /// Spec to rebuild. Required unless --all is given.
        spec: Option<String>,
        #[arg(long)]
        all: bool,
        /// Only burn/rebuild files matching this glob (subset of spec ownership).
        #[arg(long)]
        only: Option<String>,
        /// Override the pace_layer="never" guard.
        #[arg(long)]
        force: bool,
    },
    /// The invariant gate: spec well-formed + eval coverage + no drift.
    Check {
        /// Spec to check. Omit to check all specs.
        spec: Option<String>,
        #[arg(long)]
        all: bool,
        /// Reconstruct the spec from code and report convergence (advisory).
        #[arg(long)]
        reverse: bool,
        /// Rebuild twice and compare eval results (spec-tightness probe; destructive).
        #[arg(long)]
        determinism: bool,
        /// Accept the current code as this spec's baseline (escape hatch).
        #[arg(long)]
        accept: bool,
    },
    /// Author and inspect specs (the source of truth).
    Spec {
        #[command(subcommand)]
        cmd: SpecCmd,
    },
    /// Manage and run evals (the acceptance oracle).
    Eval {
        #[command(subcommand)]
        cmd: EvalCmd,
    },
    /// Manage and run gates (blocking validation hooks).
    Gate {
        #[command(subcommand)]
        cmd: GateCmd,
    },
    /// Show current loop status.
    Status,
    /// Live terminal dashboard that watches a run as it happens.
    Watch,
    /// Inspect iteration logs.
    Log {
        /// Iteration number to show. Omit to list all.
        n: Option<u64>,
        #[arg(short, long)]
        follow: bool,
    },
    /// Validate environment and config (agent adapter, gates, git).
    Doctor,
    /// Preview the exact prompt the agent would receive for a task (no run).
    Explain {
        /// Task id to preview (e.g. T-003).
        task: String,
        /// Spec to look in. Omit to search all specs.
        #[arg(long)]
        spec: Option<String>,
        /// Override the phase whose prompt to compose (default: next pending phase).
        #[arg(long)]
        phase: Option<String>,
    },

    // ── deprecated aliases (hidden) ──────────────────────────────────────────
    #[command(hide = true)]
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
    #[command(hide = true)]
    Regen {
        spec: String,
        #[arg(long)]
        component: Option<String>,
        #[arg(long)]
        twice: bool,
        #[arg(long)]
        force_boundary: bool,
    },
    #[command(hide = true)]
    Manifest {
        #[command(subcommand)]
        cmd: ManifestCmd,
    },
    #[command(hide = true)]
    Hooks {
        #[command(subcommand)]
        cmd: GateCmd,
    },
    #[command(hide = true)]
    Logs {
        #[arg(long)]
        iteration: Option<u64>,
        #[arg(long)]
        follow: bool,
    },
}

#[derive(ValueEnum, Clone, Copy)]
enum SpecPart {
    Requirements,
    Design,
    Tasks,
}

#[derive(Subcommand)]
enum SpecCmd {
    /// List specs under .specs/.
    Ls,
    /// Draft a new spec from a brief (agent-assisted).
    New {
        name: String,
        /// Inline brief: what the spec should do (quoted string).
        #[arg(long)]
        brief: Option<String>,
        /// Brief file (.md/.txt/any text), or `-` for stdin.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Open a spec file in $EDITOR, then check it.
    Edit {
        name: String,
        /// Which part to edit (default: requirements).
        part: Option<SpecPart>,
    },
    /// Print a spec's resolved contents.
    Show { name: String },
    /// Report requirement↔task coverage; --fix writes task stubs.
    Tasks {
        name: String,
        #[arg(long)]
        fix: bool,
    },

    // ── deprecated aliases (hidden) ──────────────────────────────────────────
    #[command(hide = true)]
    List,
    #[command(hide = true)]
    Draft {
        name: String,
        #[arg(long)]
        brief: Option<String>,
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long)]
        interactive: bool,
    },
    #[command(hide = true)]
    Validate {
        name: Option<String>,
        #[arg(long)]
        all: bool,
    },
    #[command(hide = true)]
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
enum EvalCmd {
    /// List eval scripts for a spec.
    Ls { spec: String },
    /// Run a spec's evals against the current code.
    Run { spec: String },
    /// Draft eval stubs from a spec's requirement acceptance criteria.
    ///
    /// Produces one reviewable `evals/<spec>/REQ-NNN-*.sh` stub per requirement.
    /// The output is a DRAFT, not an oracle: every stub is marked with a TODO and
    /// must be reviewed by a human before it can be trusted. By default the draft
    /// is produced by the configured reviewer model (if any) rather than the code
    /// agent, to keep the oracle independent of whatever writes the code.
    Draft {
        spec: String,
        /// Overwrite eval scripts that already exist.
        #[arg(long)]
        force: bool,
        /// Use the primary code agent instead of the reviewer model to draft.
        #[arg(long)]
        use_code_agent: bool,
    },
}

#[derive(Subcommand)]
enum ManifestCmd {
    Record {
        spec: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Check {
        spec: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum GateCmd {
    /// List gate scripts.
    Ls,
    /// Verify gate scripts exist, are executable, and are all wired up.
    Check,
    /// Run one gate manually.
    Run {
        gate: String,
        #[arg(long)]
        task: Option<String>,
    },
    #[command(hide = true)]
    List,
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

fn deprecate(old: &str, new: &str) {
    eprintln!("note: '{old}' is deprecated — use '{new}' (see `harness --help`)");
}

/// Resolve the set of specs a command should act on.
/// `default_all` controls the no-argument behaviour: read-only commands default
/// to every spec; destructive commands require an explicit target.
fn resolve_specs(
    root: &Path,
    spec: Option<String>,
    all: bool,
    default_all: bool,
) -> Result<Vec<String>> {
    if all {
        list_specs(root)
    } else if let Some(name) = spec {
        Ok(vec![name])
    } else if default_all {
        list_specs(root)
    } else {
        anyhow::bail!("provide a spec name or --all");
    }
}

fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Commands::Init { force } => {
            let root = std::env::current_dir()?;
            cmd_init(&root, false, force)?;
            Ok(0)
        }
        Commands::Build {
            spec,
            all,
            once,
            max,
            dry_run,
        } => {
            let root = find_project_root()?;
            cmd_build(&root, spec, all, once, max, dry_run)
        }
        Commands::Rebuild {
            spec,
            all,
            only,
            force,
        } => {
            let root = find_project_root()?;
            cmd_rebuild(&root, spec, all, only.as_deref(), force, false)
        }
        Commands::Check {
            spec,
            all,
            reverse,
            determinism,
            accept,
        } => {
            let root = find_project_root()?;
            cmd_check(&root, spec, all, reverse, determinism, accept)
        }
        Commands::Spec { cmd } => {
            let root = find_project_root()?;
            cmd_spec(&root, cmd)
        }
        Commands::Eval { cmd } => {
            let root = find_project_root()?;
            cmd_eval(&root, cmd)
        }
        Commands::Gate { cmd } => {
            let root = find_project_root()?;
            cmd_gate(&root, cmd)
        }
        Commands::Status => {
            let root = find_project_root()?;
            cmd_status(&root)
        }
        Commands::Watch => {
            let root = find_project_root()?;
            tui::run(&root)
        }
        Commands::Log { n, follow } => {
            let root = find_project_root()?;
            cmd_logs(&root, n, follow)
        }
        Commands::Doctor => {
            let root = find_project_root()?;
            cmd_doctor(&root)
        }
        Commands::Explain { task, spec, phase } => {
            let root = find_project_root()?;
            cmd_explain(&root, &task, spec.as_deref(), phase.as_deref())
        }

        // ── deprecated aliases ───────────────────────────────────────────────
        Commands::Run {
            spec,
            once,
            max_iterations,
            dry_run,
        } => {
            deprecate("run", "build");
            let root = find_project_root()?;
            cmd_build(&root, spec, false, once, max_iterations, dry_run)
        }
        Commands::Regen {
            spec,
            component,
            twice,
            force_boundary,
        } => {
            deprecate("regen", "rebuild");
            let root = find_project_root()?;
            cmd_rebuild(
                &root,
                Some(spec),
                false,
                component.as_deref(),
                force_boundary,
                twice,
            )
        }
        Commands::Manifest { cmd } => {
            let root = find_project_root()?;
            match cmd {
                ManifestCmd::Record { spec, all } => {
                    deprecate("manifest record", "check --accept");
                    cmd_check(&root, spec, all, false, false, true)
                }
                ManifestCmd::Check { spec, all } => {
                    deprecate("manifest check", "check");
                    cmd_check(&root, spec, all, false, false, false)
                }
            }
        }
        Commands::Hooks { cmd } => {
            deprecate("hooks", "gate");
            let root = find_project_root()?;
            cmd_gate(&root, cmd)
        }
        Commands::Logs { iteration, follow } => {
            deprecate("logs", "log");
            let root = find_project_root()?;
            cmd_logs(&root, iteration, follow)
        }
    }
}

// ─── build / rebuild / check ─────────────────────────────────────────────────

fn cmd_build(
    root: &Path,
    spec: Option<String>,
    all: bool,
    once: bool,
    max: Option<u64>,
    dry_run: bool,
) -> Result<i32> {
    // `--all` (or no spec) means "every in-scope spec" → no filter.
    let spec_filter = if all { None } else { spec };
    run(
        root,
        RunOptions {
            spec_filter,
            once,
            max_iterations: max,
            dry_run,
        },
    )
}

/// Burn & re-render one or more specs. `twice` runs the determinism probe.
fn cmd_rebuild(
    root: &Path,
    spec: Option<String>,
    all: bool,
    only: Option<&str>,
    force: bool,
    twice: bool,
) -> Result<i32> {
    let specs = resolve_specs(root, spec, all, false)?;
    let mut worst = 0;
    for name in &specs {
        let code = cmd_regen(root, name, only, twice, force)?;
        if code != 0 {
            worst = code;
        }
    }
    Ok(worst)
}

fn print_drift(name: &str, drift: &DriftKind) {
    match drift {
        DriftKind::Unrecorded { .. } => println!(
            "✗ {name}: no baseline — run `harness rebuild {name}` or `harness check {name} --accept`"
        ),
        DriftKind::StaleCode { .. } => {
            println!("✗ {name}: spec changed, code not rebuilt — run `harness rebuild {name}`")
        }
        DriftKind::CodeDrift { path } => {
            println!("✗ {name}: hand-edit detected in owned file: {path}")
        }
        DriftKind::Missing { path } => {
            println!("✗ {name}: owned file missing: {path} — run `harness rebuild {name}`")
        }
    }
}

fn cmd_check(
    root: &Path,
    spec: Option<String>,
    all: bool,
    reverse: bool,
    determinism: bool,
    accept: bool,
) -> Result<i32> {
    let specs = resolve_specs(root, spec, all, true)?;
    if specs.is_empty() {
        println!("No specs found under .specs/");
        return Ok(0);
    }

    // Accept: adopt current code as the baseline (escape hatch / migration).
    if accept {
        for name in &specs {
            record_spec(root, name).with_context(|| format!("recording baseline for '{name}'"))?;
            println!("✓ accepted current code as baseline for '{name}'");
        }
        return Ok(0);
    }

    // Reverse: reconstruct the spec from code and emit a convergence verdict.
    if reverse {
        for name in &specs {
            cmd_sync_against_code(root, name)?;
        }
        return Ok(0);
    }

    // Determinism: rebuild twice and compare eval results (destructive).
    if determinism {
        let mut worst = 0;
        for name in &specs {
            let code = cmd_regen(root, name, None, true, false)?;
            if code != 0 {
                worst = code;
            }
        }
        return Ok(worst);
    }

    // Default: spec well-formedness + eval coverage + manifest drift.
    let mut ok = true;
    for name in &specs {
        let mut spec_ok = true;
        if let Err(e) = validate_spec(root, name) {
            eprintln!("✗ {name}: {e:#}");
            spec_ok = false;
        }
        match check_spec(root, name) {
            Ok(result) if result.is_clean() => {}
            Ok(result) => {
                spec_ok = false;
                for drift in &result.drifts {
                    print_drift(name, drift);
                }
            }
            Err(e) => {
                spec_ok = false;
                println!("✗ {name}: check error: {e:#}");
            }
        }
        if spec_ok {
            println!("✓ {name}: consistent");
        } else {
            ok = false;
        }
    }
    Ok(if ok { 0 } else { 2 })
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
        ("prompts/draft-eval.md", DRAFT_EVAL_PROMPT),
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
    for name in [
        "run_build",
        "run_unit_tests",
        "run_e2e_tests",
        "run_lint",
        "run_update_docs",
    ] {
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

    // Phase 0: install a git pre-commit hook that runs `harness check --all`.
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
        SpecCmd::Ls | SpecCmd::List => {
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
        SpecCmd::New { name, brief, from } => cmd_spec_draft(root, &name, brief, from, false),
        SpecCmd::Draft {
            name,
            brief,
            from,
            interactive,
        } => {
            deprecate("spec draft", "spec new");
            cmd_spec_draft(root, &name, brief, from, interactive)
        }
        SpecCmd::Edit { name, part } => cmd_spec_edit(root, &name, part),
        SpecCmd::Show { name } => cmd_spec_show(root, &name),
        SpecCmd::Tasks { name, fix } => {
            cmd_sync(root, &name, fix, false)?;
            Ok(0)
        }
        SpecCmd::Validate { name, all } => {
            deprecate("spec validate", "check");
            cmd_check(root, name, all, false, false, false)
        }
        SpecCmd::Sync {
            name,
            write,
            regen_tasks,
            against_code,
        } => {
            deprecate(
                "spec sync",
                if against_code {
                    "check --reverse"
                } else {
                    "spec tasks"
                },
            );
            if against_code {
                cmd_sync_against_code(root, &name)?;
            } else {
                cmd_sync(root, &name, write, regen_tasks)?;
            }
            Ok(0)
        }
    }
}

fn cmd_spec_edit(root: &Path, name: &str, part: Option<SpecPart>) -> Result<i32> {
    let dir = spec_dir(root, name);
    let file = match part.unwrap_or(SpecPart::Requirements) {
        SpecPart::Requirements => dir.join("1-requirements.json"),
        SpecPart::Design => dir.join("2-design.md"),
        SpecPart::Tasks => dir.join("3-tasks.jsonl"),
    };
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    if let Err(e) = Command::new(&editor).arg(&file).status() {
        eprintln!("failed to launch editor '{editor}': {e}");
    }
    match validate_spec(root, name) {
        Ok(_) => Ok(0),
        Err(e) => {
            eprintln!("✗ {name}: {e:#}");
            Ok(1)
        }
    }
}

fn cmd_spec_show(root: &Path, name: &str) -> Result<i32> {
    let dir = spec_dir(root, name);
    if !dir.exists() {
        anyhow::bail!("spec '{name}' not found");
    }
    for f in ["1-requirements.json", "2-design.md", "3-tasks.jsonl"] {
        println!("── {f} ──");
        match std::fs::read_to_string(dir.join(f)) {
            Ok(s) => println!("{}", s.trim_end()),
            Err(_) => println!("(missing)"),
        }
        println!();
    }
    Ok(0)
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
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || name.starts_with('-')
    {
        anyhow::bail!("spec name must match ^[a-z0-9][a-z0-9-]*$ — got '{name}'");
    }

    // ── 2. Read the brief ─────────────────────────────────────────────────────
    let brief_text = match (brief, from) {
        (Some(b), _) => b,
        (None, Some(path)) if path == *"-" => {
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
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    // ── 4. Compose the drafting prompt ────────────────────────────────────────
    // Prefer the project-local override so users can customise without
    // rebuilding the binary; fall back to the compiled-in template.
    let local_template_path = root.join(".harness").join("prompts").join("draft-spec.md");
    let template = if local_template_path.exists() {
        std::fs::read_to_string(&local_template_path)
            .unwrap_or_else(|_| DRAFT_SPEC_PROMPT.to_string())
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
    println!(
        "(The agent will write .specs/{name}/1-requirements.json, 2-design.md, 3-tasks.jsonl)\n"
    );

    let status = if cfg!(windows) {
        Command::new("cmd")
            .arg("/C")
            .arg(&cmd_str)
            .current_dir(&wd)
            .status()
    } else {
        Command::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .current_dir(&wd)
            .status()
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
            println!("  harness check {name}                # re-check any edits");
            println!("  harness spec tasks {name}           # requirement↔task coverage");
            println!("  harness build {name} --dry-run --once");
            println!("  harness build {name}");
            Ok(0)
        }
        Err(e) => {
            eprintln!("✗ validation failed: {e:#}");
            eprintln!("\nFix the issues above and re-run:");
            eprintln!("  harness check {name}");
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
            anyhow::bail!(
                "requirement {} has no acceptance criteria (not testable)",
                r.id
            );
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
            let exists = [
                h.clone(),
                format!("{h}.ps1"),
                format!("{h}.cmd"),
                format!("{h}.bat"),
            ]
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
                        t.id,
                        p
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
        // Read every eval file (name + contents) once, rather than re-scanning
        // the directory per requirement.
        let eval_texts: Vec<String> = std::fs::read_dir(&evals_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let body = std::fs::read_to_string(e.path()).unwrap_or_default();
                format!("{name}\n{body}")
            })
            .collect();

        for req in &reqs.requirements {
            // A requirement is covered only if its id appears as a whole token —
            // `R-1` must not be satisfied by an incidental `R-10` or `R-12`.
            let has_eval = eval_texts
                .iter()
                .any(|text| references_id(text, req.id.as_str()));
            if !has_eval {
                anyhow::bail!(
                    "requirement {} has no eval in evals/{name}/ — \
                     add an eval script or stub referencing '{}'",
                    req.id,
                    req.id
                );
            }
        }
    }

    Ok(())
}

/// True if `id` appears in `haystack` as a whole token, i.e. not immediately
/// adjacent to another identifier character. Requirement ids contain hyphens
/// (`R-1`), so a hyphen/alphanumeric/underscore on either side means we matched
/// a *longer* id (`R-10`) and should not count it as coverage.
fn references_id(haystack: &str, id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    let is_id_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(id) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_id_char(haystack[..abs].chars().next_back().unwrap());
        let after_idx = abs + id.len();
        let after_ok =
            after_idx >= bytes.len() || !is_id_char(haystack[after_idx..].chars().next().unwrap());
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn detect_cycle(tasks: &[harness_core::spec::Task]) -> Result<()> {
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

    let uncovered: Vec<_> = reqs
        .requirements
        .iter()
        .filter(|r| !covered.contains(&r.id))
        .collect();
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

        for (next_num, req) in (max_num + 1..).zip(uncovered.iter()) {
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
                last_failure: None,
                phases: vec![],
                completed_phases: vec![],
                created_at: now,
                updated_at: now,
            };
            println!("  + T-{:03} for {}", next_num, req.id);
            tasks.push(task);
        }
        save_tasks(&dir, &tasks)?;
        println!(
            "  wrote {} new task stub(s) to .specs/{name}/3-tasks.jsonl",
            uncovered.len()
        );
    }

    if regen_tasks {
        eprintln!("note: --regen-tasks (full task regeneration from requirements) is not yet implemented.");
    }

    Ok(())
}

// ─── hooks ─────────────────────────────────────────────────────────────────

fn cmd_gate(root: &Path, cmd: GateCmd) -> Result<i32> {
    match cmd {
        GateCmd::Ls | GateCmd::List => {
            let dir = root.join(".harness").join("scripts").join("hooks");
            if !dir.is_dir() {
                println!("no gates directory at {}", dir.display());
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
        GateCmd::Check => cmd_gate_check(root),
        GateCmd::Run { gate, task } => {
            let (task_json, task_id, spec_name) = match task {
                Some(tid) => match find_task_json(root, &tid)? {
                    Some((j, s)) => (j, tid, s),
                    None => ("{}".to_string(), tid, String::new()),
                },
                None => ("{}".to_string(), String::new(), String::new()),
            };
            let config = load_harness_config(root)?;
            let inv = HookInvocation {
                hook_name: gate.clone(),
                task_id,
                spec_name,
                iteration: 0,
                attempt: 0,
            };
            let outcome = run_hook(root, &inv, &task_json, config.hooks.default_timeout_secs)?;
            print!("{}", outcome.stdout);
            eprint!("{}", outcome.stderr);
            println!(
                "\n[gate '{}' exit {} in {}ms]",
                gate, outcome.exit_code, outcome.duration_ms
            );
            Ok(outcome.exit_code)
        }
    }
}

// ─── eval ──────────────────────────────────────────────────────────────────

fn cmd_eval(root: &Path, cmd: EvalCmd) -> Result<i32> {
    match cmd {
        EvalCmd::Ls { spec } => {
            let dir = root.join("evals").join(&spec);
            if !dir.is_dir() {
                println!("no evals for '{spec}' (expected {})", dir.display());
                return Ok(0);
            }
            let mut found = false;
            let mut names: Vec<String> = std::fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            for n in names {
                println!("{n}");
                found = true;
            }
            if !found {
                println!("no eval scripts in {}", dir.display());
            }
            Ok(0)
        }
        EvalCmd::Run { spec } => cmd_eval_run(root, &spec),
        EvalCmd::Draft {
            spec,
            force,
            use_code_agent,
        } => cmd_eval_draft(root, &spec, force, use_code_agent),
    }
}

/// Draft eval stubs from a spec's requirement acceptance criteria.
///
/// This mirrors `cmd_spec_draft`: compose a prompt from a template, run an agent,
/// then leave the result for a human to review. The crucial difference from spec
/// drafting is *independence* — the eval is meant to be an oracle that does not
/// trust the code agent's reading of the spec. So by default we drive the draft
/// with the configured `reviewer_command` (a second, independent model) and fall
/// back to the primary agent only when no reviewer is configured or the caller
/// passes `--use-code-agent`. The output is explicitly a draft: stubs are marked
/// with TODOs and must be made real by a human before they can be trusted.
fn cmd_eval_draft(root: &Path, spec: &str, force: bool, use_code_agent: bool) -> Result<i32> {
    // ── 1. Load the spec's requirements (the source of the draft) ─────────────
    let dir = spec_dir(root, spec);
    if !dir.is_dir() {
        anyhow::bail!("no spec '{spec}' at {} — run `harness spec new {spec}` first", dir.display());
    }
    let reqs = load_requirements(&dir)
        .with_context(|| format!(".specs/{spec}/1-requirements.json failed to parse"))?;
    if reqs.requirements.is_empty() {
        anyhow::bail!("spec '{spec}' has no requirements to draft evals from");
    }

    // ── 2. Ensure the eval output directory exists ────────────────────────────
    let evals_dir = root.join("evals").join(spec);
    std::fs::create_dir_all(&evals_dir)
        .with_context(|| format!("failed to create {}", evals_dir.display()))?;

    // Tell the agent which requirements already have an eval so it can respect
    // `--force`. We match the same way the coverage gate does: a requirement is
    // covered if its ID appears in the text of any eval file.
    let eval_texts: Vec<String> = std::fs::read_dir(&evals_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .collect();
    let covered: Vec<&str> = reqs
        .requirements
        .iter()
        .map(|r| r.id.as_str())
        .filter(|id| eval_texts.iter().any(|t| t.contains(*id)))
        .collect();

    // ── 3. Compose the requirement digest the agent will work from ────────────
    let requirements_json = serde_json::to_string_pretty(&reqs.requirements)
        .context("failed to serialize requirements")?;

    let force_note = if force {
        "Overwrite any existing eval script for these requirements.".to_string()
    } else if covered.is_empty() {
        "No requirements have evals yet; write a stub for every requirement.".to_string()
    } else {
        format!(
            "These requirements ALREADY have an eval and must NOT be overwritten \
             (skip them): {}. Write stubs only for the remaining requirements.",
            covered.join(", ")
        )
    };

    // ── 4. Build the prompt from the template ─────────────────────────────────
    let local_template_path = root.join(".harness").join("prompts").join("draft-eval.md");
    let template = if local_template_path.exists() {
        std::fs::read_to_string(&local_template_path).unwrap_or_else(|_| DRAFT_EVAL_PROMPT.to_string())
    } else {
        DRAFT_EVAL_PROMPT.to_string()
    };
    let prompt = template
        .replace("{spec_name}", spec)
        .replace("{requirements}", &requirements_json)
        .replace("{force_note}", &force_note);

    let prompt_path = std::env::temp_dir().join(format!("harness-draft-eval-{spec}.md"));
    std::fs::write(&prompt_path, &prompt)
        .with_context(|| format!("failed to write draft prompt to {}", prompt_path.display()))?;

    // ── 5. Pick the model: reviewer by default, code agent only on request ────
    let config = load_harness_config(root)?;
    let reviewer = config
        .agent
        .reviewer_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (command_template, model_label) = match (use_code_agent, reviewer) {
        (false, Some(rc)) => (rc.to_string(), "reviewer model (independent of code agent)"),
        (false, None) => {
            eprintln!(
                "note: no reviewer_command configured — drafting with the primary code agent.\n\
                 For a stronger, independent oracle, set agent.reviewer_command in .harness/harness.toml."
            );
            (config.agent.command.clone(), "primary code agent")
        }
        (true, _) => (config.agent.command.clone(), "primary code agent (forced)"),
    };
    let cmd_str = command_template.replace("{prompt_file}", &prompt_path.to_string_lossy());
    let working_dir = config.agent.working_dir.as_deref().unwrap_or(".");
    let wd = root.join(working_dir);

    println!("Drafting evals for '{spec}' using the {model_label}…");
    println!("(The agent will write stub scripts to evals/{spec}/)\n");

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

    // ── 6. Make the stubs executable and report ───────────────────────────────
    let mut written = 0usize;
    if let Ok(entries) = std::fs::read_dir(&evals_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_file() {
                make_executable(&p)?;
                written += 1;
            }
        }
    }

    println!("\n✓ drafted evals into evals/{spec}/ ({written} script(s) present)");
    println!("\n⚠ These are DRAFTS, not an oracle. Before trusting them:");
    println!("  - read each stub and confirm it encodes the requirement's INTENT,");
    println!("    not just whatever the code happens to do;");
    println!("  - make sure no stub reads from src/ or peeks at the implementation;");
    println!("  - replace every `# TODO` with a real, behaviour-level assertion.");
    println!("\nThen check coverage and run them:");
    println!("  harness check {spec}        # every requirement must have an eval");
    println!("  harness eval run {spec}");
    Ok(0)
}

/// Run a spec's eval scripts against the current code. The oracle is invoked
/// the same way the rebuild gate runs it (HARNESS_SPEC / HARNESS_ROOT in env).
fn cmd_eval_run(root: &Path, spec: &str) -> Result<i32> {
    let dir = root.join("evals").join(spec);
    if !dir.is_dir() {
        anyhow::bail!("no evals directory at {}", dir.display());
    }
    let mut evals: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    evals.sort();
    if evals.is_empty() {
        println!("no eval scripts in {}", dir.display());
        return Ok(0);
    }
    let mut all_pass = true;
    for ev in &evals {
        let rel = ev.strip_prefix(root).unwrap_or(ev);
        let status = Command::new(ev)
            .current_dir(root)
            .env("HARNESS_SPEC", spec)
            .env("HARNESS_ROOT", root.to_string_lossy().to_string())
            .status();
        match status {
            Ok(s) if s.success() => println!("✓ {}", rel.display()),
            Ok(s) => {
                println!("✗ {} (exit {})", rel.display(), s.code().unwrap_or(-1));
                all_pass = false;
            }
            Err(e) => {
                println!("✗ {} — {e}", rel.display());
                all_pass = false;
            }
        }
    }
    Ok(if all_pass { 0 } else { 2 })
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

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Static preflight for gates: every gate referenced by config/phases/tasks must
/// exist as an executable script, before a run discovers it the hard way.
fn cmd_gate_check(root: &Path) -> Result<i32> {
    use std::collections::BTreeSet;

    let dir = root.join(".harness").join("scripts").join("hooks");
    let config = load_harness_config(root)?;

    // Collect every gate name anything references, with where it came from.
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for g in &config.hooks.default {
        referenced.insert(g.clone());
    }
    for pc in config.phases.values() {
        if let Some(hooks) = &pc.hooks {
            for g in hooks {
                referenced.insert(g.clone());
            }
        }
    }
    for s in list_specs(root)? {
        if let Ok(tasks) = load_tasks(&spec_dir(root, &s)) {
            for t in &tasks {
                for g in &t.hooks {
                    referenced.insert(g.clone());
                }
            }
        }
    }

    if referenced.is_empty() {
        println!("no gates referenced by config, phases, or tasks");
        return Ok(0);
    }

    let mut ok = true;
    for gate in &referenced {
        let path = dir.join(gate);
        if !path.exists() {
            println!("✗ {gate} — missing (expected {})", path.display());
            ok = false;
        } else if !is_executable(&path) {
            println!(
                "✗ {gate} — not executable (run: chmod +x {})",
                path.display()
            );
            ok = false;
        } else {
            println!("✓ {gate}");
        }
    }

    // Informational: scripts present on disk that nothing references.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.path().is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !referenced.contains(&name) {
                    println!("· {name} (present but unreferenced)");
                }
            }
        }
    }

    Ok(if ok { 0 } else { 2 })
}

/// Preview the exact prompt the agent would receive for a task, without running
/// it. The prompt goes to stdout (pipe-friendly); metadata goes to stderr.
fn cmd_explain(
    root: &Path,
    task_id: &str,
    spec_filter: Option<&str>,
    phase_override: Option<&str>,
) -> Result<i32> {
    let config = load_harness_config(root)?;

    let specs = match spec_filter {
        Some(s) => vec![s.to_string()],
        None => list_specs(root)?,
    };
    let mut found: Option<(Task, String)> = None;
    for s in &specs {
        let tasks = load_tasks(&spec_dir(root, s))
            .with_context(|| format!("loading tasks for spec '{s}'"))?;
        if let Some(t) = tasks.into_iter().find(|t| t.id == task_id) {
            found = Some((t, s.clone()));
            break;
        }
    }
    let (task, spec_name) = found.with_context(|| match spec_filter {
        Some(s) => format!("task '{task_id}' not found in spec '{s}'"),
        None => format!("task '{task_id}' not found in any spec"),
    })?;

    // Mirror the loop's phase selection: explicit override, else the next phase
    // that hasn't completed yet.
    let effective_phases: Vec<String> = if task.phases.is_empty() {
        config.loop_config.phase_sequence.clone()
    } else {
        task.phases.clone()
    };
    let current_phase: Option<String> = match phase_override {
        Some(p) => Some(p.to_string()),
        None => effective_phases
            .iter()
            .find(|p| !task.completed_phases.contains(*p))
            .cloned(),
    };
    let phase_cfg = current_phase.as_deref().and_then(|p| config.phases.get(p));
    let phase_template = phase_cfg.and_then(|pc| pc.prompt_template.as_deref());

    let state = load_state(root)?;
    let is_first = state.iteration_count == 0;

    let prompt = compose_prompt(
        root,
        &config,
        &task,
        &spec_name,
        is_first,
        current_phase.as_deref(),
        phase_template,
    )?;

    let agent_cmd = phase_cfg
        .and_then(|pc| pc.agent_command.as_deref())
        .unwrap_or(&config.agent.command);
    eprintln!(
        "# prompt preview · task {} · spec {} · phase {} · {} chars",
        task.id,
        spec_name,
        current_phase.as_deref().unwrap_or("(none)"),
        prompt.chars().count()
    );
    eprintln!("# agent: {}", agent_cmd);
    if task.attempts > 0 {
        eprintln!("# note: task has {} prior attempt(s)", task.attempts);
    }
    println!("{prompt}");
    Ok(0)
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
                println!(
                    "  {} [{:?}] phases: {}",
                    t.id,
                    t.status,
                    phases_display.join(", ")
                );
            }
        }
    }
    println!("\ntasks: todo={todo} in_progress={prog} blocked={blocked} done={done}");
    Ok(0)
}

/// One-line summary of an iteration record, for listing and `--follow` output.
fn iteration_summary_line(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let iter = v.get("iteration").and_then(|i| i.as_u64()).unwrap_or(0);
    let task = v.get("task_id").and_then(|i| i.as_str()).unwrap_or("?");
    let status = v
        .get("task_status_after")
        .and_then(|i| i.as_str())
        .unwrap_or("?");
    let exit = v
        .get("agent_exit_status")
        .and_then(|i| i.as_i64())
        .unwrap_or(0);
    let gates = v
        .get("hook_results")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter()
                .map(|h| {
                    let n = h.get("name").and_then(|x| x.as_str()).unwrap_or("?");
                    let ok = h.get("passed").and_then(|x| x.as_bool()).unwrap_or(false);
                    format!("{}{}", if ok { "✓" } else { "✗" }, n)
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    Some(format!(
        "iter {iter:>3} · {task} · {status} · agent exit {exit}{}{}",
        if gates.is_empty() { "" } else { " · " },
        gates
    ))
}

fn cmd_logs(root: &Path, iteration: Option<u64>, follow: bool) -> Result<i32> {
    let dir = root.join(".harness").join("logs").join("iterations");
    if !dir.is_dir() {
        println!("no iteration logs yet");
        // Still allow --follow to wait for the first record to appear.
        if !follow {
            return Ok(0);
        }
    }

    let read_sorted = |dir: &Path| -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default();
        entries.sort();
        entries
    };

    if follow {
        use std::collections::HashSet;
        let mut seen: HashSet<PathBuf> = HashSet::new();
        // Print everything that already exists, then stream new records.
        for p in read_sorted(&dir) {
            if let Some(line) = iteration_summary_line(&p) {
                println!("{line}");
            }
            seen.insert(p);
        }
        println!("— following {} · Ctrl-C to stop —", dir.display());
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            for p in read_sorted(&dir) {
                if seen.insert(p.clone()) {
                    if let Some(line) = iteration_summary_line(&p) {
                        println!("{line}");
                    }
                }
            }
        }
    }

    let entries = read_sorted(&dir);

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
                match iteration_summary_line(p) {
                    Some(line) => println!("{line}"),
                    None => println!("{}", p.file_name().unwrap().to_string_lossy()),
                }
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

    check!(
        ".harness/ exists",
        root.join(".harness").is_dir(),
        "run `harness init`"
    );

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
            let exists = [
                h.clone(),
                format!("{h}.ps1"),
                format!("{h}.cmd"),
                format!("{h}.bat"),
            ]
            .iter()
            .any(|cand| hooks_dir.join(cand).exists());
            check!(
                format!("hook '{h}' present"),
                exists,
                "create the hook stub"
            );
        }

        if !c.loop_config.phase_sequence.is_empty() {
            println!("phases: {}", c.loop_config.phase_sequence.join(" → "));
            for phase_name in &c.loop_config.phase_sequence {
                if let Some(phase_cfg) = c.phases.get(phase_name) {
                    if let Some(hooks) = &phase_cfg.hooks {
                        for h in hooks {
                            let exists = [
                                h.clone(),
                                format!("{h}.ps1"),
                                format!("{h}.cmd"),
                                format!("{h}.bat"),
                            ]
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
    check!(
        "at least one spec",
        !specs.is_empty(),
        "run `harness spec new <name>`"
    );
    check!(
        "git repository",
        root.join(".git").exists(),
        "run `git init` for rollback safety"
    );

    if !ok {
        println!("\nFor spec↔code consistency, run `harness check --all`.");
    }
    Ok(if ok { 0 } else { 1 })
}

// ─── rebuild (burn & re-render) ──────────────────────────────────────────────

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
    let all_owned = harness_core::manifest::expand_owned_paths(root, owns)?;
    let to_burn: Vec<String> = if let Some(comp_glob) = component {
        let comp_set = harness_core::manifest::build_owns_globset(&[comp_glob.to_string()])?;
        all_owned
            .into_iter()
            .filter(|p| comp_set.is_match(p))
            .collect()
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
        println!(
            "[{attempt_label}] HEAD checkpoint: {}",
            if head_sha.is_empty() {
                "(no commit)"
            } else {
                &head_sha
            }
        );

        // Snapshot untracked files BEFORE burning/regenerating, so rollback only
        // removes files this regen created — never the user's untracked work.
        let untracked_before = harness_core::util::git_list_untracked(root).unwrap_or_default();

        // 2. Delete owned files (they're ashes; the spec is the source).
        println!("[{attempt_label}] Burning {} owned file(s)…", to_burn.len());
        for rel in &to_burn {
            let abs = root.join(rel);
            if abs.exists() {
                std::fs::remove_file(&abs).with_context(|| format!("deleting owned file {rel}"))?;
                println!("  del {rel}");
            }
        }

        // 3. Compose a regeneration prompt and run the agent.
        let config = load_harness_config(root)?;
        let regen_prompt = compose_regen_prompt(root, spec_name, &reqs, &to_burn)?;
        let (prompt_file, _) = harness_core::prompt::write_prompt_file(&regen_prompt)?;

        let cmd_str = config
            .agent
            .command
            .replace("{prompt_file}", &prompt_file.to_string_lossy());
        let working_dir = config.agent.working_dir.as_deref().unwrap_or(".");
        let wd = root.join(working_dir);

        println!("[{attempt_label}] Running agent for regeneration…");
        let status = if cfg!(windows) {
            Command::new("cmd")
                .arg("/C")
                .arg(&cmd_str)
                .current_dir(&wd)
                .status()
        } else {
            Command::new("sh")
                .arg("-c")
                .arg(&cmd_str)
                .current_dir(&wd)
                .status()
        }
        .with_context(|| format!("failed to launch agent: {cmd_str}"))?;
        let _ = std::fs::remove_file(&prompt_file);

        let agent_exit = status.code().unwrap_or(-1);
        if agent_exit != 0 {
            println!("[{attempt_label}] Agent exited {agent_exit} — rolling back.");
            let _ = harness_core::util::git_restore_to_head(root, &untracked_before);
            return Ok((agent_exit, vec![]));
        }

        // 4. Run hooks (including evals from evals/<spec>/).
        let guardrails = harness_core::config::load_guardrails(root)?;
        let hooks_to_run = if config.hooks.default.is_empty() {
            vec![]
        } else {
            config.hooks.default.clone()
        };

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
            let timeout = harness_core::hooks::hook_timeout(
                &guardrails,
                hook_name,
                config.hooks.default_timeout_secs,
            );
            let blocking = harness_core::hooks::is_hook_blocking(&guardrails, hook_name);
            match run_hook(root, &inv, &dummy_task, timeout) {
                Ok(outcome) => {
                    let passed = outcome.exit_code == 0 && !outcome.timed_out;
                    let entry = format!(
                        "  hook {hook_name}: {}",
                        if passed { "PASS" } else { "FAIL" }
                    );
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
                    if harness_core::hooks::is_hook_blocking(&guardrails, hook_name) {
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
            let _ = harness_core::util::git_restore_to_head(root, &untracked_before);
            return Ok((-1, hook_log));
        }

        // 5. Phase 5: cross-model review gate for public_interface specs.
        if reqs.public_interface {
            if let Some(ref reviewer_cmd) = config.agent.reviewer_command {
                if !reviewer_cmd.is_empty() {
                    println!(
                        "[{attempt_label}] Running cross-model reviewer (public_interface spec)…"
                    );
                    let review_prompt = compose_review_prompt(spec_name, &reqs, &to_burn);
                    let (review_file, _) = harness_core::prompt::write_prompt_file(&review_prompt)?;
                    let rcmd =
                        reviewer_cmd.replace("{prompt_file}", &review_file.to_string_lossy());
                    let rstatus = if cfg!(windows) {
                        Command::new("cmd")
                            .arg("/C")
                            .arg(&rcmd)
                            .current_dir(root)
                            .status()
                    } else {
                        Command::new("sh")
                            .arg("-c")
                            .arg(&rcmd)
                            .current_dir(root)
                            .status()
                    }
                    .with_context(|| "failed to launch reviewer")?;
                    let _ = std::fs::remove_file(&review_file);
                    if rstatus.code().unwrap_or(-1) != 0 {
                        println!(
                            "[{attempt_label}] Reviewer rejected regeneration — rolling back."
                        );
                        let _ = harness_core::util::git_restore_to_head(root, &untracked_before);
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
            for l in &log2 {
                println!("{l}");
            }
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

fn git_commit_all(root: &Path, message: &str) -> Result<()> {
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .status();
    let _ = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(root)
        .status();
    Ok(())
}

fn compose_regen_prompt(
    root: &Path,
    spec_name: &str,
    reqs: &harness_core::spec::RequirementsFile,
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
    Ok(format!(
        r#"# Regeneration Task: {spec_name}

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
"#
    ))
}

fn compose_review_prompt(
    spec_name: &str,
    reqs: &harness_core::spec::RequirementsFile,
    regenerated_files: &[String],
) -> String {
    let req_json = serde_json::to_string_pretty(&reqs.requirements).unwrap_or_default();
    let files_list = regenerated_files.join("\n");
    format!(
        r#"# Cross-Model Review: {spec_name}

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
"#
    )
}

// ─── spec sync --against-code (Phase 4) ──────────────────────────────────────

/// Drive an agent to reconstruct the spec from owned code, then diff against
/// the canonical spec to produce a convergence verdict.
fn cmd_sync_against_code(root: &Path, spec_name: &str) -> Result<()> {
    let dir = spec_dir(root, spec_name);
    let reqs = load_requirements(&dir)?;

    if reqs.owns.is_empty() {
        eprintln!(
            "note: spec '{spec_name}' has no 'owns' declaration — nothing to reconstruct from."
        );
        return Ok(());
    }

    let owned_files = harness_core::manifest::expand_owned_paths(root, &reqs.owns)?;
    if owned_files.is_empty() {
        println!("No owned files found on disk for spec '{spec_name}'.");
        return Ok(());
    }

    let out_dir = root.join(".harness").join("roundtrip");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(format!("{spec_name}.reconstructed.md"));

    // Compose the reconstruction prompt.
    let files_list = owned_files.join("\n");
    let prompt = format!(
        r#"# Spec Reconstruction: {spec_name}

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
"#,
        out = out_path.display()
    );

    let config = load_harness_config(root)?;
    let (prompt_file, _) = harness_core::prompt::write_prompt_file(&prompt)?;
    let cmd_str = config
        .agent
        .command
        .replace("{prompt_file}", &prompt_file.to_string_lossy());
    let working_dir = config.agent.working_dir.as_deref().unwrap_or(".");
    let wd = root.join(working_dir);

    println!("Running reconstruction agent for '{spec_name}'…");
    let status = if cfg!(windows) {
        Command::new("cmd")
            .arg("/C")
            .arg(&cmd_str)
            .current_dir(&wd)
            .status()
    } else {
        Command::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .current_dir(&wd)
            .status()
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
    let canonical_req =
        std::fs::read_to_string(dir.join("1-requirements.json")).unwrap_or_default();
    let canonical_design = std::fs::read_to_string(dir.join("2-design.md")).unwrap_or_default();
    let reconstructed = std::fs::read_to_string(&out_path).unwrap_or_default();

    // Simple heuristic convergence check: look for requirement IDs in the reconstruction.
    let req_ids: Vec<&str> = reqs.requirements.iter().map(|r| r.id.as_str()).collect();

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
            println!(
                "    Review {} for undocumented behavior.",
                out_path.display()
            );
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

/// Prompt template used by `eval draft` to drive the agent.
const DRAFT_EVAL_PROMPT: &str = include_str!("../templates/draft-eval.md");

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
# Installed by `harness init` — gates commits on spec↔code consistency.
# Fails if any spec-owned file was hand-edited or the spec changed without rebuild.
harness check --all
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

# Enforce task-level write boundaries when a task declares files_hint.
# Any file the agent writes outside files_hint ∪ writes.allow fails the iteration.
# Only active for tasks that declare files_hint, so it's a no-op for tasks that
# don't — there is no downside to leaving it on, and it's the tool's core safety
# net. (Matches the in-code default; set to false only to deliberately disable.)
enforce_ownership = true

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

#[cfg(test)]
mod ref_id_tests {
    use super::references_id;

    #[test]
    fn whole_token_matches() {
        assert!(references_id("covers R-1 here", "R-1"));
        assert!(references_id("R-1", "R-1"));
        assert!(references_id("(R-1)", "R-1"));
        assert!(references_id("see R-1, R-2", "R-1"));
    }

    #[test]
    fn longer_id_does_not_falsely_match() {
        // The classic bug: R-1 must NOT be considered covered by R-10/R-12.
        assert!(!references_id("only R-10 and R-12 here", "R-1"));
        assert!(!references_id("R-100", "R-1"));
        assert!(!references_id("xR-1", "R-1"));
    }
}
