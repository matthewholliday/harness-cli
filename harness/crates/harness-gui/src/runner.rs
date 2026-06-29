//! Drives the loop the same way a user does: spawns `harness build` as a child
//! process and streams its stdout/stderr back to the UI over a channel.
//!
//! The GUI never owns run state — the loop's on-disk files remain the single
//! source of truth. This module only launches the process and relays its output.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread;

/// Options mirrored from `harness build`'s flags.
#[derive(Clone, Default)]
pub struct RunOptions {
    /// Target a single spec; empty means all specs.
    pub spec: String,
    /// `--once`: run a single iteration.
    pub once: bool,
    /// `--dry-run`: preview the next task without invoking the agent.
    pub dry_run: bool,
    /// `--max N`: cap iterations. Empty/0 means no cap.
    pub max: String,
}

/// A live `harness build` process and the channel carrying its output lines.
pub struct RunHandle {
    child: Child,
    rx: Receiver<String>,
    finished: bool,
    exit_code: Option<i32>,
}

/// Resolve the harness CLI binary. Honors `HARNESS_BIN`, else falls back to a
/// sibling `harness` next to this executable, else `harness` on `PATH`.
fn harness_bin() -> PathBuf {
    if let Ok(p) = std::env::var("HARNESS_BIN") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(if cfg!(windows) {
                "harness.exe"
            } else {
                "harness"
            });
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("harness")
}

impl RunHandle {
    /// Spawn `harness build` for `root` with the given options. A reader thread
    /// merges stdout and stderr into the returned handle's channel.
    pub fn spawn(root: &Path, opts: &RunOptions) -> anyhow::Result<RunHandle> {
        let mut cmd = Command::new(harness_bin());
        cmd.arg("build");
        if !opts.spec.trim().is_empty() {
            cmd.arg(opts.spec.trim());
        } else {
            cmd.arg("--all");
        }
        if opts.once {
            cmd.arg("--once");
        }
        if opts.dry_run {
            cmd.arg("--dry-run");
        }
        if let Ok(n) = opts.max.trim().parse::<u32>() {
            if n > 0 {
                cmd.arg("--max").arg(n.to_string());
            }
        }
        cmd.current_dir(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let (tx, rx) = channel();

        // One reader thread per stream, both feeding the same channel.
        if let Some(out) = child.stdout.take() {
            let tx = tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(err) = child.stderr.take() {
            let tx = tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }

        Ok(RunHandle {
            child,
            rx,
            finished: false,
            exit_code: None,
        })
    }

    /// Drain any pending output lines and detect process exit. Returns the new
    /// lines so the caller can append them to its log buffer.
    pub fn poll(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(l) => lines.push(l),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        // The reader threads close before the child is reaped; check the child.
        if !self.finished {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.finished = true;
                self.exit_code = status.code();
            }
        }
        lines
    }

    pub fn is_running(&self) -> bool {
        !self.finished
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Request termination of the child process.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.finished = true;
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
