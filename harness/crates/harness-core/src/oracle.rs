//! ACLC oracle — the procedure that decides an attempt's success (§4 step 4,
//! §8.2, §8.4).
//!
//! The oracle is a shell command whose exit status is the pass/fail decision.
//! It MAY additionally emit a *partial score* used to rank attempts for
//! `on_exhaustion = keep_best` (§8.2). A command reports a score by printing a
//! line of either form:
//!
//! ```text
//! ACLC_SCORE=0.75      # an explicit fraction in [0, 1]
//! ACLC_SCORE=12/20     # passed/total — normalized to 0.6
//! ```
//!
//! When the command emits no score line, the outcome carries `score = None` and
//! ranking falls back to recency (§8.2).
//!
//! Protection (§8.4) is structural in this harness: the eval suite that backs
//! the default oracle lives under `evals/` and `.specs/`, which the write guards
//! already keep outside the agent's writable surface. `oracle.protected = true`
//! therefore needs no extra enforcement here; `protected = false` only relaxes
//! the §5.3 warning.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// The result of evaluating the oracle for one attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct OracleOutcome {
    /// Whether the attempt passed (oracle exit status 0).
    pub passed: bool,
    /// Optional partial score in `[0, 1]` parsed from the oracle output.
    pub score: Option<f64>,
    /// Combined stdout+stderr, for the failure-signal / learning entry.
    pub output: String,
}

/// Parse an `ACLC_SCORE=` line from oracle output. Accepts a bare fraction
/// (`0.75`) or a `passed/total` ratio (`12/20`). Returns the last such line's
/// value, clamped to `[0, 1]`. `None` if no score line is present.
pub fn parse_score(output: &str) -> Option<f64> {
    let mut found = None;
    for line in output.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("ACLC_SCORE=") else {
            continue;
        };
        let rest = rest.trim();
        let parsed = if let Some((num, den)) = rest.split_once('/') {
            match (num.trim().parse::<f64>(), den.trim().parse::<f64>()) {
                (Ok(n), Ok(d)) if d > 0.0 => Some(n / d),
                _ => None,
            }
        } else {
            rest.parse::<f64>().ok()
        };
        if let Some(v) = parsed {
            found = Some(v.clamp(0.0, 1.0));
        }
    }
    found
}

/// Run the oracle command from `working_dir` (resolved under `root`) and return
/// its outcome. A non-zero exit, or a failure to launch, is a fail.
pub fn evaluate(root: &Path, working_dir: &str, command: &str) -> Result<OracleOutcome> {
    let wd = root.join(working_dir);
    let output = if cfg!(windows) {
        Command::new("cmd")
            .arg("/C")
            .arg(command)
            .current_dir(&wd)
            .output()
    } else {
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&wd)
            .output()
    }
    .with_context(|| format!("failed to launch oracle: {command}"))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }

    Ok(OracleOutcome {
        passed: output.status.success(),
        score: parse_score(&combined),
        output: combined,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_fraction() {
        assert_eq!(parse_score("noise\nACLC_SCORE=0.75\nmore"), Some(0.75));
    }

    #[test]
    fn parses_ratio() {
        assert_eq!(parse_score("ACLC_SCORE=12/20"), Some(0.6));
    }

    #[test]
    fn last_score_wins_and_clamps() {
        assert_eq!(parse_score("ACLC_SCORE=0.2\nACLC_SCORE=1.5"), Some(1.0));
    }

    #[test]
    fn no_score_is_none() {
        assert_eq!(parse_score("all good, 5 passed"), None);
        assert_eq!(parse_score("ACLC_SCORE=oops"), None);
        assert_eq!(parse_score("ACLC_SCORE=5/0"), None);
    }

    #[test]
    fn evaluate_passes_on_exit_zero() {
        let root = std::env::temp_dir();
        let o = evaluate(&root, ".", "printf 'ACLC_SCORE=3/4\\n'; exit 0").unwrap();
        assert!(o.passed);
        assert_eq!(o.score, Some(0.75));
    }

    #[test]
    fn evaluate_fails_on_nonzero() {
        let root = std::env::temp_dir();
        let o = evaluate(&root, ".", "echo boom 1>&2; exit 1").unwrap();
        assert!(!o.passed);
        assert!(o.output.contains("boom"));
    }
}
