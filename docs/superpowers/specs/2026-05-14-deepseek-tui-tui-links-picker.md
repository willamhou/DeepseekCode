# DeepSeek-TUI Parity: TUI Links Picker

## Context

DeepSeekCode already supports `/links`, `/dashboard`, and `/api` as
terminal-native detail views. The remaining UX gap was the bare `/links`
command: it rendered a static detail panel instead of an interactive target
picker.

## Goals

- Make `links` / `/links` open an interactive link picker from the command
  palette and composer slash command.
- Keep `links show` / `/links show` for the existing overview of all
  repository, release, docs, platform, and API links.
- Keep direct `dashboard` / `/dashboard` and `api` / `/api` aliases for focused
  DeepSeek platform and API details.
- Add focused `links repo|issues|releases|docs|dashboard|api` detail commands.
- Render link choices with labels, URLs, hints, and an action preview.
- Use keyboard navigation: up/down, enter to open, escape to close, plus
  home/end for edge jumps.

## Acceptance

- `links` opens the picker and does not immediately render a detail panel.
- `/links show` still renders the full links overview.
- Entering the picker on a selected target renders the same focused detail as
  the matching direct command.
- Composer `/links` opens the picker and clears the submitted slash command.
- Full `tui` tests continue passing.

## Remaining

DeepSeekCode intentionally keeps links terminal-native: it shows URLs in the TUI
instead of launching a GUI browser.
