# Spec-as-Source: A Developer's Guide

*A narrative introduction through the story of building a task management app.*

---

## The problem with the usual way

Maya has been here before.

She opened a six-month-old codebase this morning, trying to add a feature her PM asked for. The PR description is gone. The Notion doc that explains the design decisions is stale. The `README` describes an architecture that was scrapped in February. The code itself is real — it runs in production — but *why* it does what it does is stored nowhere except in the heads of the two engineers who originally wrote it, one of whom has since left.

She spent three hours reading source before she felt safe enough to touch anything.

This is the normal state of software. Code survives. Context doesn't.

Spec-as-source is a discipline — and a tool — built around the opposite bet: that if you make the *specification* the only thing humans are allowed to edit, and treat code as a rendering of that specification, you get a codebase that is perpetually explainable and regenerable from first principles. When context lives in the spec instead of evaporating into git history, you don't need to reverse-engineer your own software.

Let's watch it work.

---

## Setting the scene

Maya is starting fresh. She has a new project: a command-line task management app called **Tido**. It needs to store tasks, let users add and complete them, and output a clean daily report. Simple enough to fit in an afternoon, complex enough to be real.

She's going to build it with `harness`, a spec-as-source agent loop. The harness drives a coding agent (Claude) to implement code task-by-task, but it does something the usual agent workflow doesn't: it treats the spec as the source of truth and the code as a rendering. The agent never edits the spec. Humans never hand-edit the generated code. And if the spec changes, the code gets regenerated — not patched.

```sh
mkdir tido && cd tido
git init
harness init
```

This scaffolds `.harness/` (config, guardrails, prompts) and `evals/` (where correctness oracles live), and installs a git pre-commit hook that will start checking for drift once she's got specs in place.

---

## Phase 0: Writing the spec

The harness is spec-anchored by default. To become spec-as-source, Maya needs to start with a spec.

```sh
harness spec new tido --brief "A CLI task manager: add, complete, and report tasks. Persist to ~/.tido/tasks.json."
```

The agent reads the brief and produces three files under `.specs/tido/`:

**`1-requirements.json`** — the machine-readable contract:

```json
{
  "spec": "tido",
  "version": "1.0",
  "owns": ["src/**"],
  "pace_layer": "monthly",
  "requirements": [
    {
      "id": "REQ-001",
      "text": "Users can add a task with a title",
      "acceptance_criteria": [
        "tido add 'Buy milk' exits 0 and prints the new task ID",
        "The task appears in tido list with status TODO"
      ]
    },
    {
      "id": "REQ-002",
      "text": "Users can mark a task complete",
      "acceptance_criteria": [
        "tido done <id> exits 0",
        "tido list shows the task as DONE"
      ]
    },
    {
      "id": "REQ-003",
      "text": "Users can view a daily report",
      "acceptance_criteria": [
        "tido report prints tasks grouped by status",
        "Report shows count of completed vs total tasks"
      ]
    },
    {
      "id": "REQ-004",
      "text": "Tasks persist between invocations",
      "acceptance_criteria": [
        "Tasks added in one process are visible to the next",
        "Data is stored at ~/.tido/tasks.json"
      ]
    }
  ]
}
```

Two fields here are doing important work: `owns` and `pace_layer`.

**`owns`** is the ownership declaration. It tells the harness which files this spec is responsible for — in this case, everything under `src/`. This is the foundation of the whole discipline. Once a spec declares ownership, the harness can detect if those files drift away from what the spec describes, and it can regenerate them from scratch when the spec changes.

**`pace_layer`** describes how stable this component should be. `"monthly"` means Maya expects to regenerate Tido's implementation roughly once a month, when the spec changes in a meaningful way. She wouldn't put `"monthly"` on a public API that other systems depend on (that would be `"yearly"` or even `"never"`), but a CLI tool she controls is fine.

