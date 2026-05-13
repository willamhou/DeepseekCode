# DeepSeek-TUI Parity: TUI Anchor Command

## Context

DeepSeek-TUI exposes `/anchor` for pinning critical workspace facts that should survive context churn and compaction. DeepSeekCode had durable memory and notes, but no TUI-level workspace anchor file.

## Goals

- Add `anchor` / `/anchor` command palette and composer support.
- Support adding anchors with `anchor <text>` and `anchor add <text>`.
- Support listing anchors with `anchor list`.
- Support removing anchors with `anchor remove <n>`, plus `rm` / `delete` aliases.
- Show the workspace anchor file path with `anchor path`.
- Store anchors under the selected session workspace in `.dscode/anchors.md`.

## Design

Anchor files mirror the existing note-file separator format:

```text
---
Pinned fact
---
Another pinned fact
```

The command queues a local `TuiAction::Anchor { workspace, command }`, so the selected TUI session workspace determines where anchors are stored. Remote HTTP runtime TUI rejects anchor commands as local-only because the command mutates workspace files.

This slice intentionally adds the TUI management surface first. Agent-loop prompt injection for anchors remains a separate compaction-context wiring step if later review shows the residual gap is material.

## Acceptance

- `anchor <text>` and `/anchor <text>` append to `.dscode/anchors.md`.
- `anchor list` renders current anchors in the right-side detail panel.
- `anchor remove <n>` removes a 1-based anchor.
- `anchor path` renders the workspace anchor path.
- Command palette and focused composer both route anchor commands before custom slash fallback.
- Tests cover command routing and local file-backed add/list/remove/path behavior.
