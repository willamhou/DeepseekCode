# DeepSeek-TUI Parity: TUI Legacy Command Migrations

## Context

A dispatch-level audit after `/sessions prune` found two DeepSeek-TUI command
literals that are intentionally not in the command registry or completion list:
`/set` and `/deepseek`. DeepSeek-TUI catches them before unknown/custom fallback
and returns migration guidance. DeepSeekCode currently lets those names fall
through to project custom slash command handling.

## Goals

- Catch `set` / `/set` from the focused composer and command palette before
  custom slash fallback.
- Catch `deepseek` / `/deepseek` from the focused composer and command palette
  before custom slash fallback.
- Keep both commands out of help and completion surfaces, matching
  DeepSeek-TUI's hidden migration behavior.
- Document the parity slice in the running plan.

## Acceptance

- `/set model x` surfaces the retired-command guidance and queues no custom
  slash action.
- `/deepseek` surfaces the renamed-command guidance and queues no custom slash
  action.
- The full TUI test suite continues passing.