**`2-design.md`** captures the architectural decisions the agent made: it'll use a single JSON file for storage, implement a simple command dispatch in `main`, and keep data structures in a `model.rs` module. Most importantly, it records *why* — the design doc has a `## Decisions` section explaining that a local JSON file was chosen over SQLite because the volume is tiny and the portability matters.

**`3-tasks.jsonl`** breaks the requirements into concrete implementation steps with dependency ordering.

Maya reads all three and edits `2-design.md` to add a decision she cares about: tasks should have an optional due date field, even if the MVP doesn't surface it, because she knows it's coming. She notes it under `## Decisions` with a rationale. This is exactly what that section is for: capturing constraints so they aren't silently discarded the next time the code is regenerated.

---

## Phase 1: The first run

```sh
harness build tido
```

The harness picks up the first eligible task — `T-001: Scaffold the project structure` — and hands it to the agent with a prompt that contains the requirements, the design doc, and the acceptance criteria. The agent produces `src/main.rs`, `src/model.rs`, and `src/storage.rs`.

When the agent finishes, two things happen before the harness moves on.

First, **protected write enforcement**: the harness checks the git diff and verifies the agent didn't touch anything outside `src/`. The spec files under `.specs/`, the eval scripts under `evals/`, and the harness config under `.harness/` are all protected. If the agent had tried to modify `1-requirements.json` — perhaps trying to "fix" an ambiguous requirement by changing the spec rather than asking for clarification — the iteration would fail and the working tree would reset. The spec is the source of truth; the agent is not allowed to rewrite it.

Second, **gate validation**: the harness runs the configured gates — `run_build`, `run_lint`, `run_unit_tests`. If any fail, the iteration is marked as failed, the working tree resets, and the task goes back to Todo for another attempt. The failing gate's output is captured on the task, and on the next attempt it's fed back into the prompt as a "Previous attempt failed — fix this first" section — so the agent debugs the actual error instead of blindly re-running the same approach. (Want to see exactly what the agent will receive? `harness explain T-001`.)

Both checks pass. The harness marks `T-001` done, and before it moves to the next task it does one more thing: **it records the manifest**.

```json
// .harness/manifest.json (excerpt)
{
  "specs": {
    "tido": {
      "spec_inputs_hash": "a3f9c2...",
      "owned_files": {
        "src/main.rs": "7bd42e...",
        "src/model.rs": "c19f84...",
        "src/storage.rs": "e02a71..."
      }
    }
  }
}
```

The manifest records, for each spec, a hash of the spec inputs (requirements + design + tasks) and a hash of each file the spec owns. These two hashes are the heart of Phase 0. Together they answer a single question: *does the code on disk reflect the spec on disk, and has anyone touched it since?*

If either diverges — the spec changes without a regeneration, or someone hand-edits an owned file — the pre-commit hook catches it:

```sh
$ git commit -m "quick fix"
harness check --all
✗ tido: hand-edit detected in owned file: src/storage.rs
error: commit blocked by pre-commit hook
```

This is the enforcement that makes "code is not source" mechanical rather than aspirational. It's not policy — it's a gate.

---

## Phase 2: Writing evals

After the first full run completes, Maya has a working `tido` binary. But there's a gap in the setup: she has acceptance criteria in the requirements, but they're prose. The harness can't execute prose.

She writes eval scripts — one per requirement, living under `evals/tido/`:

```sh
# evals/tido/REQ-001-add-task.sh
#!/usr/bin/env sh
set -e
TMPDIR=$(mktemp -d)
export TIDO_HOME=$TMPDIR

./target/release/tido add "Buy milk" > /tmp/out.txt
grep -q "task-" /tmp/out.txt          # must print an ID
./target/release/tido list | grep -q "Buy milk"  # must appear in list
./target/release/tido list | grep -q "TODO"       # must be TODO status
```

