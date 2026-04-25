# Cursor Hooks

Source: https://cursor.com/docs/hooks

Cursor Hooks let you observe, control, and extend Cursor's agent loop with custom scripts or prompt-based checks. Hooks are spawned processes that exchange JSON over stdin/stdout. They can audit activity, block risky actions, modify supported inputs, inject context, run formatters, or continue workflows after an agent or subagent finishes.

## When To Use Hooks

Use hooks when you need behavior that should run automatically around Cursor activity:

- Run formatters or linters after agent or Tab edits.
- Gate risky shell, MCP, or subagent operations.
- Scan prompts, files, commands, or outputs for secrets and policy violations.
- Add audit logs and usage analytics.
- Inject session context or environment variables at conversation start.
- Continue an agent or subagent loop with a generated follow-up message.

Agent hooks apply to Cmd+K and Agent Chat. Tab hooks apply to inline completions and have their own events, so policies for autonomous completions can differ from policies for user-directed agent work.

## Where Hooks Live

Hooks are configured in `hooks.json` files. Multiple hook sources can be active at the same time; matching hooks from every source run. When returned fields conflict, higher-priority sources win in this order: Enterprise, Team, Project, User.

Project hooks are best when the behavior should be shared with a repository:

```json
{
  "version": 1,
  "hooks": {
    "afterFileEdit": [{ "command": ".cursor/hooks/format.sh" }]
  }
}
```

Place that file at `.cursor/hooks.json`, place scripts under `.cursor/hooks/`, and use paths relative to the project root, such as `.cursor/hooks/format.sh`.

User hooks are best for personal/global behavior:

```json
{
  "version": 1,
  "hooks": {
    "afterFileEdit": [{ "command": "./hooks/format.sh" }]
  }
}
```

Place that file at `~/.cursor/hooks.json`, place scripts under `~/.cursor/hooks/`, and use paths relative to `~/.cursor/`, such as `./hooks/format.sh`.

After creating a command hook script, make it executable:

```sh
chmod +x .cursor/hooks/format.sh
```

Cursor watches hook config files and reloads them on save. If a valid hook does not load, restart Cursor.

## Hook Definition Fields

Each entry under `hooks` is an array of hook definitions. Common fields are:

- `command`: Shell command or script path. Required for command hooks.
- `type`: `"command"` or `"prompt"`. Defaults to `"command"`.
- `prompt`: Prompt text for prompt hooks.
- `timeout`: Timeout in seconds.
- `matcher`: JavaScript-style regular expression that narrows when the hook runs.
- `failClosed`: If `true`, crashes, timeouts, and invalid JSON block the action instead of allowing it through.
- `loop_limit`: Maximum auto-follow-up loops for `stop` and `subagentStop`; defaults to `5` for Cursor hooks.

Command hooks receive JSON on stdin and return JSON on stdout. Exit code `0` means success, exit code `2` blocks the action like `permission: "deny"`, and other non-zero exits fail open unless `failClosed` is set.

Prompt hooks use a fast model to evaluate a natural-language policy. They are useful for lightweight checks, but command hooks are better when behavior must be deterministic, auditable, or dependent on exact parsing.

## Choosing An Event

Use the narrowest event that matches the goal:

- `sessionStart` / `sessionEnd`: Set up or audit a conversation session.
- `preToolUse` / `postToolUse` / `postToolUseFailure`: Observe or control generic tool calls.
- `subagentStart` / `subagentStop`: Gate subagents or chain follow-up work after they finish.
- `beforeShellExecution` / `afterShellExecution`: Gate or audit terminal commands.
- `beforeMCPExecution` / `afterMCPExecution`: Gate or audit MCP tool calls.
- `beforeReadFile` / `afterFileEdit`: Control file reads or process agent file edits.
- `beforeSubmitPrompt`: Validate a user prompt before it is submitted.
- `preCompact`: Observe context compaction.
- `stop`: Run when the agent loop ends and optionally auto-submit a follow-up.
- `afterAgentResponse` / `afterAgentThought`: Observe agent responses or reasoning blocks.
- `beforeTabFileRead` / `afterTabFileEdit`: Control or post-process Cursor Tab reads and edits.

For shell-only gating, prefer `beforeShellExecution` over generic `preToolUse`. For rewriting a tool call input, use `preToolUse`. For adding context after a tool succeeds, use `postToolUse`.

