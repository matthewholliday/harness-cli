use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::{load_guardrails, load_harness_config};
use crate::hooks::{
    hook_timeout, is_hook_blocking, run_hook, save_hook_log, truncate_output, HookInvocation,
};
use crate::prompt::{compose_prompt, write_prompt_file};
use crate::spec::{list_specs, load_tasks, save_tasks, spec_dir, Task, TaskStatus};
use crate::state::{
    append_progress, load_state, save_iteration_record, save_state, HookResult, IterationRecord,
};

pub struct RunOptions {
    pub spec_filter: Option<String>,
    pub once: bool,
    pub max_iterations: Option<u64>,
    pub dry_run: bool,
}

/// An entry in the global work pool: which spec, the task index within that
/// spec's task vector, and the task itself.
struct PoolEntry {
    spec: String,
    idx: usize,
}

/// Run the Ralph loop. Returns a process exit code (0 success, 2 blocked, 3 agent failure).
pub fn run(root: &Path, opts: RunOptions) -> Result<i32> {
    let config = load_harness_config(root)?;
    let guardrails = load_guardrails(root)?;
    let mut state = load_state(root)?;

    // Determine scope.
    let scope: Vec<String> = match &opts.spec_filter {
        Some(name) => vec![name.clone()],
        None => list_specs(root)?,
    };
    if scope.is_empty() {
        println!("No specs found under .specs/. Nothing to do.");
        return Ok(0);
    }

    // Load all tasks per spec into memory; tasks_by_spec[spec] = Vec<Task>.
    let mut tasks_by_spec: Vec<(String, Vec<Task>)> = Vec::new();
    for name in &scope {
        let dir = spec_dir(root, name);
        let tasks = load_tasks(&dir)
            .with_context(|| format!("failed to load tasks for spec '{name}'"))?;
        tasks_by_spec.push((name.clone(), tasks));
    }

    // Effective iteration budget: min of config, guardrails, and CLI override.
    let mut budget = config.loop_config.max_iterations as u64;
    budget = budget.min(guardrails.budgets.max_iterations as u64);
    if let Some(m) = opts.max_iterations {
        budget = budget.min(m);
    }

    if state.run_start.is_none() {
        state.run_start = Some(Utc::now());
    }

    let mut iter: u64 = 0;
    let mut agent_failure = false;

    while iter < budget {
        // Build the set of done task ids across all specs (for dependency checks).
        let done_ids: HashSet<String> = tasks_by_spec
            .iter()
            .flat_map(|(_, ts)| ts.iter())
            .filter(|t| t.status == TaskStatus::Done)
            .map(|t| t.id.clone())
            .collect();

        // Find the next eligible task across all specs: lowest priority Todo
        // whose depends_on are all done. Ties broken by file order (first found).
        let mut best: Option<PoolEntry> = None;
        let mut best_priority: i64 = i64::MAX;
        for (si, (_spec, ts)) in tasks_by_spec.iter().enumerate() {
            for (ti, t) in ts.iter().enumerate() {
                if t.status != TaskStatus::Todo {
                    continue;
                }
                let deps_ok = t.depends_on.iter().all(|d| done_ids.contains(d));
                if !deps_ok {
                    continue;
                }
                if t.priority < best_priority {
                    best_priority = t.priority;
                    best = Some(PoolEntry {
                        spec: scope[si].clone(),
                        idx: ti,
                    });
                }
            }
        }

        let Some(entry) = best else {
            // No eligible Todo tasks. Are any still blocked?
            let blocked = tasks_by_spec
                .iter()
                .flat_map(|(_, ts)| ts.iter())
                .filter(|t| t.status == TaskStatus::Blocked)
                .count();
            let (done, total) = count_done_total(&tasks_by_spec);
            print_summary(done, blocked, total - done - blocked);
            persist_all(root, &tasks_by_spec, &mut state)?;
            return Ok(if blocked > 0 { 2 } else { 0 });
        };

        // Locate the spec vec index for this entry.
        let spec_vec_idx = scope.iter().position(|s| *s == entry.spec).unwrap();
        let spec_name = entry.spec.clone();

        // Mark in_progress and persist.
        {
            let task = &mut tasks_by_spec[spec_vec_idx].1[entry.idx];
            task.status = TaskStatus::InProgress;
            task.updated_at = Utc::now();
        }
        persist_spec(root, &scope[spec_vec_idx], &tasks_by_spec[spec_vec_idx].1)?;

        // Snapshot the task for prompt/hook use.
        let task = tasks_by_spec[spec_vec_idx].1[entry.idx].clone();

        if opts.dry_run {
            println!(
                "[dry-run] iter {iter}: would run task {} ({}) in spec {}",
                task.id, task.title, spec_name
            );
            // Revert to Todo so dry-run doesn't mutate effective state.
            tasks_by_spec[spec_vec_idx].1[entry.idx].status = TaskStatus::Todo;
            persist_spec(root, &scope[spec_vec_idx], &tasks_by_spec[spec_vec_idx].1)?;
            iter += 1;
            if opts.once {
                break;
            }
            continue;
        }

        // Compose + write prompt.
        let is_first = state.iteration_count == 0;
        let prompt = compose_prompt(root, &config, &task, &spec_name, is_first)?;
        let (prompt_file, prompt_hash) = write_prompt_file(&prompt)?;

        // Build and run the agent command (fresh process).
        let cmd_str = config
            .agent
            .command
            .replace("{prompt_file}", &prompt_file.to_string_lossy());
        let working_dir = config
            .agent
            .working_dir
            .clone()
            .unwrap_or_else(|| ".".to_string());
        let agent_status = run_agent(root, &working_dir, &cmd_str);

        let agent_exit = match &agent_status {
            Ok(code) => *code,
            Err(e) => {
                eprintln!("agent invocation error: {e:#}");
                -1
            }
        };

        // If the agent failed to run or returned non-zero, treat as a failed attempt.
        let mut hook_results: Vec<HookResult> = Vec::new();
        let mut all_blocking_passed = true;

        if agent_exit != 0 {
            all_blocking_passed = false;
            agent_failure = true;
        } else {
            // Verify phase: run the task's hooks (or config default) in order.
            let hook_list = if task.hooks.is_empty() {
                config.hooks.default.clone()
            } else {
                task.hooks.clone()
            };
            let task_json = serde_json::to_string(&task).unwrap_or_else(|_| "{}".to_string());

            for hook_name in &hook_list {
                let inv = HookInvocation {
                    hook_name: hook_name.clone(),
                    task_id: task.id.clone(),
                    spec_name: spec_name.clone(),
                    iteration: iter,
                    attempt: task.attempts as u64,
                };
                let timeout =
                    hook_timeout(&guardrails, hook_name, config.hooks.default_timeout_secs);
                let blocking = is_hook_blocking(&guardrails, hook_name);

                match run_hook(root, &inv, &task_json, timeout) {
                    Ok(outcome) => {
                        let log_path = save_hook_log(root, hook_name, &inv, &outcome)
                            .unwrap_or_else(|_| String::new());
                        let passed = outcome.exit_code == 0 && !outcome.timed_out;
                        let combined =
                            format!("{}\n{}", outcome.stdout, outcome.stderr);
                        hook_results.push(HookResult {
                            name: hook_name.clone(),
                            exit_code: outcome.exit_code,
                            duration_ms: outcome.duration_ms,
                            blocking,
                            passed,
                            truncated_output: truncate_output(&combined, 20, 20),
                            full_log_path: log_path,
                        });
                        if blocking && !passed {
                            all_blocking_passed = false;
                            // Short-circuit remaining hooks.
                            break;
                        }
                    }
                    Err(e) => {
                        hook_results.push(HookResult {
                            name: hook_name.clone(),
                            exit_code: -1,
                            duration_ms: 0,
                            blocking,
                            passed: false,
                            truncated_output: format!("hook error: {e:#}"),
                            full_log_path: String::new(),
                        });
                        if blocking {
                            all_blocking_passed = false;
                            break;
                        }
                    }
                }
            }
        }

        // Clean up the temp prompt file.
        let _ = std::fs::remove_file(&prompt_file);

        // Decide task outcome.
        let mut git_sha: Option<String> = None;
        let final_status: TaskStatus;

        if all_blocking_passed && agent_exit == 0 {
            final_status = TaskStatus::Done;
            if config.loop_config.commit_each_success {
                let msg = config
                    .loop_config
                    .commit_message_template
                    .replace("{task_id}", &task.id)
                    .replace("{task_title}", &task.title);
                match git_commit(root, &msg) {
                    Ok(sha) => git_sha = sha,
                    Err(e) => eprintln!("warning: git commit failed: {e:#}"),
                }
            }
            append_progress(
                root,
                &format!("- [{}] iter {iter}: task {} DONE — {}", now(), task.id, task.title),
            )?;
        } else {
            // Failed attempt.
            let task_mut = &mut tasks_by_spec[spec_vec_idx].1[entry.idx];
            task_mut.attempts += 1;
            let summary = if agent_exit != 0 {
                format!("agent exited {agent_exit}")
            } else {
                hook_results
                    .iter()
                    .filter(|h| h.blocking && !h.passed)
                    .map(|h| format!("{} (exit {})", h.name, h.exit_code))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let note = format!("[iter {iter}] failed: {summary}");
            task_mut.notes = Some(match &task_mut.notes {
                Some(existing) if !existing.is_empty() => format!("{existing}\n{note}"),
                _ => note.clone(),
            });
            if task_mut.attempts >= task_mut.max_attempts {
                task_mut.status = TaskStatus::Blocked;
                final_status = TaskStatus::Blocked;
            } else {
                task_mut.status = TaskStatus::Todo;
                final_status = TaskStatus::Todo;
            }
            task_mut.updated_at = Utc::now();
            append_progress(
                root,
                &format!(
                    "- [{}] iter {iter}: task {} {} — {}",
                    now(),
                    task.id,
                    if final_status == TaskStatus::Blocked { "BLOCKED" } else { "retry" },
                    summary
                ),
            )?;
        }

        // If we passed, set the task status to Done now.
        if final_status == TaskStatus::Done {
            let task_mut = &mut tasks_by_spec[spec_vec_idx].1[entry.idx];
            task_mut.status = TaskStatus::Done;
            task_mut.updated_at = Utc::now();
        }

        // Write iteration record.
        let record = IterationRecord {
            iteration: iter,
            task_id: task.id.clone(),
            spec_name: spec_name.clone(),
            prompt_hash,
            agent_exit_status: agent_exit,
            hook_results,
            git_commit_sha: git_sha,
            task_status_after: format!("{:?}", final_status).to_lowercase(),
            timestamp: Utc::now(),
        };
        save_iteration_record(root, &record)?;

        // Persist task + state.
        persist_spec(root, &scope[spec_vec_idx], &tasks_by_spec[spec_vec_idx].1)?;
        state.iteration_count += 1;
        state.active_spec = Some(spec_name.clone());
        state.last_task_id = Some(task.id.clone());
        state.last_task_status = Some(record.task_status_after.clone());
        save_state(root, &state)?;

        iter += 1;
        if opts.once {
            break;
        }
    }

    let (done, total) = count_done_total(&tasks_by_spec);
    let blocked = tasks_by_spec
        .iter()
        .flat_map(|(_, ts)| ts.iter())
        .filter(|t| t.status == TaskStatus::Blocked)
        .count();
    print_summary(done, blocked, total - done - blocked);

    if agent_failure && opts.once {
        return Ok(3);
    }
    if blocked > 0 {
        return Ok(2);
    }
    Ok(0)
}

fn now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn count_done_total(tasks_by_spec: &[(String, Vec<Task>)]) -> (usize, usize) {
    let total: usize = tasks_by_spec.iter().map(|(_, t)| t.len()).sum();
    let done: usize = tasks_by_spec
        .iter()
        .flat_map(|(_, t)| t.iter())
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    (done, total)
}

fn persist_spec(root: &Path, spec_name: &str, tasks: &[Task]) -> Result<()> {
    save_tasks(&spec_dir(root, spec_name), tasks)
}

fn persist_all(
    root: &Path,
    tasks_by_spec: &[(String, Vec<Task>)],
    state: &mut crate::state::LoopState,
) -> Result<()> {
    for (name, tasks) in tasks_by_spec {
        persist_spec(root, name, tasks)?;
    }
    save_state(root, state)
}

fn run_agent(root: &Path, working_dir: &str, cmd_str: &str) -> Result<i32> {
    let wd = root.join(working_dir);
    let status = if cfg!(windows) {
        Command::new("cmd")
            .arg("/C")
            .arg(cmd_str)
            .current_dir(&wd)
            .status()
    } else {
        Command::new("sh")
            .arg("-c")
            .arg(cmd_str)
            .current_dir(&wd)
            .status()
    }
    .with_context(|| format!("failed to launch agent: {cmd_str}"))?;
    Ok(status.code().unwrap_or(-1))
}

/// Commit all changes; returns the new commit sha (None if nothing to commit).
fn git_commit(root: &Path, message: &str) -> Result<Option<String>> {
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .status()
        .context("git add failed to run")?;
    if !add.success() {
        anyhow::bail!("git add returned non-zero");
    }
    let commit = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(root)
        .output()
        .context("git commit failed to run")?;
    if !commit.status.success() {
        // Likely nothing to commit; not fatal.
        return Ok(None);
    }
    let rev = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("git rev-parse failed")?;
    let sha = String::from_utf8_lossy(&rev.stdout).trim().to_string();
    Ok(Some(sha))
}

fn print_summary(done: usize, blocked: usize, remaining: usize) {
    println!("\n=== harness summary ===");
    println!("  done:      {done}");
    println!("  blocked:   {blocked}");
    println!("  remaining: {remaining}");
}