```sh
# evals/tido/REQ-004-persistence.sh
#!/usr/bin/env sh
set -e
TMPDIR=$(mktemp -d)
export TIDO_HOME=$TMPDIR

./target/release/tido add "Persist this"
ID=$(./target/release/tido list | grep "Persist this" | awk '{print $1}')

# Simulate a new process by re-running tido list from scratch
./target/release/tido list | grep -q "$ID"
```

These scripts are deliberately written without reference to the implementation. They don't import test helpers, they don't mock anything, they don't peek at `src/`. They interact with Tido the same way a user would. This is the key property: **the evals define correct behavior, not correct implementation**. A completely different implementation — different file structure, different module layout, different internal types — would pass these evals as long as it behaved correctly.

When `evals/tido/` exists, `harness check` enforces coverage:

```sh
harness check tido
# validates that every requirement ID appears in at least one eval script
✗ tido: requirement REQ-003 has no eval in evals/tido/
```

Maya writes the missing eval and the spec checks clean. Now the evals are the oracle. They define what "correct" means, independently of how the code is structured.

She didn't have to start from a blank file, though. `harness eval draft tido` turns each requirement's `acceptance_criteria` into a stub script under `evals/tido/` — one per requirement, named after its ID. To keep the oracle independent of whatever wrote the code, the draft is produced by the configured *reviewer* model rather than the code agent (pass `--use-code-agent` to override, or set `agent.reviewer_command` to get a genuinely independent one). Crucially the stubs are **drafts, not truth**: each one is hermetic scaffolding with `# TODO`s where the model couldn't ground a detail without reading the implementation, and each `exit 1`s until a human replaces those TODOs with real behaviour-level assertions. So the draft removes the blank-page work without letting a machine-guessed oracle quietly pass — Maya still reads every stub and makes it real.

---

## Phase 3: The spec changes — regenerate, don't patch

Three weeks later, Maya's PM comes back with a change: the `tido report` command should group tasks by due date, not just by status, and tasks should have a required priority field. Two meaningful additions to the spec.

Maya edits `1-requirements.json` directly, adding `REQ-005` and updating `REQ-003`. She edits `2-design.md` to note that the storage format needs a migration path (first run with new version should back up the old file). She also updates the eval for `REQ-003`.

She does not touch `src/`. That's the point.

```sh
harness check tido
✗ tido: spec changed, code not rebuilt — run `harness rebuild tido`
```

The manifest detected that `spec_inputs_hash` changed — the requirements file was edited — and the owned files haven't been regenerated yet. This is the `StaleCode` drift class: spec moved, code didn't follow.

```sh
harness rebuild tido
```

The rebuild sequence:

1. **Checkpoint**: the harness records the current HEAD SHA.
2. **Burn**: every file in `src/` is deleted. Not archived, not diffed — deleted. The files are ashes.
3. **Rebuild**: the agent receives a regeneration prompt containing the current spec and design doc. The prompt explicitly says: *these files have been deleted — recreate them from the spec alone. Do not consult any prior implementation. There is none.* (The agent literally cannot, since the files are gone.)
4. **Gate**: the harness runs the hooks and then the eval scripts. If `REQ-001-add-task.sh` fails, the iteration fails. If `REQ-004-persistence.sh` fails because the migration logic is missing, the iteration fails. The evals are the definition of done.
5. **Record**: on success, the harness records the new baseline automatically.
6. **Commit**: `regen: tido` is committed.

This is "don't refactor, regenerate." The agent isn't asked to patch the old code with the new requirements — it's asked to start from the spec. The result might look completely different internally (different module boundaries, different struct layouts), and that's fine. The evals don't care about the internals.

If the regeneration fails because an eval fails, the harness surfaces the offending clause and stops. It doesn't loop trying to patch code into passing — it tells Maya which requirement failed so she can clarify the spec.

```
[regen] eval evals/tido/REQ-003-report.sh: FAIL (exit 1) — spec may be ambiguous
[regen] Gates failed — rolling back.

Hint: REQ-003 says "group by due date" but does not specify behavior for tasks
with no due date. Clarify the spec before retrying.
```

