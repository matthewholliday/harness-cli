# harness

A project-agnostic CLI for running coding agents as a **Ralph loop** with
deterministic, blocking validation gates ("hooks"). Implements the `harness`
design spec (draft v0.1).

The tool ships **no opinion** about your build system, test runner, or language.
You fill in per-project hook scripts, a guardrails policy, and a spec; `harness`
drives a fresh-context agent over the task list one task at a time and decides
"done" by running your hooks — not by trusting the agent's claim.

## Build

The CLI lives in the Cargo workspace under `harness/`.

```sh
cd harness
cargo build --release
# binary at harness/target/release/harness

# …or build + install `harness` (and the optional `harness-gui`) onto your PATH:
../scripts/build-and-reinstall.sh
```

## Quick start

```sh
cd your-project
git init                         # rollback boundary; recommended
harness init                     # scaffold .harness/
# edit .harness/scripts/hooks/* to run your real build/test/lint

# Build the spec one layer at a time — each step is gated on the one before it:
harness spec requirements <name> --brief "what it should do"   # layer 1
harness spec design <name>                                     # layer 2 (needs requirements)
harness spec tasks <name>                                      # layer 3 (needs design)
# …or run all three in order at once:  harness spec new <name> --brief "…"

harness check <name>             # spec well-formed + eval coverage + drift
harness build <name>             # layer 4: generate code (needs requirements+design+tasks)
harness eval draft <name>        # layer 5: draft evals (needs code)
```

## The five-layer model

A spec is built as a **vertical slice** of five layers, produced strictly in
order. Each layer is the input to the next, and the harness makes it
*structurally impossible* to skip ahead — the gate is enforced before any agent
runs, not left to the agent's judgement:

```
requirements → design → tasks → code → evals
```

| layer | artifact | command | requires |
|---|---|---|---|
| 1 requirements | `.specs/<name>/1-requirements.json` | `harness spec requirements` | — |
| 2 design | `.specs/<name>/2-design.md` | `harness spec design` | requirements |
| 3 tasks | `.specs/<name>/3-tasks.jsonl` | `harness spec tasks` | + design |
| 4 code | files matched by the spec's `owns` globs | `harness build` | + tasks |
| 5 evals | `evals/<name>/*` | `harness eval draft` | + code |

Run `harness spec status <name>` to see which layers exist and the single next
allowed action. Each drafting command is also **write-scoped**: it may only
touch its own layer's file(s); anything it writes elsewhere is reverted.

## Guided setup (optional Claude Code agent)

`harness init` installs a Claude Code subagent to
`.claude/agents/harness-setup.md`. Instead of editing the hook stubs by hand,
you can let it configure them for you: it detects your project's
build/test/lint/docs commands, wires the `.harness/scripts/hooks/*` scripts and
`harness.toml`/`guardrails.toml` to match, runs each hook once, and finishes
with `harness doctor`.

In Claude Code, just ask for it:

```
> use the harness-setup agent to configure harness for this repo
```

It detects, **then confirms with you** before writing — it won't invent
commands that don't exist or overwrite hooks you've already filled in. The
canonical definition lives at
[`templates/harness-setup.md`](templates/harness-setup.md); `init` writes a copy
into each project (skipped if one already exists, unless `--force`).

## Commands

The CLI follows one grammar: top-level **verbs** for the lifecycle (`build`,
`rebuild`, `check`…), and **nouns** for managing source objects (`spec`, `eval`,
`gate`). Code is treated as a build artifact rendered from the spec.

