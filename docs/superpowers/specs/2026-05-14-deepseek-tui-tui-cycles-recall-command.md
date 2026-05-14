# DeepSeek-TUI Parity: TUI Cycles and Recall Commands

## Context

DeepSeek-TUI exposes `/cycles`, `/cycle <n>`, and `/recall <query>` for context
handoff visibility and archive search. DeepSeekCode does not have the same live
cycle manager, but it does persist compaction summaries and already exposes the
DeepSeek-TUI-compatible `recall_archive` tool to the agent.

## Goals

- Add `cycles` / `/cycles` command-palette and composer support.
- Add `cycle <n>` / `/cycle <n>` command-palette and composer support.
- Add `recall <query>` / `/recall <query>` command-palette and composer
  support.
- Treat active-thread summary items as DeepSeekCode cycle handoffs.
- Render a compact list of durable handoffs and a detailed view for one handoff.
- Run `recall_archive` locally from the TUI and render its JSON summary in the
  right-side detail panel.
- Include help, slash completions, docs, and unit coverage.

## Design

`TuiApp` parses cycle commands into `TuiCycleCommand`. `/cycles` and `/cycle <n>`
are TUI-local because the app snapshot already includes active-thread summary
items. Cycle numbering is one-based and ordered by item index.

`/recall <query>` queues a local file-backed action. The CLI action handler uses
`RecallArchiveTool` with the selected workspace config and optional selected
thread id, then renders the tool's deterministic JSON output under a Recalls
detail panel.

This is intentionally a DeepSeekCode-native equivalent rather than a direct
copy of DeepSeek-TUI's in-memory cycle briefings: durable compaction summaries
are the persisted archive boundary in this runtime.

## Acceptance

- `/cycles` shows active-thread summary handoffs or a clear empty state.
- `/cycle <n>` shows the full summary for a one-based handoff number.
- `/cycle` and invalid numbers show concise usage/errors.
- `/recall <query>` queues a local action, rejects empty queries, and renders
  `recall_archive` output.
- Help and completion surfaces include `/cycles`, `/cycle`, and `/recall`.
- Tests cover list/detail/invalid cycle commands and recall action handling.
