# harness

A project-agnostic CLI for running coding agents as a **Ralph loop** with
deterministic, blocking validation gates ("hooks"). Implements the `harness`
design spec (draft v0.1).

The tool ships **no opinion** about your build system, test runner, or language.
You fill in per-project hook scripts, a guardrails policy, and a spec; `harness`
drives a fresh-context agent over the task list one task at a time and decides
"done" by running your hooks — not by trusting the agent's claim.

## Build

The repository is a Cargo workspace with three crates:

- `harness-core` — the shared data/logic library (config, state, specs, the loop, the snapshot read model).
- `harness-cli` — the `harness` command-line tool (CLI + terminal dashboard).
- `harness-gui` — an optional desktop dashboard (egui).

```sh
cargo build --release                  # builds every crate
# CLI binary at target/release/harness
# GUI binary at target/release/harness-gui

cargo build --release -p harness-cli   # CLI only (no egui dependencies)
```

## Quick start

```sh
cd your-project
git init                         # rollback boundary; recommended
harness init                     # scaffold .harness/
# edit .harness/scripts/hooks/* to run your real build/test/lint
harness gate check               # confirm the gate scripts are wired and executable
# author a spec under .specs/<name>/ (1-requirements.json, 2-design.md, 3-tasks.jsonl)
harness check <name>             # spec well-formed + eval coverage + no drift
harness build --dry-run --once   # preview task selection
harness build                    # drive the loop
```

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

| Command | Purpose |
|---|---|
| `harness init [--force]` | Scaffold `.harness/` (config, prompts, guardrails, gate stubs, logs). |
| `harness build [<spec> \| --all] [--once] [--max <N>] [--dry-run]` | Render code from a spec, task by task (incremental, non-destructive). The Ralph loop. |
| `harness rebuild [<spec> \| --all] [--only <glob>] [--force]` | Burn a spec's owned files and re-render from the spec (destructive, eval-gated). |
| `harness check [<spec> \| --all] [--reverse] [--determinism] [--accept]` | The invariant gate: spec well-formed + eval coverage + no manifest drift. Exits `2` on any problem. |
| `harness spec ls` | List specs under `.specs/`. |
| `harness spec new <name> [--brief <text> \| --from <file>]` | Draft a new spec from a brief (agent-assisted). |
| `harness spec edit <name> [requirements\|design\|tasks]` | Open a spec file in `$EDITOR`, then check it. |
| `harness spec show <name>` | Print a spec's resolved contents. |
| `harness spec tasks <name> [--fix]` | Report requirement↔task coverage; `--fix` writes task stubs. |
| `harness eval ls <spec>` / `harness eval run <spec>` | List or run a spec's evals (the acceptance oracle). |
| `harness eval draft <spec> [--force] [--use-code-agent]` | Draft reviewable eval stubs from the spec's acceptance criteria (independent reviewer model by default; stubs `exit 1` until a human fills in the TODOs). |
| `harness gate ls` | List gate scripts. |
| `harness gate check` | Verify every referenced gate exists and is executable (static preflight). Exits `2` if any is broken. |
| `harness gate run <gate> [--task <id>]` | Run one gate manually. |
| `harness explain <task> [--spec <name>] [--phase <name>]` | Preview the exact prompt the agent would receive for a task, without running it. |
| `harness status` | Active spec/task and task counts. |
| `harness watch` | Live terminal dashboard (TUI) that visualizes a run as it happens. |
| `harness log [<n>] [--follow]` | List iteration records, show one by number, or `--follow` to stream them live. |
| `harness doctor` | Validate config, gates, agent adapter, git. |

> Earlier verbs (`run`, `regen`, `hooks`, `logs`, `spec list/draft/validate/sync`,
> `manifest`) still work as hidden, deprecated aliases that print a migration note.

## Exit codes (top-level)

| Code | Meaning |
|---|---|
| `0` | Success / loop completed. |
| `1` | Usage or config error. |
| `2` | Loop stopped with blocked tasks remaining. |
| `3` | Agent adapter failure (in `--once` mode). |

## How the loop works

Each iteration:

1. Select the lowest-`priority` `todo` task whose `depends_on` are all `done`
   (across all in-scope specs).
2. Compose a prompt from the template + guardrails + the task + matching
   requirements + design excerpt + `progress.md`. If the task failed a previous
   attempt, the captured failure (failing gate output or agent error) is injected
   as a **"Previous attempt failed — fix this first"** section so the agent can
   diagnose and self-correct rather than blindly repeat the same approach.
