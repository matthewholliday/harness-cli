from __future__ import annotations

import json
import tempfile
import threading
import unittest
from pathlib import Path

from logging_system import CallResult, RunLog


class TestRunLog(unittest.TestCase):
    def test_create_unique_run_dirs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first = RunLog.create(root)
            second = RunLog.create(root)
            self.assertNotEqual(first.run_dir, second.run_dir)
            self.assertTrue(first.run_dir.exists())
            self.assertTrue(second.run_dir.exists())

    def test_required_json_shape_and_optional_extras_omitted(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = RunLog.create(Path(tmp))
            run.log_call(
                parent_chain=["orchestration", "planner"],
                child="executor",
                duration_s=1.23,
                result=CallResult(status="success"),
                extras=None,
            )
            log_file = run.run_dir / "orchestration" / "planner" / "executor" / "call.log"
            payload = json.loads(log_file.read_text(encoding="utf-8").strip())
            self.assertIn("ts", payload)
            self.assertEqual(payload["parent"], "planner")
            self.assertEqual(payload["child"], "executor")
            self.assertIsInstance(payload["duration_s"], float)
            self.assertEqual(payload["result"]["status"], "success")
            self.assertNotIn("extras", payload)

    def test_concurrent_writes_preserve_line_integrity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = RunLog.create(Path(tmp))
            target = run.run_dir / "orchestration" / "planner" / "executor" / "call.log"

            def worker(i: int) -> None:
                run.log_call(
                    parent_chain=["orchestration", "planner"],
                    child="executor",
                    duration_s=float(i),
                    result=CallResult(status="success"),
                    extras={"worker": i},
                )

            threads = [threading.Thread(target=worker, args=(i,)) for i in range(32)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()

            lines = target.read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(lines), 32)
            seen_workers = set()
            for line in lines:
                parsed = json.loads(line)
                self.assertIn("ts", parsed)
                self.assertEqual(parsed["parent"], "planner")
                self.assertEqual(parsed["child"], "executor")
                self.assertIn("result", parsed)
                seen_workers.add(parsed["extras"]["worker"])
            self.assertEqual(len(seen_workers), 32)


if __name__ == "__main__":
    unittest.main()
