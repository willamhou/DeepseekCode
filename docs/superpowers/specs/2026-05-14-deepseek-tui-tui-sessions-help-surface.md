# DeepSeek-TUI Parity: TUI Sessions and Help Surface

## Context

The post-translate source audit showed the built-in command behavior was present
for most DeepSeek-TUI command names, but `/sessions` and `/resume` could still
fall through to project custom slash commands from the focused composer. The
help index also omitted several already-implemented commands (`help`, `save`,
`load`, and `attach`), which made simple source audits undercount the built-in
surface.

## Goals

- Add built-in `sessions` / `/sessions`, `session` / `/session`, and
  `resume` / `/resume` handling before custom slash fallback.
- Support `sessions filter <query>` / `/sessions filter <query>` and clearing
  the filter with `sessions filter`.
- Add help index entries for `help`, `sessions`, `save`, `load`, and `attach`.
- Add slash completions and docs for the slash session picker entry points.

## Acceptance

- `/sessions` from the composer opens the session picker and does not queue a
  custom slash command.
- `/resume` is accepted as an alias.
- `/sessions filter <query>` updates the visible session picker filter.
- Help output includes the implemented help/save/load/attach/session surfaces.
