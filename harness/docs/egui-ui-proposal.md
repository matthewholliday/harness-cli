# Proposal: An egui GUI for the Harness CLI

**Status:** Implemented (phases 1–5; see "Implementation status" below)
**Author:** (proposed)
**Date:** 2026-06-28

## 1. Summary

Harness is today a synchronous Rust CLI for running coding agents as a Ralph
loop with deterministic validation gates. It ships a read-only Ratatui dashboard
(`harness watch`) that polls `.harness/logs/` every 400ms and renders a five-zone
terminal view.

This proposal adds an optional **egui desktop GUI** — `harness-gui` — that mirrors
the existing dashboard and then goes beyond what a terminal can express:
real-time agent log streaming, interactive run controls, phase timelines, and
spec/task inspection. The GUI reuses the existing on-disk state model and the
existing crate's data types, so it adds a presentation layer without changing the
core loop.

## 2. Motivation

The Ratatui `watch` view (`harness/src/tui.rs`) is good but bounded by the
terminal:

- **Read-only.** It can't start/stop a run, retry a blocked task, or open a spec
  file. Users drop back to the shell for every action.
- **Fixed character cells.** Phase progression, hook timing, and the iteration
  history are hard to visualize as charts or timelines.
- **One view at a time.** No side-by-side spec text + task list + live agent
  output.
- **No live agent stdout.** The TUI shows iteration *records* after the fact; it
  never streams what the agent is doing right now.

An egui app keeps Harness's "state lives on disk, UI just reads it" philosophy
while removing these limits. egui is pure-Rust, immediate-mode (a natural fit for
the existing snapshot-every-tick pattern), and cross-platform.

## 3. Goals / Non-Goals

**Goals**
- Visual parity with `harness watch` (status, stats, task list, detail, progress log).
- Launch and monitor `harness build` from the GUI, with live stdout/stderr streaming.
- Interactive controls: start/stop a run, target a spec, `--once`/`--max`/`--dry-run` toggles.
- Browse specs (requirements / design / tasks) and per-task detail.
- Phase-aware visualization (timeline / swimlane per task).
- Ship as a separate, optional binary — zero impact on the CLI's footprint.

**Non-Goals (v1)**
- No editing of spec files from the GUI (read-only browsing first; editing is a later phase).
- No replacement of the CLI or the Ratatui TUI — both stay.
- No web/remote UI — desktop-native only.
- No re-architecting the core loop. The GUI drives it via the existing CLI surface.

## 4. Architecture

### 4.1 Reuse the existing state model

Everything the GUI renders already lives on disk and is already modeled in the
crate:

- `state.rs` — `LoopState`, `IterationRecord`, `HookResult`
- `spec.rs` — `Task`, `TaskStatus`, `Requirement`, `RequirementsFile`, `load_tasks`, `list_specs`
- `manifest.rs` — drift detection
- `config.rs` — agent/loop/phase/hooks config

The `tui.rs` `Snapshot` (tui.rs:85) is the perfect blueprint: an immutable
read of state + tasks + iteration records + progress tail, with a "live"
heuristic. We extract that snapshot logic into a shared, UI-agnostic module so
both the Ratatui and egui front-ends consume it.

### 4.2 Proposed crate layout

Promote `harness` to a small workspace so the GUI is a separate binary that
depends on the core as a library:

```
harness/
├─ Cargo.toml              # [workspace]
├─ crates/
│  ├─ harness-core/        # library: config, state, spec, manifest,
│  │                       #   hooks, loop_runner, prompt, util, snapshot
│  ├─ harness-cli/         # existing binary: clap + tui.rs (Ratatui)
│  └─ harness-gui/         # new binary: eframe/egui
```

This is the only structural change to existing code: move the modules into
`harness-core` and have `harness-cli` depend on it. The CLI's behavior is
unchanged. (A lighter alternative — keep one crate and gate the GUI behind a
`gui` Cargo feature — is viable but workspace split keeps the GUI's heavy
dependency tree out of the CLI build entirely. **Recommendation: workspace.**)

### 4.3 Driving the loop

The GUI does **not** call `loop_runner::run()` in-process (that function is
blocking and writes to the terminal). Instead it follows the same model a user
does: spawn `harness build [args]` as a child process via `std::process::Command`,
capture stdout/stderr on a background thread, and feed lines into the UI through
an `mpsc` channel. The egui repaint loop drains the channel each frame.

