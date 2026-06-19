from __future__ import annotations

import hashlib
import json
import os
import re
import threading
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal

Status = Literal["success", "failure", "cancelled", "unknown"]

_MAX_SEGMENT_LEN = 64
_NAME_RE = re.compile(r"[^A-Za-z0-9._-]+")
_UNDERSCORE_RUN_RE = re.compile(r"_+")


@dataclass(frozen=True)
class CallResult:
    status: Status
    error: str | None = None


class RunLog:
    def __init__(self, run_dir: Path) -> None:
        self._run_dir = run_dir
        self._path_locks: dict[Path, threading.Lock] = {}
        self._path_locks_guard = threading.Lock()

    @classmethod
    def create(cls, logs_root: Path | str = "logs") -> "RunLog":
        root = Path(logs_root)
        root.mkdir(parents=True, exist_ok=True)
        timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%d-%H-%M-%S")
        base_name = f"orchestration-{timestamp}"
        candidate = root / base_name
        suffix = 2
        while True:
            try:
                candidate.mkdir()
                return cls(candidate)
            except FileExistsError:
                candidate = root / f"{base_name}-{suffix}"
                suffix += 1

    @property
    def run_dir(self) -> Path:
        return self._run_dir

    def log_call(
        self,
        *,
        parent_chain: list[str],
        child: str,
        duration_s: float,
        result: CallResult,
        extras: dict[str, Any] | None = None,
    ) -> None:
        if not parent_chain:
            raise ValueError("parent_chain must not be empty")
        if not child:
            raise ValueError("child must not be empty")

        sanitized_chain = [self._sanitize_name(name) for name in parent_chain]
        child_segment = self._sanitize_name(child)
        log_path = self._run_dir.joinpath(*sanitized_chain, child_segment, "call.log")
        log_path.parent.mkdir(parents=True, exist_ok=True)

        record: dict[str, Any] = {
            "ts": self._utc_iso_now(),
            "parent": parent_chain[-1],
            "child": child,
            "duration_s": float(duration_s),
            "result": {"status": result.status},
        }
        if result.status != "success" and result.error:
            record["result"]["error"] = result.error[:500]
        if extras:
            record["extras"] = extras

        encoded = (json.dumps(record, separators=(",", ":"), ensure_ascii=True) + "\n").encode(
            "utf-8"
        )
        lock = self._get_lock(log_path)
        with lock:
            fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
            try:
                os.write(fd, encoded)
                os.fsync(fd)
            finally:
                os.close(fd)

    def _get_lock(self, path: Path) -> threading.Lock:
        with self._path_locks_guard:
            lock = self._path_locks.get(path)
            if lock is None:
                lock = threading.Lock()
                self._path_locks[path] = lock
            return lock

    @staticmethod
    def _sanitize_name(name: str) -> str:
        if not name:
            raise ValueError("name must not be empty")
        segment = _NAME_RE.sub("_", name)
        segment = _UNDERSCORE_RUN_RE.sub("_", segment)
        segment = segment.strip("._-")
        if not segment:
            raise ValueError(f"name sanitizes to empty: {name!r}")
        if len(segment) > _MAX_SEGMENT_LEN:
            digest = hashlib.sha1(name.encode("utf-8")).hexdigest()[:8]
            keep = _MAX_SEGMENT_LEN - 9
            segment = f"{segment[:keep]}-{digest}"
        return segment

    @staticmethod
    def _utc_iso_now() -> str:
        now = datetime.now(timezone.utc)
        ms = int(now.microsecond / 1000)
        return now.strftime("%Y-%m-%dT%H:%M:%S.") + f"{ms:03d}Z"
