# harness

A project-agnostic CLI for running coding agents as a **Ralph loop** with
deterministic, blocking validation gates ("hooks"). Implements the `harness`
design spec (draft v0.1).

The tool ships **no opinion** about your build system, test runner, or language.
You fill in per-project hook scripts, a guardrails policy, and a spec; `harness`
drives a fresh-context agent over the task list one task at a time and decides
"done" by running your hooks — not by trusting the agent's claim.

## Build

```sh
cargo build --release
# binary at target/release/harness
```

## Quick start

```sh
cd your-project
git init                         # rollback boundary; recommended
harness init                     # scaffold .harness/
# edit .harness/scripts/hooks/* to run your real build/test/lint
# author a spec under .specs/<name>/ (1-requirements.json, 2-design.md, 3-tasks.jsonl)
harness spec validate <name>
harness run --dry-run --once     # preview task selection
harness run                      # drive the loop
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
| `harness init [--from-specs] [--force]` | Scaffold `.harness/` (config, prompts, guardrails, hook stubs, logs). |
| `harness spec list` | List specs under `.specs/`. |
| `harness spec draft <name>` | Drafting guidance (agent-assisted drafting not yet automated). |
| `harness spec edit <name> [--requirements\|--design\|--tasks]` | Open a spec file in `$EDITOR`, then validate. |
| `harness spec validate <name \| --all>` | Check JSON/JSONL, design headings, requirement refs, hook existence, dependency DAG. |
| `harness spec sync <name>` | Report drift between requirements and tasks (read-only). |
| `harness run [--spec <n>] [--once] [--max-iterations <N>] [--dry-run]` | Run the Ralph loop. |
| `harness hooks list` | List hook scripts. |
| `harness hooks run <hook> [--task <id>]` | Run one hook manually. |
| `harness status` | Active spec/task and task counts. |
| `harness watch` | Live terminal dashboard (TUI) that visualizes a run as it happens. |
| `harness logs [--iteration <n>]` | List iteration records or show one. |
| `harness doctor` | Validate config, hooks, agent adapter, git. |

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
   requirements + design excerpt + `progress.md`.
3. Run the agent as a **fresh process** (`[agent].command`, with `{prompt_file}`
   substituted).
4. Run the task's blocking **hooks** in order. First blocking failure
   short-circuits the iteration.
5. All blocking hooks pass → mark `done`, optionally `git commit`. Otherwise
   increment `attempts`; park as `blocked` once `max_attempts` is reached.
6. Write a structured iteration record and update `state.json` / `progress.md`.

State lives entirely on disk (`.harness/`, `.specs/`), so a run is safe to
Ctrl-C and resume.

## Watching a run live

`harness watch` opens a read-only terminal dashboard that re-reads the on-disk
run state a few times a second and paints it. Because the loop already persists
everything as it goes (`state.json`, `iterations/*.json`, `progress.md`, and the
per-spec `3-tasks.jsonl`), the dashboard never touches the loop — run it in a
second terminal next to `harness run`, or open it after a run to replay what
happened.

```sh
# terminal 1
harness run

# terminal 2
harness watch
```

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
harness doctor                   # confirms claude is callable, hooks exist, git is present
harness hooks run run_build      # smoke-test a single hook by hand
harness spec validate api
harness run                      # drive the loop; each task is built/linted/tested before it counts as done
```

If `tsc` or Vitest fails, the iteration fails: `harness` increments the task's
`attempts`, restores the working tree to the last clean commit (because
`reset_on_failure = true`), and either retries or parks the task as `blocked`
once `max_attempts` is hit — so a broken type or red test never gets recorded as
done on the agent's say-so.

## Status

v0.1. Implemented: the full loop, hooks, spec validation/sync (read-only),
logging, and all CLI commands above. Not yet implemented: agent-assisted
`spec draft`, `spec sync --write/--regen-tasks/--against-code`, write-allowlist
enforcement/sandboxing, and cross-model review.
