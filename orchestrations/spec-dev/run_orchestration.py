#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import random
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from logging_system import CallResult, RunLog
from settings import MAX_TASK_RETRIES

DEFAULT_TIMEOUT_S = 60
DEFAULT_MAX_RETRIES = 10
DEFAULT_MAX_CONSECUTIVE_FAILURES = 3
REPLY_HEADER_RE = re.compile(r"^## Reply\s+(\d+)\s+[—-]\s+([a-z]+)\s*$", re.MULTILINE)
ALLOWED_REPLY_TYPES = {"result", "escalation", "handoff", "aborted"}


class OrchestrationError(RuntimeError):
    """Raised when orchestration execution fails."""


@dataclass(frozen=True)
class AgentResult:
    stdout: str
    stderr: str
    returncode: int
    duration_ms: int
    result: dict[str, Any] | None = None


def run_agent(
    prompt: str,
    *,
    agent: str | None = None,
    force: bool = False,
    output_format: str = "text",
    extra_args: list[str] | None = None,
) -> AgentResult:
    start = time.time()
    binary = Path().expanduser().resolve()
    _ = binary  # keep linter quiet for stdlib-only constraints
    agent_bin = str(Path((__import__("os").environ.get("CURSOR_AGENT_BIN", "cursor-agent"))))
    argv = [agent_bin, "-p", prompt, "--output-format", output_format]
    if agent:
        argv.extend(["--agent", agent])
    if force:
        argv.append("--force")
    if extra_args:
        argv.extend(extra_args)

    completed = subprocess.run(
        argv,
        capture_output=True,
        text=True,
        check=False,
    )
    duration_ms = int((time.time() - start) * 1000)
    if completed.returncode != 0:
        raise OrchestrationError(
            f"cursor-agent failed with exit code {completed.returncode}: {completed.stderr.strip()}"
        )

    parsed: dict[str, Any] | None = None
    if output_format == "json":
        try:
            parsed = json.loads(completed.stdout)
        except json.JSONDecodeError as exc:
            raise OrchestrationError("cursor-agent returned malformed JSON output") from exc

    return AgentResult(
        stdout=completed.stdout,
        stderr=completed.stderr,
        returncode=completed.returncode,
        duration_ms=duration_ms,
        result=parsed,
    )


def _parse_frontmatter(text: str) -> tuple[dict[str, Any], str]:
    if not text.startswith("---\n"):
        raise OrchestrationError("task file is missing YAML frontmatter")
    end = text.find("\n---\n", 4)
    if end == -1:
        raise OrchestrationError("task file frontmatter is not closed")
    frontmatter_text = text[4:end]
    body = text[end + 5 :]
    parsed: dict[str, Any] = {}
    current_map: str | None = None
    for raw in frontmatter_text.splitlines():
        if not raw.strip():
            continue
        if raw.startswith("  ") and current_map is not None:
            k, _, value = raw.strip().partition(":")
            parsed.setdefault(current_map, {})
            parsed[current_map][k.strip()] = value.strip()
            continue
        key, sep, value = raw.partition(":")
        if not sep:
            continue
        key = key.strip()
        value = value.strip()
        if value:
            parsed[key] = value
            current_map = None
        else:
            parsed[key] = {}
            current_map = key
    return parsed, body


def _parse_replies(body: str) -> list[tuple[int, str, str]]:
    matches = list(REPLY_HEADER_RE.finditer(body))
    replies: list[tuple[int, str, str]] = []
    for idx, match in enumerate(matches):
        start = match.end()
        end = matches[idx + 1].start() if idx + 1 < len(matches) else len(body)
        replies.append((int(match.group(1)), match.group(2), body[start:end].strip()))
    return replies


def _extract_assumption_ids(text: str) -> set[str]:
    lines = text.splitlines()
    in_assumptions = False
    ids: set[str] = set()
    for line in lines:
        stripped = line.strip()
        if not in_assumptions:
            if stripped == "assumptions:":
                in_assumptions = True
            continue
        if not line.startswith(" ") and not line.startswith("\t"):
            break
        matched = re.match(r"^[ \t]+([A-Za-z0-9_.-]+)\s*:", line)
        if matched:
            ids.add(matched.group(1))
    return ids


