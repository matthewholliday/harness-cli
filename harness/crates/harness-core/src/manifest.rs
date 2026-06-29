use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::spec::{load_requirements, spec_dir};

// ── Data structures ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpecManifestEntry {
    /// SHA-256 of the concatenated spec input files (1-requirements.json +
    /// 2-design.md + 3-tasks.jsonl). Changes when a human edits the spec.
    pub spec_inputs_hash: String,
    /// Relative-to-root path → SHA-256 of file content, for every file owned
    /// by this spec (expanded from the spec's `owns` globs at record time).
    pub owned_files: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    pub specs: HashMap<String, SpecManifestEntry>,
}

// ── Path helpers ───────────────────────────────────────────────────────────────

pub fn manifest_path(root: &Path) -> PathBuf {
    root.join(".harness").join("manifest.json")
}

// ── Serialization ─────────────────────────────────────────────────────────────

pub fn load_manifest(root: &Path) -> Result<Manifest> {
    let path = manifest_path(root);
    if !path.exists() {
        return Ok(Manifest::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading manifest {}", path.display()))?;
    let m: Manifest = serde_json::from_str(&content)
        .with_context(|| format!("parsing manifest {}", path.display()))?;
    Ok(m)
}

pub fn save_manifest(root: &Path, manifest: &Manifest) -> Result<()> {
    let path = manifest_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(manifest).context("serializing manifest")?;
    crate::util::atomic_write_str(&path, &data)
        .with_context(|| format!("writing manifest {}", path.display()))?;
    Ok(())
}

// ── Hashing helpers ────────────────────────────────────────────────────────────

fn hash_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn hash_file(path: &Path) -> Result<String> {
    let content =
        std::fs::read(path).with_context(|| format!("reading {} for hashing", path.display()))?;
    Ok(hash_bytes(&content))
}

/// SHA-256 over the concatenation of spec input files.
pub fn compute_spec_inputs_hash(spec_dir_path: &Path) -> Result<String> {
    let mut combined = String::new();
    for name in ["1-requirements.json", "2-design.md", "3-tasks.jsonl"] {
        let p = spec_dir_path.join(name);
        if p.exists() {
            combined.push_str(
                &std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?,
            );
        }
    }
    Ok(hash_bytes(combined.as_bytes()))
}

// ── Glob helpers ───────────────────────────────────────────────────────────────

pub fn build_owns_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let g = GlobBuilder::new(pat)
            .build()
            .with_context(|| format!("invalid owns glob '{pat}'"))?;
        builder.add(g);
    }
    builder.build().context("building owns globset")
}

/// Expand ownership globs relative to `root` into a sorted list of relative paths.
pub fn expand_owned_paths(root: &Path, owns: &[String]) -> Result<Vec<String>> {
    if owns.is_empty() {
        return Ok(Vec::new());
    }
    let globset = build_owns_globset(owns)?;
    let mut paths = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            if let Some(s) = rel.to_str() {
                if globset.is_match(s) {
                    paths.push(s.to_string());
                }
            }
        }
    }
    paths.sort();
    Ok(paths)
}

// ── record ─────────────────────────────────────────────────────────────────────

/// Recompute and persist the manifest entry for `spec_name`.
/// Called after a successful run iteration or `harness manifest record`.
pub fn record_spec(root: &Path, spec_name: &str) -> Result<()> {
    let dir = spec_dir(root, spec_name);
    let reqs = load_requirements(&dir)?;

    let spec_inputs_hash = compute_spec_inputs_hash(&dir)?;

    let mut owned_files: HashMap<String, String> = HashMap::new();
    for rel in expand_owned_paths(root, &reqs.owns)? {
        let abs = root.join(&rel);
        let hash = hash_file(&abs)?;
        owned_files.insert(rel, hash);
    }

    let mut manifest = load_manifest(root)?;
    manifest.specs.insert(
        spec_name.to_string(),
        SpecManifestEntry {
            spec_inputs_hash,
            owned_files,
        },
    );
    save_manifest(root, &manifest)?;
    Ok(())
}

// ── check ──────────────────────────────────────────────────────────────────────

#[derive(Debug)]
#[allow(dead_code)]
pub enum DriftKind {
    /// The spec inputs changed but `manifest record` has not been re-run.
    StaleCode { spec_name: String },
    /// An owned file's content changed since the last `manifest record`.
    CodeDrift { path: String },
    /// An owned file recorded in the manifest no longer exists.
    Missing { path: String },
    /// The spec has no manifest entry yet.
    Unrecorded { spec_name: String },
}

#[allow(dead_code)]
pub struct CheckResult {
    pub spec: String,
    pub drifts: Vec<DriftKind>,
}

impl CheckResult {
    pub fn is_clean(&self) -> bool {
        self.drifts.is_empty()
    }
}

/// Check a single spec for ownership drift. Returns all detected drift events.
pub fn check_spec(root: &Path, spec_name: &str) -> Result<CheckResult> {
    let manifest = load_manifest(root)?;
    let Some(entry) = manifest.specs.get(spec_name) else {
        return Ok(CheckResult {
            spec: spec_name.to_string(),
            drifts: vec![DriftKind::Unrecorded {
                spec_name: spec_name.to_string(),
            }],
        });
    };

    let dir = spec_dir(root, spec_name);
    let current_hash = compute_spec_inputs_hash(&dir)?;

    let mut drifts = Vec::new();

    if current_hash != entry.spec_inputs_hash {
        drifts.push(DriftKind::StaleCode {
            spec_name: spec_name.to_string(),
        });
    }

    for (rel_path, recorded_hash) in &entry.owned_files {
        let abs = root.join(rel_path);
        if !abs.exists() {
            drifts.push(DriftKind::Missing {
                path: rel_path.clone(),
            });
        } else {
            let current = hash_file(&abs)?;
            if current != *recorded_hash {
                drifts.push(DriftKind::CodeDrift {
                    path: rel_path.clone(),
                });
            }
        }
    }

    Ok(CheckResult {
        spec: spec_name.to_string(),
        drifts,
    })
}
