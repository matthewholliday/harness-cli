#!/usr/bin/env bash
set -euo pipefail

ORCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_REPO="${1:-$(pwd)}"
AGENTS_DIR="$TARGET_REPO/.cursor/agents"

if ! command -v cursor-agent >/dev/null 2>&1; then
  echo "ERROR: cursor-agent is not installed or not on PATH." >&2
  exit 1
fi

python3 - <<'PY'
import sys
if sys.version_info < (3, 10):
    raise SystemExit("ERROR: Python 3.10+ is required.")
print("Python version check passed.")
PY

mkdir -p "$AGENTS_DIR"

for agent_file in "$ORCH_DIR"/agents/*.md; do
  cp "$agent_file" "$AGENTS_DIR/"
done

echo "No third-party Python dependencies required."
echo "Next step:"
echo "  cd \"$ORCH_DIR\" && python run_orchestration.py --spec-file <path-to-spec>"
