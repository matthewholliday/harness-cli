#!/usr/bin/env bash
# Build the harness CLI from source and (re)install it into ~/.cargo/bin.
#
# Usage: scripts/reinstall-harness.sh
#
# Idempotent: `cargo install --force` rebuilds in release mode and overwrites
# any existing `harness` binary on the cargo path.
set -euo pipefail

# Resolve the repo root from this script's location, so it works from anywhere.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HARNESS_DIR="$REPO_ROOT/harness"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found on PATH — install Rust (https://rustup.rs)" >&2
  exit 1
fi

if [ ! -f "$HARNESS_DIR/Cargo.toml" ]; then
  echo "error: no Cargo.toml at $HARNESS_DIR" >&2
  exit 1
fi

echo "Building and installing harness from $HARNESS_DIR ..."
cargo install --path "$HARNESS_DIR" --locked --force

echo
echo "Installed: $(command -v harness)"
harness --version