3. Run the agent as a **fresh process** (`[agent].command`, with `{prompt_file}`
   substituted).
4. Run the task's blocking **gates** in order. First blocking failure
   short-circuits the iteration.
5. All blocking gates pass → mark `done`, clear the stored failure, optionally
   `git commit`. Otherwise capture the failure into the task's `last_failure`,
   increment `attempts`, and park as `blocked` once `max_attempts` is reached.
6. Write a structured iteration record and update `state.json` / `progress.md`.

State lives entirely on disk (`.harness/`, `.specs/`), so a run is safe to
Ctrl-C and resume — just re-run `harness build` and it picks up the remaining
tasks. To preview what the agent will see for a given task before (or after) a
run, use `harness explain <task>`.

## Watching a run live

`harness watch` opens a read-only terminal dashboard that re-reads the on-disk
run state a few times a second and paints it. Because the loop already persists
everything as it goes (`state.json`, `iterations/*.json`, `progress.md`, and the
per-spec `3-tasks.jsonl`), the dashboard never touches the loop — run it in a
second terminal next to `harness build`, or open it after a run to replay what
happened.

```sh
# terminal 1
harness build

# terminal 2
harness watch
```

For a lighter-weight, log-only view (no TUI), `harness log --follow` streams a
one-line summary of each iteration as it completes — handy over SSH or when piping
to a file.

### Desktop dashboard (egui)

`harness-gui` is a native desktop dashboard rendering the same on-disk state as
`harness watch`, plus things a terminal can't do well: **Start/Stop** controls
that launch `harness build` for you, live streaming of the run's output, a spec
browser, and a per-task phase grid.

```sh
harness-gui                 # search upward from the cwd for a .harness/ project
harness-gui /path/to/proj   # or point it at a project root
```

Like `watch`, it never owns run state — it reads the loop's files on a timer and,
when you press Start, shells out to the `harness` CLI. It resolves the CLI as
`$HARNESS_BIN`, else a `harness` binary next to `harness-gui`, else `harness` on
`PATH`.

It shows:

- **Header** — run status (`RUNNING` / `IDLE` / `COMPLETE` / `STOPPED`), active
  spec, iteration vs. budget, elapsed time, and the phase sequence. "Running" is
  inferred from an in-progress task or a recent write to the log files.
- **Progress gauge** — done / active / todo / blocked counts as a bar.
- **Task table** — every task across all in-scope specs, in-progress first, with
  status, attempts, and per-task phase progress. Select with `↑/↓` (or `j/k`).
- **Task detail** — for the selected task: requirements/deps, phase checklist,
  the latest iteration's agent exit and per-hook pass/fail + timings, the commit
  sha, and the last failure note.
- **Progress log** — the tail of `progress.md`, colorized by outcome.

Keys: `↑/↓` select · `g/G` jump to top/bottom · `r` force refresh · `q` quit.
The view auto-refreshes ~every 0.4s.

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
{"id":"T-001","spec":"api","title":"Add User type and zod schema","requirements":["REQ-001"],"status":"todo","priority":1,"depends_on":[],"hooks":["run_build","run_lint"],"acceptance":["User schema parses a valid payload","tsc passes"],"files_hint":["src/models/user.ts"],"attempts":0,"max_attempts":3,"notes":"","created_at":"2026-06-20T00:00:00Z","updated_at":"2026-06-20T00:00:00Z"}
```

Tasks that omit `hooks` fall back to `[hooks].default` from `harness.toml`.

### 5. Verify and run

```sh
harness doctor                   # confirms claude is callable, gates exist, git is present
harness gate check               # confirm every referenced gate is present and executable
harness gate run run_build       # smoke-test a single gate by hand
harness check api                # spec well-formed + eval coverage + no drift
harness build                    # drive the loop; each task is built/linted/tested before it counts as done
```

If `tsc` or Vitest fails, the iteration fails: `harness` increments the task's
`attempts`, restores the working tree to the last clean commit (because
`reset_on_failure = true`), and either retries or parks the task as `blocked`
once `max_attempts` is hit — so a broken type or red test never gets recorded as
done on the agent's say-so.

## Status

v0.1. Implemented: the full loop with prior-failure feedback into retries,
gates (with `gate check` preflight), spec authoring/coverage, the invariant
`check`, destructive `rebuild`, prompt preview (`explain`), live `watch` and
`log --follow`, and all CLI commands above. Not yet implemented: write-allowlist
sandboxing and cross-model review.
