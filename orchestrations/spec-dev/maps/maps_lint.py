#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path


REQUIRED_HANDOFF_KEYS = ("goal", "done_when", "assumptions", "escalate_if")
ALLOWED_REPLY_TYPES = {"result", "escalation", "handoff", "aborted"}

HEADER_RE = re.compile(r"^## Reply\s+\d+\s+[—-]\s+([a-z]+)\s*$", re.MULTILINE)
KEY_RE_TEMPLATE = r"(?m)^{}\s*:"
LIST_RE_TEMPLATE = r"(?m)^{}\s*:\s*\[([^\]]*)\]\s*$"


@dataclass(frozen=True)
class ReplySection:
    reply_type: str
    body: str
    index: int


def extract_frontmatter(text: str) -> tuple[str, str]:
    if not text.startswith("---\n"):
        raise ValueError("Missing YAML frontmatter start delimiter")

    end = text.find("\n---\n", 4)
    if end == -1:
        raise ValueError("Missing YAML frontmatter end delimiter")

    frontmatter = text[4:end]
    body = text[end + 5 :]
    return frontmatter, body


def has_key(block: str, key: str) -> bool:
    return re.search(KEY_RE_TEMPLATE.format(re.escape(key)), block) is not None


def parse_id_list(block: str, key: str) -> list[str]:
    match = re.search(LIST_RE_TEMPLATE.format(re.escape(key)), block)
    if not match:
        return []
    raw = match.group(1).strip()
    if not raw:
        return []
    parts = [p.strip() for p in raw.split(",")]
    cleaned: list[str] = []
    for part in parts:
        token = part.strip("'\"")
        if token:
            cleaned.append(token)
    return cleaned


def extract_assumption_ids_from_block(block: str) -> set[str]:
    ids: set[str] = set()
    assumptions_match = re.search(
        r"(?ms)^assumptions\s*:\s*\n(?P<body>(?:^[ \t]+.+\n?)*)",
        block,
    )
    if not assumptions_match:
        return ids

    for line in assumptions_match.group("body").splitlines():
        m = re.match(r"^[ \t]+([A-Za-z0-9_.-]+)\s*:", line)
        if m:
            ids.add(m.group(1))
    return ids


def parse_replies(body: str) -> list[ReplySection]:
    matches = list(HEADER_RE.finditer(body))
    replies: list[ReplySection] = []
    for i, match in enumerate(matches):
        start = match.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(body)
        replies.append(
            ReplySection(
                reply_type=match.group(1),
                body=body[start:end].strip(),
                index=i + 1,
            )
        )
    return replies


def lint_maps_file(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    errors: list[str] = []

    try:
        frontmatter, body = extract_frontmatter(text)
    except ValueError as exc:
        return [str(exc)]

    for key in REQUIRED_HANDOFF_KEYS:
        if not has_key(frontmatter, key):
            errors.append(f"handoff missing required key: {key}")

    current_assumption_ids = extract_assumption_ids_from_block(frontmatter)
    if not current_assumption_ids:
        errors.append("handoff assumptions must include at least one ID")

    replies = parse_replies(body)
    terminal_seen = False

    for reply in replies:
        reply_type = reply.reply_type

        if reply_type not in ALLOWED_REPLY_TYPES:
            errors.append(
                f"reply {reply.index} has invalid type '{reply_type}' "
                f"(allowed: {sorted(ALLOWED_REPLY_TYPES)})"
            )
            continue

        if terminal_seen:
            errors.append(
                f"reply {reply.index} appears after terminal reply state"
            )

        if reply_type == "result":
            if not has_key(reply.body, "confirmed"):
                errors.append(f"reply {reply.index} result missing key: confirmed")
            if not has_key(reply.body, "artifact"):
                errors.append(f"reply {reply.index} result missing key: artifact")
            confirmed = parse_id_list(reply.body, "confirmed")
            if set(confirmed) != current_assumption_ids:
                errors.append(
                    f"reply {reply.index} confirmed IDs must match current assumptions "
                    f"{sorted(current_assumption_ids)}"
                )
            terminal_seen = True

        elif reply_type == "escalation":
            if not has_key(reply.body, "violated"):
                errors.append(f"reply {reply.index} escalation missing key: violated")
            if not has_key(reply.body, "observed"):
                errors.append(f"reply {reply.index} escalation missing key: observed")
            if not has_key(reply.body, "disposition"):
                errors.append(f"reply {reply.index} escalation missing key: disposition")
            violated = parse_id_list(reply.body, "violated")
            if not violated:
                errors.append(
                    f"reply {reply.index} escalation violated must be non-empty"
                )
            unknown = sorted(set(violated) - current_assumption_ids)
            if unknown:
                errors.append(
                    f"reply {reply.index} escalation references unknown IDs: {unknown}"
                )

        elif reply_type == "handoff":
            disallowed = []
            for key in re.findall(r"(?m)^([A-Za-z0-9_.-]+)\s*:", reply.body):
                if key not in {"goal", "done_when", "assumptions", "escalate_if", "limits", "context"}:
                    disallowed.append(key)
            if disallowed:
                errors.append(
                    f"reply {reply.index} handoff contains unsupported keys: {sorted(set(disallowed))}"
                )
            updated_ids = extract_assumption_ids_from_block(reply.body)
            if updated_ids:
                current_assumption_ids = updated_ids

        elif reply_type == "aborted":
            if not has_key(reply.body, "reason"):
                errors.append(f"reply {reply.index} aborted missing key: reason")
            terminal_seen = True

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate a MAPS task file for required protocol invariants."
    )
    parser.add_argument("task_file", help="Path to MAPS task markdown file")
    args = parser.parse_args()

    path = Path(args.task_file)
    if not path.exists():
        print(f"ERROR: file not found: {path}", file=sys.stderr)
        return 2

    errors = lint_maps_file(path)
    if errors:
        print(f"MAPS lint failed for {path}:")
        for error in errors:
            print(f"- {error}")
        return 1

    print(f"MAPS lint passed: {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
