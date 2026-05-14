# DeepSeek-TUI Parity: TUI Feedback Picker

## Context

DeepSeekCode already supports `/feedback bug`, `/feedback feature`, and
`/feedback security` as terminal-native detail views. The remaining UX gap was
the bare `/feedback` command: it rendered a static detail panel instead of an
interactive feedback target picker.

## Goals

- Make `feedback` / `/feedback` open an interactive feedback picker from the
  command palette and composer slash command.
- Keep `feedback show` / `/feedback show` for the existing read-only feedback
  target overview.
- Keep direct `feedback bug`, `feedback feature`, and `feedback security`
  commands for explicit link views.
- Render bug, feature, and security choices with concise hints plus an action
  preview.
- Use keyboard navigation: up/down, enter to open, escape to close, plus
  home/end for edge jumps.

## Acceptance

- `feedback` opens the picker and does not immediately render a detail panel.
- `/feedback show` still renders the feedback overview detail.
- Entering the picker on a selected target renders the same detail as the
  direct feedback subcommand.
- Composer `/feedback` opens the picker and clears the submitted slash command.
- Full `tui` tests continue passing.

## Remaining

DeepSeekCode intentionally keeps feedback terminal-native: it shows links in
the TUI instead of launching a GUI browser or posting feedback automatically.
