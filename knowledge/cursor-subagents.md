# Cursor Subagents

Source: https://cursor.com/docs/subagents

Subagents are specialized assistants the main Cursor Agent can delegate to. Each subagent runs in its own context window, handles a focused kind of work, and returns a result to the parent. They are available in the editor, CLI, and [Cloud Agents](https://cursor.com/docs/cloud-agent).

## Why Subagents Exist

- **Context isolation** — Long research or exploration does not consume the main conversation’s context.
- **Parallel execution** — Multiple subagents can run at once on different parts of a problem.
- **Specialized expertise** — Custom prompts, tool access, and models can be tuned per role.
- **Reusability** — Custom definitions can be shared across projects (project vs user locations).

When Agent hits a complex task, it may launch a subagent automatically. The subagent gets a self-contained prompt with needed context; it does **not** see prior parent chat history.

## Foreground vs Background

| Mode | Behavior | Best for |
| --- | --- | --- |
| Foreground | Blocks until the subagent finishes; result returns immediately. | Sequential work that needs the output before continuing. |
| Background | Returns right away; subagent works independently. | Long jobs or parallel streams. |

Set `is_background: true` in custom subagent frontmatter for background mode (see configuration below).

## Built-In Subagents

Cursor ships three built-in subagents for noisy, context-heavy work. You do not configure them; Agent uses them when appropriate.

| Subagent | Role | Why it is isolated |
| --- | --- | --- |
| **Explore** | Search and analyze codebases | Large intermediate output; often uses a faster model and parallel searches. |
| **Bash** | Run shell command sequences | Verbose logs stay out of the parent’s context. |
| **Browser** | Drive the browser via MCP | DOM snapshots and screenshots are filtered to useful summaries. |

Shared benefits: isolated intermediate output, model flexibility (e.g. fast model for explore), specialized prompts/tools, and often better cost efficiency for token-heavy steps.

## Subagents vs Skills

| Prefer **subagents** when… | Prefer **skills** when… |
| --- | --- |
| You need a separate context for long research | The task is single-purpose (changelog, format imports) |
| You want parallel workstreams | You want a quick, repeatable one-shot action |
| The work spans many steps with specialized expertise | You do not need a new context window |
| You want independent verification | A skill file is enough |

Simple, single-purpose automation (“generate a changelog”) is usually better as a [skill](https://cursor.com/docs/skills) than a custom subagent.

## Custom Subagent Locations

| Type | Paths | Scope |
| --- | --- | --- |
| Project | `.cursor/agents/`, `.claude/agents/`, `.codex/agents/` | Current project |
| User | `~/.cursor/agents/`, `~/.claude/agents/`, `~/.codex/agents/` | All projects for the user |

If names conflict, **project overrides user**, and **`.cursor/` overrides `.claude/` / `.codex/`** for the same name.

## File Format

Each subagent is a Markdown file with YAML frontmatter:

```markdown
---
name: security-auditor
description: Security specialist. Use when implementing auth, payments, or handling sensitive data.
model: inherit
readonly: true
---

You are a security expert auditing code for vulnerabilities.
...
```

### Frontmatter Fields

| Field | Required | Default | Description |
| --- | --- | --- | --- |
| `name` | No | From filename | Identifier; lowercase and hyphens recommended. |
| `description` | No | — | Shown in Task tool hints; Agent uses this to decide delegation. |
| `model` | No | `inherit` | `inherit`, `fast`, or a specific model ID (see below). |
| `readonly` | No | `false` | If `true`, restricted writes (no file edits / state-changing shell). |
| `is_background` | No | `false` | If `true`, runs in background without blocking the parent. |

### Model Field

| Value | Behavior |
| --- | --- |
| `inherit` | Same model as the parent (default). |
| `fast` | Smaller, faster model; good for search, verification, high-volume steps. |
| Specific ID | Exact model (see [models and pricing](https://cursor.com/docs/models-and-pricing)). |

Cursor may **not** honor the configured model if: a team admin blocked it, the model needs [Max Mode](https://cursor.com/help/ai-features/max-mode) and it is off, or the model is not on your plan — then Cursor falls back to a compatible model. On some legacy request-based plans without Max Mode, subagents may run on Composer regardless of `model`; team policy can further restrict this.

## Using Subagents

**Automatic delegation** — Agent chooses based on complexity, your custom `description` fields, context, and tools. Phrases like “use proactively” or “always use for” in `description` encourage routing.

**Explicit invocation** — Slash-style or natural language:

```text
/verifier confirm the auth flow is complete
Use the verifier subagent to confirm the auth flow is complete
```

**Parallel work** — Ask for two streams in one message (e.g. “review API changes and update docs in parallel”); Agent can issue multiple Task calls so subagents run concurrently.

## Resuming Subagents

Each run has an **agent ID**. Resume with full preserved context:

```text
Resume agent abc123 and analyze the remaining test failures
```

Background subagents persist state as they run; you can resume after completion to continue the same thread.

## Common Patterns

**Verifier** — Skeptical second pass: confirm claimed work exists, run tests, report pass vs incomplete. Often `model: fast`.

**Orchestrator** — Parent sequences specialists (e.g. planner → implementer → verifier) with structured handoffs.

Example templates for `debugger` and `test-runner` subagents appear in the official docs; mirror those with focused `description` lines so delegation stays predictable.

## Best Practices

- One clear responsibility per subagent; avoid generic “coding helper” agents.
- Invest in **`description`** — it drives when Agent delegates; test with real prompts.
- Keep the **body prompt** concise and specific.
- Commit **`.cursor/agents/`** so the team shares the same specialists.
- Use **Agent to draft** the first version, then tighten manually.
- Use **hooks** if you need consistent post-processing of subagent output to files.

**Anti-patterns:** dozens of vague agents, descriptions like “general tasks,” multi-thousand-word prompts, duplicating what a slash command or skill would do better, starting with more than a few agents before you have distinct use cases.

## Managing Subagents

- **Create:** Ask Agent to scaffold ` .cursor/agents/<name>.md`, or add Markdown files manually under project or user paths above.
- **Inspect:** List files under `.cursor/agents/` (and user dir) to see what is configured; custom subagents appear in Agent’s available tools.

## Performance And Cost

| Benefit | Trade-off |
| --- | --- |
| Context isolation | Startup cost (subagent gathers its own context) |
| Parallel runs | Higher total tokens (multiple contexts) |
| Specialized focus | Extra latency vs parent for trivial tasks |

Parallel subagents each bill/use tokens in their own window — roughly N× for N parallel agents. For quick, simple work, the main agent is often cheaper/faster; subagents pay off for heavy exploration, parallel lanes, or isolation from noisy tool output.

## FAQ (Short)

- **Built-ins:** `explore`, `bash`, `browser` — automatic, no project files required.
- **Nested subagents:** Yes (since Cursor 2.5); subject to Task tool access and hook/policy limits.
- **Background progress:** Output under `~/.cursor/subagents/`; parent can read files for status.
- **Failures:** Subagent returns error status; parent can retry, resume, or change strategy.
- **MCP in subagents:** Yes — subagents inherit parent tools, including MCP servers.
- **Misbehavior:** Tighten `description` and body prompt; test with a small explicit `/name` task.

For the authoritative, always-up-to-date detail (including full example frontmatter blocks), use the [official Subagents documentation](https://cursor.com/docs/subagents).
