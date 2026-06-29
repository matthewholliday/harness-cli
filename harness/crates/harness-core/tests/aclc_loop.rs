//! End-to-end ACLC loop tests: drive the real `loop_runner::run` against a fake
//! agent + oracle in a throwaway git repo, exercising the `until_pass` lifecycle,
//! the protected oracle + partial score, memory, and `on_exhaustion`.

use harness_core::loop_runner::{run, RunOptions};
use std::path::Path;
use std::process::Command;

fn sh(dir: &Path, cmd: &str) {
    let ok = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "command failed: {cmd}");
}

fn write(path: &Path, contents: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// Scaffold a minimal harness project with one spec and one task, plus a fake
/// agent and oracle. `aclc_toml` is spliced into harness.toml. Returns the root.
fn scaffold(name: &str, agent_sh: &str, oracle_cmd: &str, aclc_toml: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "aclc-loop-{}-{}-{}",
        name,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();

    // Fake agent script.
    let agent = root.join("agent.sh");
    write(&agent, agent_sh);

    // harness.toml: agent runs our script; ACLC block from the caller.
    write(
        &root.join(".harness/harness.toml"),
        &format!(
            "[agent]\ncommand = \"sh {} {{prompt_file}}\"\n\n{aclc_toml}\n\
             [aclc.oracle]\ncommand = \"{oracle_cmd}\"\nprotected = true\n",
            agent.display()
        ),
    );
    write(&root.join(".harness/prompts/loop.md"), "Do task {task_id}.\n");

    // One spec with one task that owns work/**.
    write(
        &root.join(".specs/demo/1-requirements.json"),
        r#"{"spec":"demo","version":"1","requirements":[{"id":"REQ-1","acceptance_criteria":["it works"]}],"owns":["work/**"]}"#,
    );
    write(
        &root.join(".specs/demo/3-tasks.jsonl"),
        "{\"id\":\"T-1\",\"spec\":\"demo\",\"title\":\"do it\",\"requirements\":[\"REQ-1\"],\"status\":\"todo\",\"priority\":1,\"created_at\":\"2026-01-01T00:00:00Z\",\"updated_at\":\"2026-01-01T00:00:00Z\"}\n",
    );

    // Mirror real projects: spec/harness runtime state is gitignored, so the
    // harness's own task-state writes don't trip the protected-write guard.
    write(&root.join(".gitignore"), ".specs/\n.harness/\nagent.sh\n");
    write(&root.join("README.md"), "baseline\n");

    // Git repo with a baseline commit (only the tracked, non-ignored files).
    sh(&root, "git init -q");
    sh(&root, "git config user.email t@t.t && git config user.name t");
    sh(&root, "git add -A && git commit -q -m baseline");
    root
}

fn task_status(root: &Path) -> String {
    let raw = std::fs::read_to_string(root.join(".specs/demo/3-tasks.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
    v["status"].as_str().unwrap().to_string()
}

/// `until_pass` / `continue` / `compact` (raw): the agent increments a counter
/// that survives between attempts; the protected oracle passes on the 2nd
/// attempt. The run must converge, mark the task done, and record a learning
/// from the first failure.
#[test]
fn until_pass_loops_then_passes_and_records_memory() {
    let agent = "#!/bin/sh\nmkdir -p work\nn=$(cat work/n 2>/dev/null || echo 0)\nn=$((n+1))\necho $n > work/n\necho \"attempt $n\" > work/out.txt\nexit 0\n";
    // Oracle passes once the counter reaches 2; emits a partial score.
    let oracle = "n=$(cat work/n 2>/dev/null || echo 0); echo ACLC_SCORE=$n/2; [ \\\"$n\\\" -ge 2 ]";
    let aclc = "[aclc]\nloop = \"until_pass\"\nworkspace = \"continue\"\nmemory = \"compact\"\nmemory_cap = 8\nlearning = \"raw\"\nmax_attempts = 5\non_exhaustion = \"keep_best\"\n";

    let root = scaffold("pass", agent, oracle, aclc);
    let code = run(&root, RunOptions { spec_filter: None, once: false, max_iterations: None, dry_run: false }).unwrap();

    assert_eq!(code, 0, "loop should exit clean");
    assert_eq!(task_status(&root), "done");
    let out = std::fs::read_to_string(root.join("work/out.txt")).unwrap();
    assert!(out.contains("attempt 2"), "final workspace = passing attempt: {out}");

    // A learning was recorded after the first failed attempt (raw → compact).
    let learnings = root.join(".harness/logs/learnings/demo/T-1.md");
    assert!(learnings.exists(), "LEARNINGS.md should exist");
    let txt = std::fs::read_to_string(&learnings).unwrap();
    assert!(txt.contains("ACLC_SCORE=1/2"), "raw learning captured the failure signal: {txt}");

    std::fs::remove_dir_all(&root).ok();
}

/// Exhaustion: the oracle never passes; with `max_attempts = 2` the task is
/// parked Blocked and the run returns exit 2.
#[test]
fn exhaustion_blocks_task_and_returns_two() {
    let agent = "#!/bin/sh\nmkdir -p work\necho hi > work/out.txt\nexit 0\n";
    let oracle = "echo ACLC_SCORE=0/1; exit 1"; // always fails
    let aclc = "[aclc]\nloop = \"until_pass\"\nworkspace = \"fresh\"\nmemory = \"off\"\nmax_attempts = 2\non_exhaustion = \"keep_last\"\n";

    let root = scaffold("exhaust", agent, oracle, aclc);
    let code = run(&root, RunOptions { spec_filter: None, once: false, max_iterations: None, dry_run: false }).unwrap();

    assert_eq!(code, 2, "blocked task → exit 2");
    assert_eq!(task_status(&root), "blocked");
    std::fs::remove_dir_all(&root).ok();
}