| Command | Purpose |
|---|---|
| `harness init [--force]` | Scaffold `.harness/`, `evals/`, and the pre-commit gate. |
| `harness build <spec \| --all> [--once] [--max <N>] [--dry-run]` | Render code from a spec, task by task (incremental, non-destructive). |
| `harness rebuild <spec \| --all> [--only <glob>] [--force]` | Burn the spec's owned files and re-render from scratch (destructive, eval-gated). |
| `harness check [<spec> \| --all]` | The invariant gate: spec well-formed + eval coverage + no drift. |
| `harness check <spec> --reverse` | Reconstruct the spec from code and report convergence (advisory). |
| `harness check <spec> --determinism` | Rebuild twice and compare eval results (spec-tightness probe). |
| `harness check <spec> --accept` | Accept the current code as the spec's baseline (escape hatch). |
| `harness spec requirements <name> [--brief "…" \| --from <file> \| -]` | Layer 1: draft requirements from a brief. |
| `harness spec design <name>` | Layer 2: draft a design from the requirements (gated on layer 1). |
| `harness spec tasks <name>` | Layer 3: draft tasks from the design (gated on layers 1–2). |
| `harness spec new <name> [--brief "…" \| --from <file> \| -]` | Run layers 1→3 in order, each gated (convenience wrapper). |
| `harness spec status <name>` | Show the five-layer ladder and the next allowed action. |
| `harness spec edit <name> [requirements\|design\|tasks]` | Open a spec file in `$EDITOR`, then check it. |
| `harness spec show <name>` | Print a spec's resolved contents. |
| `harness spec ls` | List specs under `.specs/`. |
| `harness spec coverage <name> [--fix]` | Report requirement↔task coverage; `--fix` writes task stubs. |
| `harness eval ls <spec>` | List eval scripts for a spec. |
| `harness eval run <spec>` | Run a spec's evals against the current code. |
| `harness eval draft <spec> [--force] [--use-code-agent]` | Draft reviewable eval stubs from the spec's acceptance criteria. Uses the independent reviewer model by default. |
| `harness gate ls` | List gate (validation hook) scripts. |
| `harness gate check` | Verify every referenced gate exists and is executable (static preflight; exits 2 if broken). |
| `harness gate run <name> [--task <id>]` | Run one gate manually. |
| `harness explain <task> [--spec <name>] [--phase <name>]` | Preview the exact prompt the agent would receive for a task, without running it. |
| `harness status` | Active spec/task and task counts. |
| `harness watch` | Live terminal dashboard. |
| `harness log [<n>] [-f\|--follow]` | List iteration records, show one by number, or stream them live (`--follow`). |
| `harness doctor` | Validate environment and config (agent adapter, gates, git). |

Older verbs (`run`, `regen`, `manifest`, `hooks`, `logs`, `spec list/draft/validate/sync`)
remain as hidden deprecated aliases that print a migration note and dispatch to
the new command for one release.

## Exit codes (top-level)

| Code | Meaning |
|---|---|
| `0` | Success / consistent. |
| `1` | Usage or config error (you typed it wrong). |
| `2` | Invariant violation — drift, failed gate/eval, or blocked tasks remaining. |
| `3` | Agent adapter failure (in `--once` mode). |

## How the loop works

Each iteration:

1. Select the lowest-`priority` `todo` task whose `depends_on` are all `done`
   (across all in-scope specs).
2. Compose a prompt from the template + guardrails + the task + matching
   requirements + design excerpt + `progress.md`. On a retry, the prior attempt's
   captured failure (failing gate output or agent error) is injected so the agent
   can fix the root cause instead of repeating it.
3. Run the agent as a **fresh process** (`[agent].command`, with `{prompt_file}`
   substituted).
4. Run the task's blocking **gates** in order. First blocking failure
   short-circuits the iteration.
5. All blocking gates pass → mark `done`, clear the stored failure, optionally
   `git commit`. Otherwise capture the failure, increment `attempts`, and park as
   `blocked` once the global `[budgets].max_attempts_per_task` cap is reached.
6. Write a structured iteration record and update `state.json` / `progress.md`.

State lives entirely on disk (`.harness/`, `.specs/`), so a run is safe to
Ctrl-C and resume (re-run `harness build`). Preview any task's prompt with
`harness explain <task>`, and stream iterations live with `harness log --follow`.

## Hook contract

A hook is any executable in `.harness/scripts/hooks/`. It receives:

- **Env:** `HARNESS_HOOK`, `HARNESS_TASK_ID`, `HARNESS_SPEC`, `HARNESS_ITERATION`,
  `HARNESS_ATTEMPT`, `HARNESS_ROOT`.
- **Stdin:** the task object as JSON.
- **Exit code:** `0` = pass; non-zero = fail/block.

Full output is captured to `.harness/logs/hooks/`; a head+tail excerpt lands in
the iteration record. By convention all hooks block except `run_e2e_tests`
(non-blocking by default); override per hook in `guardrails.toml`.

## Example: a TypeScript project driven by Claude Code