## Matchers

Matchers prevent hooks from running on every event. They use JavaScript-style regular expressions, not POSIX regex syntax, so use `\s` rather than `[[:space:]]`.

Matcher targets depend on the event:

- `preToolUse`, `postToolUse`, and `postToolUseFailure` match tool names such as `Shell`, `Read`, `Write`, `Delete`, `Task`, or `MCP: ...`.
- `subagentStart` and `subagentStop` match subagent type, such as `generalPurpose`, `explore`, or `shell`.
- `beforeShellExecution` and `afterShellExecution` match the full shell command string.
- `beforeReadFile` matches file-read tool types such as `Read` or `TabRead`.
- `afterFileEdit` matches edit tool types such as `Write` or `TabWrite`.
- `beforeSubmitPrompt`, `stop`, `afterAgentResponse`, and `afterAgentThought` match fixed event names.

If a hook is not firing, remove or simplify the matcher first. Once the base hook is confirmed to load and run, tighten the matcher.

## Common Inputs And Outputs

All hook input includes shared metadata such as `conversation_id`, `generation_id`, `model`, `hook_event_name`, `cursor_version`, `workspace_roots`, `user_email`, and `transcript_path`.

Important event-specific outputs:

- `preToolUse`: Return `permission`, `user_message`, `agent_message`, and optionally `updated_input`.
- `postToolUse`: Return `additional_context`; for MCP tools, it can also return `updated_mcp_tool_output`.
- `subagentStart`: Return `permission` and optional `user_message`.
- `subagentStop`: Return `followup_message`; it is consumed only when the subagent completed successfully.
- `beforeShellExecution` / `beforeMCPExecution`: Return `permission`, `user_message`, and `agent_message`; `permission` can be `"allow"`, `"deny"`, or `"ask"`.
- `beforeReadFile` / `beforeTabFileRead`: Return `permission` and, for agent reads, optional `user_message`.
- `beforeSubmitPrompt`: Return `continue` and optional `user_message`.
- `stop`: Return `followup_message` to auto-submit another user message, subject to `loop_limit`.
- `sessionStart`: Return `env` and/or `additional_context`; current callers do not block session creation.

Return only fields supported by the event. Unsupported fields may be ignored.

## Minimal Command Hook

This project-level hook asks before shell commands that look like network access:

```json
{
  "version": 1,
  "hooks": {
    "beforeShellExecution": [
      {
        "command": ".cursor/hooks/approve-network.sh",
        "matcher": "curl|wget|nc ",
        "failClosed": true
      }
    ]
  }
}
```

```bash
#!/bin/bash
input=$(cat)
command=$(printf '%s' "$input" | jq -r '.command // empty')

if [[ "$command" =~ curl|wget|nc ]]; then
  cat <<'JSON'
{
  "permission": "ask",
  "user_message": "This command may make a network request. Please review it before continuing.",
  "agent_message": "A hook flagged this shell command as a possible network call."
}
JSON
  exit 0
fi

echo '{ "permission": "allow" }'
```

Check that required tools such as `jq`, `python3`, `node`, or `bun` are installed in the hook environment before relying on them.

## Environment Variables

Hook scripts receive:

- `CURSOR_PROJECT_DIR`: Workspace root directory.
- `CURSOR_VERSION`: Cursor version.
- `CURSOR_USER_EMAIL`: Authenticated user email, if available.
- `CURSOR_TRANSCRIPT_PATH`: Conversation transcript path, if transcripts are enabled.
- `CURSOR_CODE_REMOTE`: `"true"` in remote workspaces.
- `CLAUDE_PROJECT_DIR`: Compatibility alias for the project directory.

Environment variables returned by `sessionStart` are passed to later hooks in the same session.

## Troubleshooting

Use the Hooks tab in Cursor Settings to inspect configured and executed hooks. Use the Hooks output channel to see errors.

If a hook fails to work:

- Confirm `hooks.json` is valid JSON and uses `"version": 1`.
- Confirm the script path is relative to the right base directory.
- Confirm command hook scripts have executable permissions and a valid shebang.
- Confirm external tools are installed and visible on `PATH`.
- Remove matchers until the hook fires, then re-add a simple matcher.
- Set `failClosed: true` only when blocking on hook failure is intended.
- Use exit code `2` or `permission: "deny"` for intentional blocking.

