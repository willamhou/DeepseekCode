# DeepSeek-TUI Parity: TUI Edit Command

## Context

DeepSeek-TUI exposes `/edit` to load the latest user message back into the
composer so the user can revise and resubmit it. DeepSeekCode already keeps
durable thread items in the TUI, but it did not expose a compatible command for
this composer workflow.

## Goals

- Add `edit` / `/edit` command-palette and composer support.
- Add `edit help` / `/edit help` detail rendering.
- Find the latest selected-thread user message from durable TUI items.
- Load that message into the composer, focus the composer, and place the cursor
  at the end.
- Keep the operation local to the TUI: no model turn, no durable runtime
  mutation, and no rollback action.
- Reject unsupported arguments with a concise usage error.

## Design

`TuiApp` parses `edit` / `/edit` into `TuiEditCommand`. The command reuses the
same latest selected user message lookup used by `/system` task previews. When
invoked from the composer, `/edit` replaces the slash command with the loaded
message instead of clearing the composer. This matches the immediate
DeepSeek-TUI editing affordance while leaving durable conversation rewriting for
a separate `/undo` or `/retry` slice.

## Acceptance

- `edit` / `/edit` loads the latest selected user message into the composer.
- The composer remains focused and the cursor moves to the end of the loaded
  message.
- `edit help` / `/edit help` renders usage details.
- When no previous user message exists, the status says
  `no previous message to edit`.
- Invalid arguments show
  `usage: edit or /edit; use edit help for details`.
- Tests cover composer invocation, no-action behavior, help rendering, invalid
  arguments, and empty-history handling.
