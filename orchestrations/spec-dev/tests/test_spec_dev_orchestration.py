from __future__ import annotations

import json
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

import run_orchestration
from run_orchestration import AgentResult, OrchestrationError


class _Completed:
    def __init__(self, *, returncode: int, stdout: str, stderr: str = "") -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class TestRunAgent(unittest.TestCase):
    @patch("run_orchestration.subprocess.run")
    def test_argv_construction(self, mock_run) -> None:
        mock_run.return_value = _Completed(returncode=0, stdout="{}", stderr="")
        with patch.dict("os.environ", {"CURSOR_AGENT_BIN": "agent-bin"}):
            run_orchestration.run_agent(
                "prompt",
                agent="planner",
                force=True,
                output_format="json",
                extra_args=["--trust"],
            )
        argv = mock_run.call_args[0][0]
        self.assertEqual(argv[0], "agent-bin")
        self.assertEqual(argv[1:3], ["-p", "prompt"])
        self.assertIn("--output-format", argv)
        self.assertIn("json", argv)
        self.assertIn("--agent", argv)
        self.assertIn("planner", argv)
        self.assertIn("--force", argv)
        self.assertIn("--trust", argv)

    @patch("run_orchestration.subprocess.run")
    def test_malformed_json_raises(self, mock_run) -> None:
        mock_run.return_value = _Completed(returncode=0, stdout="{bad", stderr="")
        with self.assertRaises(OrchestrationError):
            run_orchestration.run_agent("p", output_format="json")


class TestRetryAndMaps(unittest.TestCase):
    def _base_args(self, workdir: Path, *, max_task_retries: int = 5) -> Namespace:
        return Namespace(
            spec_file=None,
            spec_text="spec text",
            dry_run=False,
            workdir=workdir,
            output_format="json",
            verbose=False,
            max_retries=1,
            base_backoff_s=0.0,
            max_backoff_s=0.0,
            max_consecutive_failures=3,
            max_task_retries=max_task_retries,
        )

    def test_retry_failure_then_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = self._base_args(Path(tmp))
            calls = {"n": 0}

            def fake_run_agent(*_args, **kwargs):
                calls["n"] += 1
                if calls["n"] == 1:
                    raise OrchestrationError("transient")
                return AgentResult(stdout="ok", stderr="", returncode=0, duration_ms=1, result={})

            with patch("run_orchestration.run_agent", side_effect=fake_run_agent), patch(
                "run_orchestration.time.sleep"
            ):
                result = run_orchestration._call_with_retry(
                    prompt="p",
                    agent="lifecycle-manager",
                    output_format="text",
                    max_retries=1,
                    base_backoff_s=0.0,
                    max_backoff_s=0.0,
                    dry_run=False,
                    run_log=run_orchestration.RunLog.create(Path(tmp) / "logs"),
                    parent_chain=["orchestration"],
                    verbose=False,
                )
            self.assertEqual(result.stdout, "ok")
            self.assertEqual(calls["n"], 2)

    def test_retry_exhausted(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with patch(
                "run_orchestration.run_agent",
                side_effect=OrchestrationError("always bad"),
            ), patch("run_orchestration.time.sleep"):
                with self.assertRaises(OrchestrationError):
                    run_orchestration._call_with_retry(
                        prompt="p",
                        agent="lifecycle-manager",
                        output_format="text",
                        max_retries=1,
                        base_backoff_s=0.0,
                        max_backoff_s=0.0,
                        dry_run=False,
                        run_log=run_orchestration.RunLog.create(Path(tmp) / "logs"),
                        parent_chain=["orchestration"],
                        verbose=False,
                    )

    def test_sequencing_decomposer_before_work_and_verifier_last(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = self._base_args(Path(tmp))
            order: list[str] = []

            def fake_call(*_a, **kwargs):
                agent = kwargs["agent"]
                order.append(agent)
                if agent == "spec-to-task-decomposer":
                    payload = {"result": {"tasks": ["task-1"]}}
                    return AgentResult("{}", "", 0, 1, payload)
                if agent == "verifier":
                    return AgentResult("{}", "", 0, 1, {"result": {"ok": True}})
                return AgentResult("ok", "", 0, 1, {})

            with patch("run_orchestration._call_with_retry", side_effect=fake_call):
                output = run_orchestration._execute(args)

            self.assertEqual(output["tasks_executed"], 1)
            self.assertEqual(order[0], "lifecycle-manager")
            self.assertEqual(order[1], "spec-to-task-decomposer")
            self.assertEqual(order[-1], "verifier")

    def test_task_chain_retry_then_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = self._base_args(Path(tmp), max_task_retries=3)
            director_calls = {"n": 0}
            architect_calls = {"n": 0}

            def fake_call(*_a, **kwargs):
                agent = kwargs["agent"]
                if agent == "spec-to-task-decomposer":
                    return AgentResult("{}", "", 0, 1, {"result": {"tasks": ["task-1"]}})
                if agent == "software-development-director":
                    director_calls["n"] += 1
                    return AgentResult("ok", "", 0, 1, {})
                if agent == "software-architect":
                    architect_calls["n"] += 1
                    if architect_calls["n"] == 1:
                        raise OrchestrationError("transient architect failure")
                    return AgentResult("ok", "", 0, 1, {})
                if agent == "verifier":
                    return AgentResult("{}", "", 0, 1, {"result": {"ok": True}})
                return AgentResult("ok", "", 0, 1, {})

            with patch("run_orchestration._call_with_retry", side_effect=fake_call):
                output = run_orchestration._execute(args)

            self.assertEqual(output["tasks_executed"], 1)
            self.assertEqual(director_calls["n"], 2)
            self.assertEqual(architect_calls["n"], 2)

    def test_task_chain_retry_exhausted(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = self._base_args(Path(tmp), max_task_retries=2)
            architect_calls = {"n": 0}

            def fake_call(*_a, **kwargs):
                agent = kwargs["agent"]
                if agent == "spec-to-task-decomposer":
                    return AgentResult("{}", "", 0, 1, {"result": {"tasks": ["task-1"]}})
                if agent == "software-architect":
                    architect_calls["n"] += 1
                    raise OrchestrationError("permanent architect failure")
                return AgentResult("ok", "", 0, 1, {})

            with patch("run_orchestration._call_with_retry", side_effect=fake_call):
                with self.assertRaises(OrchestrationError):
                    run_orchestration._execute(args)

            self.assertEqual(architect_calls["n"], 2)

    def test_default_max_task_retries_from_settings(self) -> None:
        import settings

        self.assertEqual(settings.MAX_TASK_RETRIES, 5)
        parsed = run_orchestration.build_parser().parse_args(["--spec-text", "x"])
        self.assertEqual(parsed.max_task_retries, 5)

    def test_maps_fail_closed_unknown_assumption(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            task_file = Path(tmp) / "task.md"
            task_file.write_text(
                "---\n"
                "goal: g\n"
                "done_when: d\n"
                "assumptions:\n"
                "  A: ok\n"
                "escalate_if:\n"
                "  - break\n"
                "---\n",
                encoding="utf-8",
            )
            with self.assertRaises(OrchestrationError):
                run_orchestration.append_reply(
                    task_file,
                    "escalation",
                    {"violated": ["B"], "observed": "x", "proposal": "y", "disposition": "blocked"},
                )
            run_orchestration.append_reply(
                task_file,
                "aborted",
                {"reason": "protocol_validation_failed"},
            )
            self.assertTrue(run_orchestration.is_terminal_state(task_file))


if __name__ == "__main__":
    unittest.main()
