use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Write `contents` to `path` atomically: write to a sibling temp file in the
/// same directory, fsync it, then `rename()` over the target. On POSIX the
/// rename is atomic, so a reader (or a crash / Ctrl-C) ever sees either the old
/// file or the complete new one — never a truncated half-write.
///
/// This matters because the harness rewrites `3-tasks.jsonl`, `state.json` and
/// the manifest after *every* iteration; a plain truncate-then-write leaves a
/// constant window where SIGINT corrupts the file and loses the task list.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    std::fs::create_dir_all(&parent).with_context(|| format!("creating directory {:?}", parent))?;

    // Temp file in the same directory so rename() stays on one filesystem.
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());
    let tmp = parent.join(format!(".{}.tmp.{}", file_name, std::process::id()));

    {
        use std::io::Write;
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating temp file {:?}", tmp))?;
        f.write_all(contents)
            .with_context(|| format!("writing temp file {:?}", tmp))?;
        f.sync_all()
            .with_context(|| format!("fsync temp file {:?}", tmp))?;
    }

    std::fs::rename(&tmp, path)
        .with_context(|| format!("atomically renaming {:?} -> {:?}", tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    Ok(())
}

/// Convenience wrapper for string payloads.
pub fn atomic_write_str(path: &Path, contents: &str) -> Result<()> {
    atomic_write(path, contents.as_bytes())
}

/// List untracked (and not-ignored) files relative to `root`. Snapshot this
/// before running an agent so a subsequent reset can delete only files the
/// agent created — never the user's pre-existing untracked work.
pub fn git_list_untracked(root: &Path) -> Result<HashSet<String>> {
    let out = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(root)
        .output()
        .context("git ls-files (snapshot) failed to run")?;
    if !out.status.success() {
        anyhow::bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Restore the working tree to HEAD after a failed agent attempt, without the
/// data-loss footgun of a blanket `git clean -fd`. Tracked files are restored
/// via `git checkout`; untracked files are removed ONLY if they are new since
/// `untracked_before` (i.e. created by the agent). `.harness/logs` is always
/// preserved. No-op (with `Ok`) if the repo has no commit to roll back to.
pub fn git_restore_to_head(root: &Path, untracked_before: &HashSet<String>) -> Result<()> {
    let has_head = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_head {
        anyhow::bail!("no commit to reset to (empty repository)");
    }

    let checkout = Command::new("git")
        .args(["checkout", "HEAD", "--", ".", ":(exclude).harness/logs"])
        .current_dir(root)
        .output()
        .context("git checkout (reset) failed to run")?;
    if !checkout.status.success() {
        anyhow::bail!(
            "git checkout failed: {}",
            String::from_utf8_lossy(&checkout.stderr).trim()
        );
    }

    let untracked_now = git_list_untracked(root).unwrap_or_default();
    for path in untracked_now.difference(untracked_before) {
        if path.starts_with(".harness/logs") {
            continue;
        }
        let abs = root.join(path);
        if let Err(e) = std::fs::remove_file(&abs) {
            eprintln!("warning: could not remove agent-created file {path}: {e}");
        }
    }
    Ok(())
}
