You are authoring a harness spec. Your job is to write exactly three files to
`.specs/{spec_name}/` and then stop. Do not implement any code. Do not modify
anything outside of `.specs/{spec_name}/`.

---

## Brief

{brief}

---

## Files to write

Write all three files below. Derive content from the brief; fill any gaps with
sensible defaults, marking anything you assumed with a `# TODO` comment.

---

### `.specs/{spec_name}/1-requirements.json`

EARS requirements. Rules:
- Every requirement uses `"shall"` in its `text` field.
- `type` must be one of: `ubiquitous`, `event`, `state`, `unwanted`, `optional`, `complex`.
- Each type has corresponding fields (`trigger` for event, `precondition` for
  state, `condition` for unwanted, `feature` for optional); set unused fields to `null`.
- Every requirement must have at least one entry in `acceptance_criteria` — these
  become the agent's done criteria.
- `priority` is one of: `must`, `should`, `could`, `wont`.
- `status` is one of: `draft`, `approved`, `deprecated`. Use `"draft"` unless
  the brief is explicit.
- IDs are `REQ-001`, `REQ-002`, … — stable and unique within the spec.

EARS templates for reference:

| type | text template |
|---|---|
| `ubiquitous` | The `<system>` shall `<response>`. |
| `event` | When `<trigger>`, the `<system>` shall `<response>`. |
| `state` | While `<precondition>`, the `<system>` shall `<response>`. |
| `unwanted` | If `<condition>`, then the `<system>` shall `<response>`. |
| `optional` | Where `<feature>`, the `<system>` shall `<response>`. |

Write the JSON without comments (they are invalid JSON); the schema is:

```json
{
  "spec": "{spec_name}",
  "version": "1",
  "introduction": "<one paragraph framing the feature and its scope>",
  "glossary": { "<term>": "<definition>" },
  "requirements": [
    {
      "id": "REQ-001",
      "type": "<type>",
      "system": "<system name>",
      "trigger": "<event clause or null>",
      "precondition": "<state clause or null>",
      "condition": "<unwanted clause or null>",
      "feature": "<optional clause or null>",
      "response": "<the shall clause>",
      "text": "<rendered sentence using the template above>",
      "rationale": "<why this requirement exists>",
      "priority": "<must|should|could|wont>",
      "acceptance_criteria": ["<concrete, testable assertion>"],
      "tags": [],
      "derived_from": [],
      "status": "draft"
    }
  ]
}
```

---

### `.specs/{spec_name}/2-design.md`

Technical design. Required headings (validation checks for these exactly):

```
## Context
## Architecture & Components
## Data Model
## Interfaces & APIs
## Flows
## Decisions
## Risks & Open Questions
## Requirement Coverage
```

The frontmatter must list every REQ-### id that this design covers:

```markdown
---
spec: {spec_name}
version: 1
status: draft
covers: [REQ-001, REQ-002]
---
# Design: {spec_name}

## Context
...

## Requirement Coverage
REQ-001 -> <component or section that satisfies it>
```

Keep it brief and concrete. Sketch the shape; the agent implementing the tasks
will read this as context each iteration.

---

### `.specs/{spec_name}/3-tasks.jsonl`

One JSON object per line. Rules:
- Tasks are the unit the loop runs one at a time; keep each focused and
  completable in a single agent session.
- `id` is `T-001`, `T-002`, … — stable and unique within the spec.
- `priority`: lower number runs sooner. Start at 1.
- `depends_on`: task ids that must be `done` before this task starts; use
  sparingly and only when truly sequential.
- `hooks`: the blocking hooks that must pass for this task to count as done.
  Omit the field to use the project default. Common values: `["run_build",
  "run_lint", "run_unit_tests"]`.
- `acceptance`: 1–4 concrete, human-readable done criteria that match the
  requirement's acceptance_criteria.
- `files_hint`: paths or globs the agent is likely to touch — helps the agent
  scope its work; not enforced.
- `max_attempts`: 3 is a safe default.
- `status` must be `"todo"` for every task.
- Timestamps: use `"2026-01-01T00:00:00Z"` as a placeholder.

Schema (one object, then repeat per task — no trailing comma, one per line):

```
{"id":"T-001","spec":"{spec_name}","title":"<imperative verb phrase>","requirements":["REQ-001"],"status":"todo","priority":1,"depends_on":[],"hooks":["run_build","run_lint","run_unit_tests"],"acceptance":["<assertion>"],"files_hint":["<path>"],"attempts":0,"max_attempts":3,"notes":"","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}
```

---

## After writing the files

Run:

```sh
harness spec validate {spec_name}
```

If validation fails, fix the issues and re-validate before stopping. The most
common failures are: missing acceptance criteria, unknown REQ-### references in
tasks, missing design headings, and malformed JSONL (trailing commas, extra
braces).

When validation passes, print a short summary:
- Number of requirements and their priorities
- Number of tasks and the dependency order they will run in
- Any TODOs you left for the user to fill in
- The command to start the loop: `harness run --spec {spec_name}`