def is_terminal_state(task_file: Path) -> bool:
    text = task_file.read_text(encoding="utf-8")
    _, body = _parse_frontmatter(text)
    replies = _parse_replies(body)
    if not replies:
        return False
    return replies[-1][1] in {"result", "aborted"}


def load_handoff(task_file: Path) -> dict[str, Any]:
    text = task_file.read_text(encoding="utf-8")
    frontmatter, body = _parse_frontmatter(text)
    required = ("goal", "done_when", "assumptions", "escalate_if")
    missing = [key for key in required if key not in frontmatter]
    if missing:
        append_reply(task_file, "aborted", {"reason": "protocol_validation_failed", "missing": missing})
        raise OrchestrationError(f"handoff missing keys: {missing}")

    assumptions_ids = _extract_assumption_ids(text[: text.find("\n---\n", 4)])
    if not assumptions_ids:
        append_reply(
            task_file,
            "aborted",
            {"reason": "protocol_validation_failed", "details": "assumptions IDs missing"},
        )
        raise OrchestrationError("handoff assumptions IDs missing")

    frontmatter["_body"] = body
    return frontmatter


def validate_reply_against_current_assumptions(
    task_file: Path, reply_type: str, payload: dict[str, Any]
) -> None:
    text = task_file.read_text(encoding="utf-8")
    frontmatter_text = text[4 : text.find("\n---\n", 4)]
    known_ids = _extract_assumption_ids(frontmatter_text)
    if reply_type == "result":
        confirmed = payload.get("confirmed")
        if not isinstance(confirmed, list) or set(confirmed) != known_ids:
            raise OrchestrationError("result confirmed IDs must match assumptions")
    elif reply_type == "escalation":
        violated = payload.get("violated")
        observed = payload.get("observed")
        if not isinstance(violated, list) or not violated or not observed:
            raise OrchestrationError("escalation requires non-empty violated and observed")
        unknown = set(violated) - known_ids
        if unknown:
            raise OrchestrationError(f"escalation contains unknown IDs: {sorted(unknown)}")
    elif reply_type == "handoff":
        assumptions = payload.get("assumptions")
        if assumptions is not None and not isinstance(assumptions, dict):
            raise OrchestrationError("handoff assumptions must be a mapping")
    elif reply_type == "aborted":
        if payload.get("reason") != "protocol_validation_failed":
            raise OrchestrationError("aborted reason must be protocol_validation_failed")
    else:
        raise OrchestrationError(f"invalid reply type: {reply_type}")


def append_reply(task_file: Path, reply_type: str, payload: dict[str, Any]) -> None:
    if reply_type not in ALLOWED_REPLY_TYPES:
        raise OrchestrationError(f"invalid reply type: {reply_type}")
    if is_terminal_state(task_file):
        raise OrchestrationError("terminal MAPS state already reached")
    validate_reply_against_current_assumptions(task_file, reply_type, payload)
    text = task_file.read_text(encoding="utf-8")
    _, body = _parse_frontmatter(text)
    next_index = len(_parse_replies(body)) + 1
    lines = [f"\n## Reply {next_index} - {reply_type}"]
    for key, value in payload.items():
        if isinstance(value, list):
            values = ", ".join(str(item) for item in value)
            lines.append(f"{key}: [{values}]")
        elif isinstance(value, dict):
            lines.append(f"{key}:")
            for child_key, child_value in value.items():
                lines.append(f"  {child_key}: {child_value}")
        elif isinstance(value, str) and "\n" in value:
            lines.append(f"{key}: |")
            for part in value.splitlines():
                lines.append(f"  {part}")
        else:
            lines.append(f"{key}: {value}")
    task_file.write_text(text + "\n".join(lines) + "\n", encoding="utf-8")


