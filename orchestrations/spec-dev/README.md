# Spec Development Orchestration

## Purpose
This orchestration turns a product spec into a strictly serial, multi-agent software delivery workflow. It coordinates a lifecycle manager, decomposes work into ordered tasks, and runs each task through direction, planning, implementation, QA, and verification. The end state is a completed MAPS task file plus structured per-task outputs and call logs.

## Workflow
1. Load spec input (`--spec-file` or `--spec-text`) and initialize MAPS task state.
2. Run `lifecycle-manager`.
3. Run `spec-to-task-decomposer` and read ordered tasks.
4. For each task, run serially:
   - `software-development-director`
   - `software-development-manager`
   - `software-architect`
   - `implementation-planner`
   - `unit-test-writer`
   - `test-driven-developer`
   - `quality-assurance-analyst`
   - `verifier` (last)
5. Append MAPS terminal `result` reply on successful completion.

## Agents
| Name | Role | File |
| --- | --- | --- |
| lifecycle-manager | Lifecycle setup and gating | `agents/lifecycle-manager.md` (custom) |
| spec-to-task-decomposer | Spec to ordered task list decomposition | `agents/spec-to-task-decomposer.md` (custom) |
| software-development-director | Task-level direction and acceptance framing | `agents/software-development-director.md` (custom) |
| software-development-manager | Execution management and checkpoints | `agents/software-development-manager.md` (custom) |
| software-architect | Architecture and contract design | `agents/software-architect.md` (custom) |
| implementation-planner | Ordered implementation planning | `agents/implementation-planner.md` (custom) |
| unit-test-writer | Unit-test strategy and definitions | `agents/unit-test-writer.md` (custom) |
| test-driven-developer | TDD implementation progression | `agents/test-driven-developer.md` (custom) |
| quality-assurance-analyst | QA assessment and risk review | `agents/quality-assurance-analyst.md` (custom) |
| verifier | Final task verification output | `agents/verifier.md` (custom) |

## Setup
1. Copy or unpack this `spec-dev` folder into the target Cursor repository under `orchestrations/spec-dev/`.
2. Verify Cursor Headless CLI: `cursor-agent --version`.
3. Preferred setup: `./setup_recipient.sh`.
4. Manual fallback:
   - Create target agent directory: `mkdir -p .cursor/agents`
   - Copy all files from `agents/*.md` into `.cursor/agents/` (or `~/.cursor/agents/` for user scope).
5. Python dependencies: none (stdlib only).

## Usage
- Basic run: `python run_orchestration.py --spec-file path/to/spec.md`
- Inline spec text: `python run_orchestration.py --spec-text "Build feature X ..."`
- Common flags: `--dry-run`, `--workdir`, `--output-format {text,json,stream-json}`, `--verbose`, `--max-retries`, `--max-consecutive-failures`, `--max-task-retries`
- Example:
  - `python run_orchestration.py --spec-file ../spec.md --output-format json`
  - Expected shape: JSON with `tasks_executed`, `task_summaries`, and `run_dir`.

## Testing
Run: `python run_tests.py`

## Configuration
- `CURSOR_AGENT_BIN`: override CLI binary path (default: `cursor-agent`)
- `CURSOR_API_KEY`: optional for non-interactive/CI environments
- Retry/circuit breaker knobs:
  - `--max-retries` (default: 10)
  - `--base-backoff-s`
  - `--max-backoff-s`
  - `--max-consecutive-failures`
  - `--max-task-retries`: replays a task's full development chain (director through verifier) on `OrchestrationError`. Defaults to `MAX_TASK_RETRIES` from `settings.py`.
- Settings file: defaults live in `settings.py` (currently `MAX_TASK_RETRIES = 5`). Edit constants there to change defaults; CLI flags still override.

## Troubleshooting
- `cursor-agent: command not found`: install Cursor Headless CLI and verify PATH.
- JSON parse failure from a subagent: rerun with `--verbose` and ensure subagent emits valid JSON when required.
- Missing subagent resolution: confirm all `agents/*.md` are copied to `.cursor/agents/`.
- Immediate MAPS abort: inspect task file for malformed handoff/reply schema.
- Frequent retries then abort: increase retry knobs or fix persistent subagent prompt failure.
