# DeepSeek-TUI Parity: TUI Goal Command

## Context

DeepSeek-TUI exposes `/goal` for setting a session objective and optional token budget. DeepSeekCode already has durable tasks and telemetry, but the TUI lacked this lightweight session-level objective display.

## Goals

- Add `goal` / `/goal` command palette and composer support.
- Support showing the current goal when no argument is provided.
- Support setting a goal with optional `budget: N` token budget syntax.
- Support `goal clear`, `goal reset`, and `goal done`.
- Render goal details in the right-side TUI detail panel.

## Design

The goal is stored in `TuiApp` memory, matching DeepSeek-TUI's session-local behavior. It is not persisted to runtime records and does not mutate user files.

The detail panel shows:

- objective
- elapsed time since it was set
- optional token budget
- active-thread cumulative token usage and percentage when usage telemetry exists
- command reminders for show, replace, and clear

## Acceptance

- `goal <objective> [budget: N]` and `/goal <objective> [budget: N]` set the TUI session goal.
- `goal` and `/goal` show the current goal or an empty-state prompt.
- `goal clear`, `goal reset`, `goal done`, and slash equivalents clear the goal.
- Existing active-thread usage summaries are used for token budget progress.
- Tests cover setting, showing, budget rendering, and composer-based clearing.
