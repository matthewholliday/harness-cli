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

## Hook contract

A hook is any executable in `.harness/scripts/hooks/`. It receives:

- **Env:** `HARNESS_HOOK`, `HARNESS_TASK_ID`, `HARNESS_SPEC`, `HARNESS_ITERATION`,
  `HARNESS_ATTEMPT`, `HARNESS_ROOT`.
- **Stdin:** the task object as JSON.
- **Exit code:** `0` = pass; non-zero = fail/block.

Full output is captured to `.harness/logs/hooks/`; a head+tail excerpt lands in
the iteration record. By convention all hooks block except `run_e2e_tests`
(non-blocking by default); override per hook in `guardrails.toml`.

## Status

v0.1. Implemented: the full loop, hooks, spec validation/sync (read-only),
logging, and all CLI commands above. Not yet implemented: agent-assisted
`spec draft`, `spec sync --write/--regen-tasks/--against-code`, write-allowlist
enforcement/sandboxing, and cross-model review.