This keeps the GUI a thin, decoupled client: the loop's correctness, exit codes,
and on-disk state remain the single source of truth. State panels refresh by
re-reading the snapshot on a timer (matching the TUI's 400ms cadence), exactly as
`watch` does today.

```
┌── harness-gui (egui/eframe) ──────────────┐
│  repaint tick → Snapshot::load(root)      │  reads .harness/logs/*
│  repaint tick → drain log channel         │
│  controls     → spawn `harness build ...` ─┼─► child process
└───────────────────────────────────────────┘     │ stdout/stderr
            ▲                                        │
            └──────── mpsc channel ◄── reader thread ┘
```

### 4.4 Why no async runtime is needed

Harness is fully synchronous today. egui/eframe runs its own event loop; child
process I/O is handled on a plain `std::thread` with a channel. No tokio
required — consistent with the current dependency philosophy.

## 5. UI Design

### 5.1 Main dashboard (parity + extension)

A three-pane layout (egui `SidePanel` + `CentralPanel` + `TopBottomPanel`):

- **Top bar:** status pill (RUNNING / IDLE / COMPLETE / STOPPED), active spec,
  iteration/budget, elapsed, current phase — same data as the TUI header
  (tui.rs:326). Plus **Start / Stop** buttons and run-option toggles.
- **Left panel:** task list with status glyphs, attempts, and phase-progress
  bars. Selecting a task drives the detail pane.
- **Central panel (tabbed):**
  - *Detail* — selected task meta, latest iteration record, hook results with
    timing, git sha, last failure note (parity with the TUI detail pane).
  - *Live agent output* — streamed stdout/stderr of the running `harness build`
    (new capability).
  - *Spec* — rendered requirements / design / tasks for the active spec.
  - *Phases* — per-task timeline/swimlane of completed vs pending phases.
- **Bottom panel:** colorized progress.md tail (green DONE, red BLOCKED, yellow retry).

### 5.2 Visualizations that the terminal can't do well

- **Stats** as a real progress bar + small donut of done/active/todo/blocked.
- **Iteration history** as a sparkline/timeline (hook pass/fail per iteration).
- **Phase swimlanes** showing each task's progression through `phase_sequence`.

## 6. Dependencies

Add to `harness-gui` only:

```toml
eframe = "0.29"   # egui + windowing + glow renderer
egui = "0.29"
egui_extras = "0.29"   # tables, optional charts
```

`harness-cli` keeps its current dependency set (ratatui, crossterm, clap, …)
untouched. Pin egui to the version line current at implementation time.

## 7. Implementation plan (phased)

1. **Workspace refactor.** Extract `harness-core`; point `harness-cli` at it.
   No behavior change; existing tests must pass. *(Mechanical, low risk.)*
2. **Shared snapshot.** Lift `Snapshot` logic out of `tui.rs` into
   `harness-core::snapshot`, consumed by the existing TUI to prove the
   abstraction.
3. **GUI skeleton.** `harness-gui` with eframe; render the read-only dashboard
   from the shared snapshot (parity with `watch`).
4. **Run controls.** Spawn/monitor `harness build`, stream output via channel,
   Start/Stop + option toggles.
5. **Spec & phase views.** Spec browsing tab and phase timeline visualization.
6. **Polish.** Theming, persisted window state, keyboard shortcuts mirroring the
   TUI (`j/k`, `g/G`, `r`, `q`).

Each phase is independently shippable; after phase 3 the GUI is already a usable
read-only dashboard.

## 8. Risks & mitigations

| Risk | Mitigation |
|------|------------|
| egui dependency tree bloats build | Separate binary; CLI build unaffected. |
| GUI and loop disagree on state | GUI never owns state — disk is the single source of truth; it only reads + shells out. |
| Workspace refactor regresses CLI | Phase 1 is mechanical and gated on the existing test suite (tui.rs:747 tests, etc.). |
| Cross-platform windowing issues | eframe/glow is well-supported on macOS/Linux/Windows; document the GL requirement. |
| Maintaining two front-ends | Shared `harness-core::snapshot` keeps the data layer single-sourced; only rendering diverges. |

## 9. Open questions

- Workspace split vs. `gui` Cargo feature flag? (Proposal recommends workspace.)
- Should the GUI eventually allow editing specs, or stay read-only and defer to `$EDITOR`/`harness spec edit`?
- Is live agent stdout streaming acceptable given agents can be verbose, or should it be opt-in/tail-limited?
- Distribution: build `harness-gui` in CI artifacts, or leave it `cargo install`-only initially?

## Implementation status

Implemented as a three-crate Cargo workspace:

- **`harness-core`** — the modules formerly under `harness/src/` (config, hooks,
  loop_runner, manifest, prompt, spec, state, util) plus a new
  `snapshot` module holding the UI-agnostic read model (`Snapshot`, `Counts`)
  lifted out of `tui.rs`.
- **`harness-cli`** — the existing `harness` binary; now depends on
  `harness-core` and its Ratatui `tui.rs` renders from `harness_core::snapshot`.
  All existing tests pass unchanged.
- **`harness-gui`** — the new `eframe`/`egui` binary. `runner.rs` spawns and
  streams `harness build`; `main.rs` renders the header (status, stats bar, Start/
  Stop + run-option controls), task list, progress log, and a tabbed central pane
  (Detail / Live output / Spec / Phases).

Decisions taken against §9's open questions: workspace split (not a feature
flag); GUI is read-only for specs in v1 (no editing); live stdout is streamed to
the Live-output tab. CLI build is unaffected by egui — `cargo build -p
harness-cli` pulls none of the GUI dependency tree.

## 10. Recommendation

Proceed with the workspace refactor (phase 1) and a read-only egui dashboard
(phases 2–3) as a first milestone. This delivers visible value quickly, proves
the shared-snapshot abstraction, and de-risks the larger interactive features —
all without touching the correctness-critical loop.