This is important. When the oracle fails because the spec is underspecified, the right fix is to clarify the spec — not to adjust the code until the eval passes by accident.

---

## Phase 4: Drift detection in reverse

A month later, Maya's co-worker Dmitri has been experimenting. He made some changes to `src/storage.rs` to try a performance idea — he forgot that Tido is under spec-as-source. The pre-commit hook caught it at commit time, so the change never landed. But Maya still wants to understand the gap.

She also wants to know if her own spec is complete. She's been evolving Tido in small ways and isn't sure if the spec still fully describes what the code does.

```sh
harness check tido --reverse
```

The harness drives an agent to read everything under `src/` and reconstruct a spec in Markdown — without being allowed to look at `.specs/tido/`. It writes the result to `.harness/roundtrip/tido.reconstructed.md`.

Then it diffs the reconstruction against the canonical spec:

```
=== Convergence Report: tido ===
Verdict: DRIFT/UNDERSPECIFIED

  Implementation drift — code does not reflect these requirements:
    REQ-005

  Hidden behavior — reconstruction describes things the spec doesn't:
    Review .harness/roundtrip/tido.reconstructed.md for undocumented behavior.
```

`REQ-005` (the due-date grouping in report) didn't make it into the reconstruction — the code doesn't implement it yet. And the reconstruction describes a "quiet mode" flag that exists in the code (`--quiet`) but isn't in the spec.

`--reverse` is advisory: Maya reviews the reconstruction herself, decides the `--quiet` flag is worth keeping, and adds it to the spec. `REQ-005` is a legit gap — she re-runs `harness rebuild tido` to bring the code up to the spec.

---

## Phase 5: Trust under nondeterminism

Tido has a public CLI interface now. Other scripts depend on its output format. Maya promotes it to `public_interface: true` in the spec:

```json
{
  "spec": "tido",
  "owns": ["src/**"],
  "pace_layer": "yearly",
  "public_interface": true,
  ...
}
```

`pace_layer` is now `"yearly"` — the CLI contract is stable, and she only expects to regenerate it for major changes. `public_interface: true` activates two extra safeguards.

**Cross-model review**: when `harness rebuild tido` runs, after the evals pass, it submits the regenerated code to a second, independent model configured as the reviewer. The reviewer reads the requirements and the new code and checks them against each other — not against the old implementation. If the reviewer finds a requirement that the code doesn't satisfy, the regeneration is rejected and rolled back.

```toml
# .harness/harness.toml
[agent]
command         = "claude -p --dangerously-skip-permissions < {prompt_file}"
reviewer_command = "claude --model claude-opus-4-8 -p < {prompt_file}"
```

**Determinism probe**: she can test whether the spec is tight enough to reliably produce equivalent implementations:

```sh
harness check tido --determinism
```

The harness runs the full rebuild sequence N times (`--passes N`, default 2). Every run must pass the evals — that's *behavioral* convergence. On top of that, the harness scores how textually similar the regenerated artifacts are to each other (line-level similarity after normalizing whitespace), so Maya can see whether the spec is tight enough to produce not just *correct* code but *the same* code each time.

```
[regen]   Regeneration complete.
[regen-2] Pass 2 of 2 (determinism probe)…
[regen-2] Regeneration complete.

=== Determinism Report: tido (2 passes) ===
Files (union): 3
  src/main.rs                  near-identical   0.98
  src/model.rs                 identical        1.00
  src/storage.rs               minor-drift      0.88
Byte-identical files: 1/3
Overall convergence: 0.95 (line-weighted)
Verdict: CONVERGED  (behavior: CONVERGED — every pass passed evals)
```

CONVERGED. The spec is tight enough that independent generations both pass evals *and* land on nearly the same artifact. A low convergence score with passing evals is the signal to push more structure into the spec (file layout, naming, signatures) — the artifact variance is telling Maya where the spec leaves the model too much freedom.