def _read_spec(spec_file: Path | None, spec_text: str | None) -> str:
    if bool(spec_file) == bool(spec_text):
        raise OrchestrationError("exactly one of --spec-file or --spec-text is required")
    if spec_text:
        return spec_text
    return spec_file.read_text(encoding="utf-8")


def _call_with_retry(
    *,
    prompt: str,
    agent: str,
    output_format: str,
    max_retries: int,
    base_backoff_s: float,
    max_backoff_s: float,
    dry_run: bool,
    run_log: RunLog,
    parent_chain: list[str],
    verbose: bool,
) -> AgentResult:
    attempts = max_retries + 1
    for attempt in range(1, attempts + 1):
        if dry_run:
            fake = AgentResult(stdout=f"[dry-run] {agent}", stderr="", returncode=0, duration_ms=0, result=None)
            run_log.log_call(
                parent_chain=parent_chain,
                child=agent,
                duration_s=0.0,
                result=CallResult(status="success"),
                extras={"dry_run": True, "attempt": attempt},
            )
            return fake
        try:
            result = run_agent(prompt, agent=agent, output_format=output_format)
            run_log.log_call(
                parent_chain=parent_chain,
                child=agent,
                duration_s=result.duration_ms / 1000.0,
                result=CallResult(status="success"),
                extras={"attempt": attempt, "output_format": output_format},
            )
            return result
        except OrchestrationError as exc:
            run_log.log_call(
                parent_chain=parent_chain,
                child=agent,
                duration_s=0.0,
                result=CallResult(status="failure", error=str(exc)),
                extras={"attempt": attempt},
            )
            if attempt >= attempts:
                raise
            sleep_for = min(max_backoff_s, base_backoff_s * (2 ** (attempt - 1)))
            sleep_for = sleep_for + random.uniform(0.0, sleep_for * 0.2)
            if verbose:
                print(f"retrying {agent} after failure ({attempt}/{attempts})", file=sys.stderr)
            time.sleep(sleep_for)
    raise OrchestrationError("retry loop exited unexpectedly")


