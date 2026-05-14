# DeepSeek-TUI Parity: TUI Config Command Routing

## Context

DeepSeek-TUI's `/config` command supports editor-mode shortcuts and
`/config <key> [value]` access for common runtime settings. DeepSeekCode
currently treats `/config` as an alias for `/settings`, so `/config model`,
`/config provider`, `/config mode`, and similar command forms stop at usage
errors instead of routing to existing focused configuration commands.

## Goals

- Route `/config` and `config` through a dedicated parser before the generic
  settings display.
- Preserve bare `/config`, `/config show`, and `/settings` as configuration
  overview displays.
- Support `/config tui`, `/config native`, and `/config web` as explicit editor
  mode requests that surface the current DeepSeekCode config surface.
- Route common key commands to existing behavior:
  - `/config model [pick|show|list|<name>]`
  - `/config provider [pick|show|list|<name> [model]]`
  - `/config profile [list|clear|<name>]`
  - `/config mode [agent|plan|yolo|1|2|3]`
  - `/config theme [dark|light|grayscale|system]`
  - `/config verbose [on|off|toggle|show]`
  - `/config translate [on|off|toggle|show]`
- Update completions, help, and TUI docs.

## Acceptance

- `/config model auto` queues the same `TuiAction::Model` as `/model auto`.
- `/config provider list` queues the same provider catalog action as
  `/provider list`.
- `/config mode plan`, `/config theme light`, `/config verbose on`, and
  `/config translate off` reuse existing local TUI handlers.
- `/config tui` renders a config detail panel instead of returning a usage
  error.
- Existing `/settings` behavior remains unchanged.
- Full `tui` tests continue passing.
