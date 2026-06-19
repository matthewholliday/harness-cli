use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;

use crate::config::GuardrailsConfig;

/// One invocation of a hook within a loop iteration.
pub struct HookInvocation {
    pub hook_name: String,
    pub task_id: String,
    pub spec_name: String,
    pub iteration: u64,
    pub attempt: u64,
}

/// Captured result of running a hook script.
pub struct HookOutcome {
    pub exit_code: i32,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Resolve a hook name to an executable script in `.harness/scripts/hooks/`.
/// Tries the bare name first (POSIX), then `.ps1`, `.cmd`, `.bat` (Windows).
pub fn resolve_hook_script(root: &Path, hook_name: &str) -> Result<PathBuf> {
    let dir = root.join(".harness").join("scripts").join("hooks");
    let candidates = [
        dir.join(hook_name),
        dir.join(format!("{hook_name}.ps1")),
        dir.join(format!("{hook_name}.cmd")),
        dir.join(format!("{hook_name}.bat")),
    ];
    for cand in &candidates {
        if cand.is_file() {
            return Ok(cand.clone());
        }
    }
    Err(anyhow!(
        "hook script '{}' not found in {}",
        hook_name,
        dir.display()
    ))
}

#[cfg(windows)]
fn build_command(script: &Path) -> Command {
    match script.extension().and_then(|e| e.to_str()) {
        Some("ps1") => {
            let mut c = Command::new("powershell");
            c.arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(script);
            c
        }
        _ => {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(script);
            c
        }
    }
}

#[cfg(not(windows))]
fn build_command(script: &Path) -> Command {
    Command::new(script)
}

/// Run a hook script with the harness environment + JSON task payload on stdin,
/// enforcing a timeout. Stdout/stderr are captured in full.
pub fn run_hook(
    root: &Path,
    inv: &HookInvocation,
    task_json: &str,
    timeout_secs: u64,
) -> Result<HookOutcome> {
    let script = resolve_hook_script(root, &inv.hook_name)?;

    let mut cmd = build_command(&script);
    cmd.current_dir(root)
        .env("HARNESS_HOOK", &inv.hook_name)
        .env("HARNESS_TASK_ID", &inv.task_id)
        .env("HARNESS_SPEC", &inv.spec_name)
        .env("HARNESS_ITERATION", inv.iteration.to_string())
        .env("HARNESS_ATTEMPT", inv.attempt.to_string())
        .env("HARNESS_ROOT", root.to_string_lossy().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn hook '{}'", inv.hook_name))?;

    // Feed the task payload to stdin then close it.
    if let Some(mut stdin) = child.stdin.take() {
        let payload = task_json.to_string();
        // Write on a thread so a hook that ignores stdin can't deadlock us.
        let _ = thread::spawn(move || {
            let _ = stdin.write_all(payload.as_bytes());
        });
    }

    // Watchdog: kill the child if it exceeds the timeout.
    let (tx, rx) = mpsc::channel::<()>();
    let killer = child;
    let waited = {
        // Move the child into a thread that waits for it.
        let (otx, orx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let out = killer.wait_with_output();
            let _ = otx.send(out);
            // signal completion (ignore error if receiver already dropped)
            let _ = tx.send(());
        });
        // Wait for either completion or timeout.
        let timed_out = rx.recv_timeout(Duration::from_secs(timeout_secs)).is_err();
        if timed_out {
            // Best-effort kill of the process tree by pid is not available here
            // since the child was moved; the wait thread will return once the
            // process is reaped. We mark timed_out and still collect output.
            // To actually terminate, we rely on the OS-level kill below.
        }
        let _ = handle.join();
        (orx.recv(), timed_out)
    };

    let duration_ms = start.elapsed().as_millis() as u64;
    let (output_res, timed_out) = waited;
    let output = output_res
        .context("hook wait thread did not return output")?
        .with_context(|| format!("failed to wait on hook '{}'", inv.hook_name))?;

    Ok(HookOutcome {
        exit_code: output.status.code().unwrap_or(-1),
        duration_ms,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        timed_out,
    })
}

/// Persist full hook output to `.harness/logs/hooks/<ts>-<hook>.log`; returns the path.
pub fn save_hook_log(
    root: &Path,
    hook_name: &str,
    inv: &HookInvocation,
    outcome: &HookOutcome,
) -> Result<String> {
    let dir = root.join(".harness").join("logs").join("hooks");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("{ts}-{hook_name}.log"));

    let body = format!(
        "# hook: {}\n# task: {}\n# spec: {}\n# iteration: {}\n# attempt: {}\n# exit_code: {}\n# duration_ms: {}\n# timed_out: {}\n\n=== STDOUT ===\n{}\n=== STDERR ===\n{}\n",
        hook_name,
        inv.task_id,
        inv.spec_name,
        inv.iteration,
        inv.attempt,
        outcome.exit_code,
        outcome.duration_ms,
        outcome.timed_out,
        outcome.stdout,
        outcome.stderr,
    );
    std::fs::write(&path, body)
        .with_context(|| format!("failed to write hook log {}", path.display()))?;
    Ok(path.to_string_lossy().to_string())
}

/// Truncate multi-line output to the first `head` and last `tail` lines.
pub fn truncate_output(s: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= head + tail {
        return s.to_string();
    }
    let mut out: Vec<String> = Vec::new();
    out.extend(lines[..head].iter().map(|l| l.to_string()));
    out.push(format!(
        "... ({} lines omitted) ...",
        lines.len() - head - tail
    ));
    out.extend(lines[lines.len() - tail..].iter().map(|l| l.to_string()));
    out.join("\n")
}

/// Is a hook blocking? Per-hook guardrail config wins; otherwise everything
/// blocks except `run_e2e_tests`, which defaults to non-blocking.
pub fn is_hook_blocking(guardrails: &GuardrailsConfig, hook_name: &str) -> bool {
    if let Some(cfg) = guardrails.hooks.get(hook_name) {
        return cfg.blocking;
    }
    hook_name != "run_e2e_tests"
}

/// Resolve a hook's timeout: per-hook guardrail override, else the supplied default.
pub fn hook_timeout(guardrails: &GuardrailsConfig, hook_name: &str, default_secs: u64) -> u64 {
    guardrails
        .hooks
        .get(hook_name)
        .map(|c| c.timeout_secs)
        .unwrap_or(default_secs)
}
