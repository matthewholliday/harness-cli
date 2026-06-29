//! ACLC memory — the `LEARNINGS.md` artifact carried between attempts (§7.2–7.4).
//!
//! Memory is a single text artifact per task. It is **read before** an attempt
//! (injected into the prompt as readable context) and **written after** a failed
//! attempt (§4). The four modes (`off`/`replace`/`append`/`compact`) and the two
//! learning kinds (`raw`/`reflection`) are defined in [`crate::aclc`]; this
//! module owns the on-disk artifact and the update semantics.

use crate::aclc::Memory;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Entries are separated by this sentinel. It is an HTML comment so the file
/// still renders cleanly as Markdown, and it will not occur in agent prose.
const DELIM: &str = "\n<!--aclc-entry-->\n";

/// Path to a task's learnings artifact. It lives under `.harness/logs/` so it is
/// agent-*readable* (the harness injects it into the prompt) while every write
/// is owned by the harness — the agent never edits it.
pub fn learnings_path(root: &Path, spec: &str, task_id: &str) -> PathBuf {
    root.join(".harness")
        .join("logs")
        .join("learnings")
        .join(spec)
        .join(format!("{task_id}.md"))
}

/// The current learning entries for a task, oldest first. Empty when no artifact
/// exists yet.
pub fn load_entries(root: &Path, spec: &str, task_id: &str) -> Vec<String> {
    let path = learnings_path(root, spec, task_id);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_entries(&raw)
}

fn parse_entries(raw: &str) -> Vec<String> {
    raw.split(DELIM)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.starts_with("<!-- aclc:learnings"))
        .map(|s| s.to_string())
        .collect()
}

fn serialize_entries(entries: &[String]) -> String {
    // The header is its own DELIM-separated chunk so it never glues onto the
    // first entry; `parse_entries` then filters it out cleanly.
    let mut out = String::from("<!-- aclc:learnings v1 — managed by the harness; do not edit -->");
    for e in entries {
        out.push_str(DELIM);
        out.push_str(e.trim());
    }
    out.push('\n');
    out
}

/// Render the current learnings for injection into a prompt. Returns an empty
/// string when there is nothing to inject, so the caller can omit the section.
pub fn render_for_prompt(entries: &[String]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for (i, e) in entries.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", i + 1, e.trim().replace('\n', "\n   ")));
    }
    s.trim_end().to_string()
}

/// Build a `raw` learning entry: the failure signal verbatim, truncated to a
/// sane size so a noisy gate log cannot dominate later prompts (§7.3).
pub fn raw_entry(failure_signal: &str) -> String {
    let trimmed = failure_signal.trim();
    const CAP: usize = 1200;
    if trimmed.chars().count() <= CAP {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(CAP).collect();
        format!("{head}\n[... truncated ...]")
    }
}

/// Normalize an entry for dedup: lowercase, collapse whitespace.
fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Apply a memory update after a failed attempt produced learning entry `e`
/// (§7.2). For `compact`, entries are deduplicated by normalized text and the
/// newest `cap` are retained — deterministic in count (≤ cap) per §7.4. (A
/// richer agent reconciliation pass is an allowed, application-defined upgrade.)
///
/// `off` removes any existing artifact and discards `e`.
pub fn update(
    root: &Path,
    spec: &str,
    task_id: &str,
    mode: Memory,
    cap: u32,
    entry: &str,
) -> Result<()> {
    let path = learnings_path(root, spec, task_id);

    if mode == Memory::Off {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }

    let entry = entry.trim();
    if entry.is_empty() {
        return Ok(());
    }

    let next: Vec<String> = match mode {
        Memory::Off => unreachable!(),
        Memory::Replace => vec![entry.to_string()],
        Memory::Append => {
            let mut v = load_entries(root, spec, task_id);
            v.push(entry.to_string());
            v
        }
        Memory::Compact => {
            let mut v = load_entries(root, spec, task_id);
            v.push(entry.to_string());
            compact(v, cap as usize)
        }
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    crate::util::atomic_write_str(&path, &serialize_entries(&next))
}

/// Reduce `entries` to at most `cap`, dropping exact (normalized) duplicates —
/// keeping the most recent occurrence — then keeping the newest `cap` (§7.4).
fn compact(entries: Vec<String>, cap: usize) -> Vec<String> {
    // Dedup keeping the latest occurrence: walk from the end, skip seen.
    let mut seen = std::collections::HashSet::new();
    let mut rev_unique: Vec<String> = Vec::new();
    for e in entries.into_iter().rev() {
        if seen.insert(norm(&e)) {
            rev_unique.push(e);
        }
    }
    rev_unique.reverse();
    let cap = cap.max(1);
    if rev_unique.len() > cap {
        rev_unique.split_off(rev_unique.len() - cap)
    } else {
        rev_unique
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "aclc-mem-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn off_discards_and_removes() {
        let root = tmp();
        update(&root, "s", "T-1", Memory::Append, 8, "first").unwrap();
        assert_eq!(load_entries(&root, "s", "T-1").len(), 1);
        update(&root, "s", "T-1", Memory::Off, 8, "ignored").unwrap();
        assert!(load_entries(&root, "s", "T-1").is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn replace_overwrites() {
        let root = tmp();
        update(&root, "s", "T-2", Memory::Replace, 8, "one").unwrap();
        update(&root, "s", "T-2", Memory::Replace, 8, "two").unwrap();
        assert_eq!(load_entries(&root, "s", "T-2"), vec!["two".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn append_accumulates_and_round_trips() {
        let root = tmp();
        update(&root, "s", "T-3", Memory::Append, 8, "alpha line").unwrap();
        update(&root, "s", "T-3", Memory::Append, 8, "beta\nmultiline").unwrap();
        let e = load_entries(&root, "s", "T-3");
        assert_eq!(
            e,
            vec!["alpha line".to_string(), "beta\nmultiline".to_string()]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn compact_caps_to_newest() {
        let root = tmp();
        for i in 0..10 {
            update(
                &root,
                "s",
                "T-4",
                Memory::Compact,
                3,
                &format!("lesson {i}"),
            )
            .unwrap();
        }
        let e = load_entries(&root, "s", "T-4");
        assert_eq!(e.len(), 3);
        assert_eq!(e, vec!["lesson 7", "lesson 8", "lesson 9"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn compact_dedups_keeping_latest() {
        let out = compact(
            vec![
                "Use the public API".into(),
                "use   the PUBLIC api".into(), // dup of #1 (normalized)
                "Add a guard clause".into(),
            ],
            8,
        );
        // The earlier duplicate is dropped; the later one is kept in its place.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "use   the PUBLIC api");
        assert_eq!(out[1], "Add a guard clause");
    }

    #[test]
    fn render_numbers_entries() {
        let r = render_for_prompt(&["first".into(), "second".into()]);
        assert_eq!(r, "1. first\n2. second");
        assert_eq!(render_for_prompt(&[]), "");
    }

    #[test]
    fn raw_entry_truncates() {
        let big = "x".repeat(5000);
        let e = raw_entry(&big);
        assert!(e.contains("[... truncated ...]"));
        assert!(e.chars().count() < 5000);
    }
}