def _execute(args: argparse.Namespace) -> dict[str, Any]:
    spec = _read_spec(args.spec_file, args.spec_text)
    workdir = args.workdir.resolve()
    task_file = workdir / "tasks" / time.strftime("%Y-%m-%d", time.gmtime()) / "spec-dev.md"
    task_file.parent.mkdir(parents=True, exist_ok=True)
    if not task_file.exists():
        task_file.write_text(
            "---\n"
            "goal: Implement software from provided specification.\n"
            "done_when: All generated tasks are implemented and verified.\n"
            "assumptions:\n"
            "  A: Specification is complete enough for decomposition.\n"
            "escalate_if:\n"
            "  - Any assumption above is observed false.\n"
            "limits:\n"
            f"  timeout_s: {DEFAULT_TIMEOUT_S}\n"
            f"  max_retries: {DEFAULT_MAX_RETRIES}\n"
            f"context: {spec[:1200]}\n"
            "---\n",
            encoding="utf-8",
        )
    load_handoff(task_file)

    run_log = RunLog.create(workdir / "logs")
    parent_chain = ["orchestration"]
    consecutive_failures = 0

    def invoke(agent: str, prompt: str, *, fmt: str = "text") -> AgentResult:
        nonlocal consecutive_failures
        try:
            result = _call_with_retry(
                prompt=prompt,
                agent=agent,
                output_format=fmt,
                max_retries=args.max_retries,
                base_backoff_s=args.base_backoff_s,
                max_backoff_s=args.max_backoff_s,
                dry_run=args.dry_run,
                run_log=run_log,
                parent_chain=parent_chain,
                verbose=args.verbose,
            )
            consecutive_failures = 0
            return result
        except OrchestrationError:
            consecutive_failures += 1
            if consecutive_failures >= args.max_consecutive_failures:
                append_reply(
                    task_file,
                    "aborted",
                    {"reason": "protocol_validation_failed", "details": "circuit_breaker_open"},
                )
                raise OrchestrationError("circuit breaker opened")
            raise

    lifecycle = invoke("lifecycle-manager", f"Prepare lifecycle for spec development.\n\nSpec:\n{spec}")
    _ = lifecycle
    decompose = invoke(
        "spec-to-task-decomposer",
        f"Decompose this spec into serial implementation tasks as JSON array named tasks.\n\n{spec}",
        fmt="json",
    )
    tasks_payload = decompose.result or {}
    tasks = tasks_payload.get("result", {}).get("tasks") if isinstance(tasks_payload.get("result"), dict) else None
    if not isinstance(tasks, list):
        raise OrchestrationError("decomposer output must include result.tasks list")

    task_summaries: list[dict[str, Any]] = []
    chain = [
        "software-development-director",
        "software-development-manager",
        "software-architect",
        "implementation-planner",
        "unit-test-writer",
        "test-driven-developer",
        "quality-assurance-analyst",
    ]
    def run_task_chain(index: int, task_text: str) -> dict[str, Any]:
        current: dict[str, Any] = {"task": task_text, "steps": []}
        for role in chain:
            result = invoke(role, f"Task {index}/{len(tasks)}\n\n{task_text}", fmt="text")
            current["steps"].append({"role": role, "output": result.stdout.strip()})
        verifier = invoke("verifier", f"Verify completion for task {index}:\n{task_text}", fmt="json")
        current["verifier"] = verifier.result or {"raw": verifier.stdout}
        return current

    attempts = max(1, args.max_task_retries)
    for index, task in enumerate(tasks, start=1):
        task_text = str(task)
        last_error: OrchestrationError | None = None
        for attempt in range(1, attempts + 1):
            try:
                task_summaries.append(run_task_chain(index, task_text))
                last_error = None
                break
            except OrchestrationError as exc:
                last_error = exc
                run_log.log_call(
                    parent_chain=parent_chain,
                    child=f"task-{index}",
                    duration_s=0.0,
                    result=CallResult(status="failure", error=str(exc)),
                    extras={"task_attempt": attempt, "max_task_attempts": attempts},
                )
                if args.verbose:
                    print(
                        f"retrying task {index} after failure ({attempt}/{attempts})",
                        file=sys.stderr,
                    )
        if last_error is not None:
            raise last_error

    append_reply(task_file, "result", {"confirmed": ["A"], "artifact": "spec-dev completed", "notes": "done"})
    return {
        "tasks_executed": len(tasks),
        "task_summaries": task_summaries,
        "run_dir": str(run_log.run_dir),
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Run the spec-dev orchestration lifecycle.")
    parser.add_argument("--spec-file", type=Path, help="Path to spec markdown/text file")
    parser.add_argument("--spec-text", help="Raw spec text")
    parser.add_argument("--dry-run", action="store_true", help="Print prompts but skip cursor-agent invocation")
    parser.add_argument("--workdir", type=Path, default=Path.cwd(), help="Working directory for tasks and logs")
    parser.add_argument(
        "--output-format",
        choices=("text", "json", "stream-json"),
        default="text",
        help="CLI output format for orchestration result",
    )
    parser.add_argument("--verbose", action="store_true", help="Enable verbose errors and retry logging")
    parser.add_argument("--max-retries", type=int, default=DEFAULT_MAX_RETRIES)
    parser.add_argument("--base-backoff-s", type=float, default=0.2)
    parser.add_argument("--max-backoff-s", type=float, default=1.5)
    parser.add_argument("--max-consecutive-failures", type=int, default=DEFAULT_MAX_CONSECUTIVE_FAILURES)
    parser.add_argument(
        "--max-task-retries",
        type=int,
        default=MAX_TASK_RETRIES,
        help="Times to replay a task's development chain on failure",
    )
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    try:
        result = _execute(args)
        if args.output_format == "json":
            print(json.dumps(result))
        elif args.output_format == "stream-json":
            print(json.dumps({"event": "orchestration.completed", "result": result}))
        else:
            print(f"Completed {result['tasks_executed']} tasks")
        return 0
    except OrchestrationError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        if args.verbose:
            raise
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