This walks through wiring `harness` into a typical TypeScript project that uses
`tsc`, ESLint, and Vitest, with [Claude Code](https://claude.com/claude-code)
as the agent.

### 1. Point the agent adapter at Claude Code

In `.harness/harness.toml`, set `[agent].command`. `harness` writes the composed
prompt to a temp file each iteration and substitutes its path for
`{prompt_file}`; Claude Code reads it on stdin in non-interactive (`-p`) mode:

```toml
[agent]
# Fresh `claude` process per iteration, reading the prompt from stdin.
command = "claude -p --dangerously-skip-permissions < {prompt_file}"
working_dir = "."

[loop]
max_iterations = 50
commit_each_success = true
reset_on_failure = true        # restore the tree to last commit after a failed iteration

[hooks]
default = ["run_build", "run_lint", "run_unit_tests"]
default_timeout_secs = 600
```

> `--dangerously-skip-permissions` lets the agent edit files unattended. Pair it
> with the `[writes]` allowlist in `guardrails.toml` and the per-iteration git
> commit so every green state is recoverable.

### 2. Write the hook scripts

Each hook is just an executable in `.harness/scripts/hooks/` that exits `0` on
pass. `harness init` creates stubs; replace their bodies with the real commands.

`.harness/scripts/hooks/run_build` — type-check with `tsc`:

```sh
#!/usr/bin/env sh
# Build = the TypeScript compiles with no errors.
npx tsc --noEmit
```

`.harness/scripts/hooks/run_lint` — ESLint over the source:

```sh
#!/usr/bin/env sh
# Lint = ESLint passes (and Prettier, if you use it).
npx eslint . --max-warnings=0
```

`.harness/scripts/hooks/run_unit_tests` — Vitest in run-once mode:

```sh
#!/usr/bin/env sh
# Unit tests = the Vitest suite is green. CI=true keeps it non-interactive.
CI=true npx vitest run
```

`.harness/scripts/hooks/run_e2e_tests` — Playwright (non-blocking by default):

```sh
#!/usr/bin/env sh
# End-to-end = Playwright. Kept non-blocking locally (see guardrails below).
CI=true npx playwright test
```

Make them executable (the stubs `init` writes already are; new ones need it):

```sh
chmod +x .harness/scripts/hooks/*
```

A hook can read the task as JSON on stdin if it wants to scope its work — for
example, only running tests for the files a task touched. With Node available
you can parse it inline:

```sh
#!/usr/bin/env sh
# Scope Vitest to the task's files_hint, if any were provided.
PATTERNS=$(node -e 'const t=JSON.parse(require("fs").readFileSync(0,"utf8")); process.stdout.write((t.files_hint||[]).join(" "))')
if [ -n "$PATTERNS" ]; then
  CI=true npx vitest run $PATTERNS
else
  CI=true npx vitest run
fi
```

### 3. Keep e2e advisory, scope writes

In `.harness/guardrails/guardrails.toml`:

```toml
[writes]
allow = ["src/**", "tests/**", "docs/**", ".specs/**"]
deny  = [".harness/guardrails/**", ".git/**", "**/.env*"]

[operations]
deny_destructive = true

# Playwright is slow and flaky without a server — record it but don't block on it.
[hooks.run_e2e_tests]
blocking = false
timeout_secs = 1800
```

### 4. Reference hooks per task

A task in `.specs/<name>/3-tasks.jsonl` can override the default hook set via its
`hooks` field — e.g. a pure type-model change that needs the compiler and linter
but no tests yet:

```json
{"id":"T-001","spec":"api","title":"Add User type and zod schema","requirements":["REQ-001"],"status":"todo","priority":1,"depends_on":[],"hooks":["run_build","run_lint"],"acceptance":["User schema parses a valid payload","tsc passes"],"files_hint":["src/models/user.ts"],"attempts":0,"notes":"","created_at":"2026-06-20T00:00:00Z","updated_at":"2026-06-20T00:00:00Z"}
```

Tasks that omit `hooks` fall back to `[hooks].default` from `harness.toml`.

### 5. Verify and run

```sh
harness doctor                   # confirms claude is callable, gates exist, git is present
harness gate run run_build       # smoke-test a single gate by hand
harness check api
harness build api                # drive the loop; each task is built/linted/tested before it counts as done
```

If `tsc` or Vitest fails, the iteration fails: `harness` increments the task's
`attempts`, restores the working tree to the last clean commit (because
`reset_on_failure = true`), and either retries or parks the task as `blocked`
once the global `[budgets].max_attempts_per_task` cap is hit — so a broken type
or red test never gets recorded as done on the agent's say-so.

## Status

v0.1 — **pre-1.0**. The CLI surface and on-disk formats (`.specs/`, `.harness/`,
the JSON/JSONL layer schemas) are not yet stable and may change between 0.x
releases; pin a version if you script against them.

Implemented: the full loop, gates, the gated five-layer pipeline
(requirements → design → tasks → code → evals) with per-layer write scoping,
drift detection (`check`), burn-and-rebuild (`rebuild`), evals, logging, and all
CLI commands above. Not yet implemented: full deterministic task regeneration
from requirements (`spec coverage --fix` only writes stubs today).
