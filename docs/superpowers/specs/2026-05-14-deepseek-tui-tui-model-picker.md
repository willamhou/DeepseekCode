# DeepSeek-TUI Parity: TUI Model Picker

## Context

DeepSeekCode already supports `/model <name>`, `/model show`, `models`, and
`/models` for local file-backed TUI sessions. That closes direct config
mutation, but the bare `/model` command still required a read-only detail panel
instead of an interactive model selection surface.

## Goals

- Make `model` / `/model` open an interactive model picker from the TUI command
  palette and composer slash command.
- Keep `model show` / `/model show` for the existing read-only config detail.
- Keep `models` / `/models` and `model list` / `/model list` for the offline
  model catalog detail.
- Render local model choices with labels, full model ids, hints, and a selected
  workspace action preview.
- Use keyboard navigation: up/down, enter to apply, escape to close, plus
  home/end for edge jumps.
- Queue the same `TuiAction::Model::Set` action used by direct model commands,
  preserving existing local config mutation behavior.

## Acceptance

- `model` opens the picker and does not immediately mutate config.
- `/model show` still queues a model config detail action.
- `/models` still queues a model catalog action.
- Entering the picker on a selected model queues `TuiAction::Model::Set`.
- The picker renders model rows and a selected workspace action preview.
- Existing direct model updates and `/config model ...` routing remain
  unchanged.
- Full `tui` tests continue passing.

## Remaining

This slice intentionally keeps `/models` as an offline catalog. Online provider
model discovery remains a separate runtime/API-backed gap because it needs a
provider-specific fetch contract, credentials handling, cache policy, and
network permission behavior.