---

## What Maya has at the end

Six months from now, when someone new joins the team and opens this repo, they will find:

- **`.specs/tido/1-requirements.json`** — a machine-readable contract with rationale for every decision
- **`.specs/tido/2-design.md`** — architectural notes including *why* choices were made, written at the time they were made
- **`evals/tido/`** — a suite of behavioral tests that define correctness in terms of user-visible behavior, not implementation details
- **`src/`** — generated code that is known to satisfy the spec (the manifest says so), has never been hand-edited (the pre-commit hook enforces this), and can be regenerated from scratch in under a minute

They don't need to reverse-engineer the codebase. The spec is the codebase. The source files are just a rendering.

When requirements change — and they will — the workflow is: edit the spec, run `harness rebuild`, review the diff. Not: find every place the old behavior was hardcoded, patch them, hope you didn't miss one.

---

## The guiding invariant

Everything in this guide flows from one rule:

> **Committed owned code must equal what regeneration from the current spec produces, and it must satisfy the spec's code-independent evals. Humans edit specs and evals; never code.**

The manifest enforces "equal to regeneration." The evals enforce "satisfies the spec." The protected-write gate enforces "never code."

That's it. Everything else — pace layers, determinism probes, round-trip sync — is tooling to make that invariant practical at scale.

---

## Quick reference

| Command | What it does |
|---|---|
| `harness init` | Scaffold `.harness/`, `evals/`, pre-commit gate |
| `harness spec requirements <name> --brief "..."` | Layer 1: draft requirements from a brief |
| `harness spec design <name>` | Layer 2: draft design (gated on requirements) |
| `harness spec tasks <name>` | Layer 3: draft tasks (gated on design) |
| `harness spec new <name> --brief "..."` | Run layers 1→3 in order, each gated |
| `harness spec status <name>` | Show the five-layer ladder + next allowed action |
| `harness check <name>` | Spec well-formed + eval coverage + drift |
| `harness build <name>` | Incremental render loop (spec-anchored) |
| `harness check --all` | Detect drift; exits 2 on any |
| `harness check <name> --accept` | Snapshot current code as the baseline |
| `harness rebuild <name>` | Burn owned files, re-render from spec, gate on evals |
| `harness rebuild <name> --only "src/storage/**"` | Rebuild a subset of owned files |
| `harness check <name> --determinism` | Determinism probe: rebuild N times (`--passes N`, default 2) and report artifact-convergence score |
| `harness check <name> --reverse` | Reconstruct spec from code, emit convergence verdict |
| `harness eval draft <name>` | Draft reviewable eval stubs from acceptance criteria (independent reviewer model) |
| `harness eval run <name>` | Run the oracle against current code |
| `harness gate check` | Verify every referenced gate exists and is executable |
| `harness explain <task>` | Preview the exact prompt the agent would receive (no run) |
| `harness log --follow` | Stream a one-line summary of each iteration as it completes |
| `harness doctor` | Environment/config health check |

**Key fields in `1-requirements.json`:**

| Field | Values | Meaning |
|---|---|---|
| `owns` | `["src/**"]` | Files this spec owns (and can regenerate) |
| `pace_layer` | `weekly` / `monthly` / `yearly` / `never` | How often regeneration is expected |
| `public_interface` | `true` / `false` | Requires cross-model review + protects against casual regen |

**Drift classes reported by `harness check`:**

| Class | Cause | Fix |
|---|---|---|
| `Unrecorded` | Spec has `owns` but no baseline was written | `harness rebuild <spec>` or `harness check <spec> --accept` |
| `StaleCode` | Spec inputs changed since last record | `harness rebuild <spec>` |
| `CodeDrift` | Owned file hash changed (hand-edit) | Revert the edit; code is not source |
| `Missing` | An owned file was deleted outside of rebuild | `harness rebuild <spec>` |
