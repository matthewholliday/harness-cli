# ACLC v0.1 Conformance — harness

This harness implements the **Agentic Coding Loop Configuration (ACLC) v0.1**
control surface at the **Full** conformance level (§9). ACLC governs *one task's
attempt loop*; the harness's outer multi-task scheduler and SDLC phase machinery
are application extensions outside ACLC's scope (§1.1, §10).

## Where it lives

| Concern | Location |
|---|---|
| Config model, validator, presets, JSON Schema | `crates/harness-core/src/aclc.rs` |
| `LEARNINGS.md` memory (§7.2–7.4) | `crates/harness-core/src/memory.rs` |
| Protected oracle + partial score (§8.2/§8.4) | `crates/harness-core/src/oracle.rs` |
| Lifecycle, workspace, exhaustion, reflection | `crates/harness-core/src/loop_runner.rs` |
| Config table + legacy reconciliation + writer | `crates/harness-core/src/config.rs` |
| CLI surface (`harness aclc …`, doctor) | `crates/harness-cli/src/main.rs` |
| GUI settings panel | `crates/harness-gui/src/main.rs` |

## Configuration

ACLC is the `[aclc]` table in `.harness/harness.toml`:

```toml
[aclc]
loop = "until_pass"      # off | until_pass
workspace = "continue"   # fresh | continue
memory = "compact"       # off | replace | append | compact
memory_cap = 8
learning = "reflection"  # raw | reflection
max_attempts = 12
on_exhaustion = "keep_best"  # keep_best | keep_last | clean

[aclc.oracle]
command = "harness eval run demo"
protected = true
```

When the `[aclc]` table is absent the loop keeps its historical behaviour:
`workspace` derives from the legacy `reset_on_failure`, the attempt cap from
`[budgets].max_attempts_per_task`, and the loop stays single-pass at the ACLC
layer (`resolve_aclc`).

## Clause-by-clause

- **§3 model / §3.1 defaults** — `AclcConfig` with the eight fields and the exact
  defaults (`loop=off`, `max_attempts=10`, `workspace=continue`, `memory=off`,
  `memory_cap=8`, `learning=reflection`, `oracle.protected=true`,
  `on_exhaustion=keep_best`). Inert fields are ignored by the engine and rendered
  disabled in the GUI.
- **§4 lifecycle** — `loop_runner::run`: prepare workspace → load memory → run
  agent → evaluate oracle → on pass return (memory untouched) → on fail derive
  learning + update memory + record attempt → on exhaustion apply policy. Memory
  is read before and written only after a failed attempt; an attempt never sees
  its own not-yet-written entry.
- **§5.1 hard constraints / §6** — `aclc::validate` returns `{severity, fields,
  message}` records; the run refuses to start when any `error` is present
  (`until_pass` without oracle, `max_attempts < 1`, `memory_cap < 1`).
- **§5.2 inert guard / §5.3 discouraged combos** — all six warning rows are
  emitted (append-no-cap, raw-under-accumulation, unprotected-oracle-with-memory,
  clean+fresh, fresh+memory-off, plus inert-field warnings). Warnings never block.
- **§7.1 presets** — `single_pass`, `resample`, `refine`, `refine_notes`,
  `resample_notes` with normative semantics; `resample` aliases legacy "Ralph
  Loop". `harness aclc preset <name>` and the GUI preset selector apply them.
- **§7.2 memory modes** — `off`/`replace`/`append`/`compact` in `memory::update`.
- **§7.3 learning** — `raw` (verbatim failure signal, truncated) and `reflection`
  (an agent pass over the failure producing one actionable claim;
  `.harness/prompts/reflect.md` or a built-in default).
- **§7.4 compaction** — append-then-reconcile to ≤ `memory_cap`, deterministic in
  count, dedup-by-normalized-text keeping the latest. (A richer agent
  reconciliation pass is an allowed upgrade.)
- **§8.1 exhaustion** — `keep_best` / `keep_last` / `clean` in
  `apply_on_exhaustion`.
- **§8.2 ranking** — each failed attempt's tree is snapshotted (`git stash
  create`) with its oracle partial score; `keep_best` restores the highest score,
  ties broken by recency. Score parsed from `ACLC_SCORE=<frac>` / `<n>/<m>` lines.
- **§8.3 workspace isolation** — resets restore tracked files to the baseline
  commit and delete only agent-created untracked files, preserving the user's
  pre-existing untracked work (`git_restore_to_head`). **Known deviation:** resets
  operate on the project's working tree in place rather than a separate worktree;
  a fully isolated per-attempt worktree is the recommended follow-up.
- **§8.4 protected oracle** — the default oracle is the spec's eval suite under
  `evals/`, which the write guards already keep outside the agent's writable
  surface; `protected = true` therefore needs no extra enforcement.
- **§9 Full** — all memory modes including `compact`, both learning modes,
  compaction, the protected oracle, and the §5.2–5.3 warning set are implemented.

## Application-defined choices (§10)

- Snapshot/isolation mechanism: `git stash create` snapshots + in-place restore.
- Attempt scoring beyond §8.2: oracle `ACLC_SCORE=` line, else binary pass/fail.
- Reflection and compaction prompts: `.harness/prompts/reflect.md` (built-in
  default provided); compaction is deterministic recency+dedup.

## CLI

```
harness aclc show         # resolved config + matching preset
harness aclc validate     # findings; exit 1 on any error
harness aclc preset <name> [--oracle "<cmd>"]
harness aclc schema       # write .harness/schema/aclc-0.1.schema.json
harness doctor            # includes aclc validity + warnings
```
