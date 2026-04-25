# Cursor Headless CLI

Primary source: https://cursor.com/docs/cli/headless

Related docs:

- https://cursor.com/docs/cli/using (non-interactive / print mode)
- https://cursor.com/docs/cli/installation
- https://cursor.com/docs/cli/reference/authentication
- https://cursor.com/docs/cli/reference/output-format

The Cursor Agent CLI (`agent`) supports **headless** use in scripts and CI: you run it with **print mode** (`-p` / `--print`) so it does not require an interactive TTY. Combine that with flags for **file writes**, **output shape**, and **authentication** depending on whether you need analysis-only or automated edits.

## Core idea: print mode

Use `-p` / `--print` for non-interactive runs. Print mode is also inferred when stdout is not a TTY or stdin is piped.

- **Without `--force`**: the agent can propose changes but **does not apply** file modifications in print mode.
- **With `--force`** (or **`--yolo`**): the agent may **write files directly** without confirmation—use only in trusted automation.

```bash
# Analysis / answer only (no guaranteed writes)
agent -p "What does this codebase do?"

# Allow the agent to modify files in print mode
agent -p --force "Refactor this code to use modern ES6+ syntax"
```

## Setup

### Install

- macOS, Linux, WSL: `curl https://cursor.com/install -fsS | bash`
- Windows (native PowerShell): `irm 'https://cursor.com/install?win32=true' | iex`

Verify: `agent --version`

Add `~/.local/bin` to `PATH` if the installer places `agent` there (see installation doc). Updates: `agent update` (CLI may auto-update by default).

### Authentication

**Browser login (recommended for local use):**

```bash
agent login
agent status
agent logout
```

**API key (automation / CI):**

1. Create a key in [Cursor Dashboard → Cloud Agents](https://cursor.com/dashboard/cloud-agents) (User API Keys).
2. Export `CURSOR_API_KEY` or pass `--api-key`.

```bash
export CURSOR_API_KEY=your_api_key_here
agent -p "Analyze this code"
```

`agent status` shows auth state and endpoint configuration. For dev-only TLS issues the docs mention `--insecure`; for custom endpoints, `--endpoint`.

## Output formats (`--output-format`)

`--output-format` applies with `--print` (or when print mode is inferred). Default is **`text`**: only the **final** assistant message—good for simple scripts.

| Format | Use case |
|--------|----------|
| `text` | Final answer only; simplest for shell pipelines |
| `json` | One JSON object at end of a successful run; parse with `jq` |
| `stream-json` | NDJSON events for progress, tool calls, and final result |

On **failure**, the process exits non-zero; stderr gets an error message. **`json`** does not emit a well-formed result object on failure. **`stream-json`** may end without a terminal `result` line.

### `json` success shape (reference)

Successful `json` output is a single object ending with a newline, for example:

- `type`: `"result"`
- `subtype`: `"success"`
- `is_error`: `false`
- `duration_ms`, `duration_api_ms`
- `result`: full assistant text
- `session_id`, optional `request_id`

See https://cursor.com/docs/cli/reference/output-format for the canonical schema.

### `stream-json` and real-time text

- **`--output-format stream-json`**: one NDJSON line per event; ends with a **`result`** event on success.
- **`--stream-partial-output`** (with `stream-json`): character-level style streaming. Assistant lines split into three cases; for **new text** use events where **`timestamp_ms` is present** and **`model_call_id` is absent**. Skip buffered flushes before tool calls (`model_call_id` present) and the final end-of-turn flush (no `timestamp_ms`).

If you only need the finished answer in stream mode, ignore intermediate `assistant` lines and read the terminal **`result`** event.

## Images and binary paths

Put **paths in the prompt** (relative to cwd or absolute). The agent reads files via tool calls, including images and other media.

```bash
agent -p "Analyze this image: ./screenshot.png"
agent -p --output-format json "Describe: $IMAGE_PATH" | jq -r '.result'
```

Ensure files exist and are readable from the process cwd.

## Patterns from the headless doc

**Batch file edits** (trusted repo only):

```bash
find src/ -name "*.js" | while read -r file; do
  agent -p --force "Add JSDoc comments to $file"
done
```

**Structured review** (write review to a file via prompt + `--force` if the agent should write):

```bash
agent -p --force --output-format text \
  "Review recent changes; write findings to review.txt"
```

**Stream progress** (parse NDJSON with `jq`; see headless doc for a full `while read` example).

## Implementation notes (output-format reference)

- NDJSON: one JSON object per line, newline-terminated.
- `thinking` events are **suppressed** in print mode.
- Unknown fields may appear over time; parsers should ignore extras.
- Correlate tool start/complete with `call_id` in stream events.

## Troubleshooting

- **Not authenticated**: `agent login` or set `CURSOR_API_KEY` / `--api-key`.
- **No file changes**: add `--force` (or `--yolo`) in print mode when writes are intended.
- **Wrong PATH**: ensure `agent` is on `PATH` (often `~/.local/bin`).
- **Parse errors on failure**: handle non-zero exit and stderr; do not assume trailing JSON on failure for `json` / partial streams.
