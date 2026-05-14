# DeepSeek-TUI TUI Theme Persistence

Status: implemented

## Gap

DeepSeekCode had DeepSeek-TUI-style `/theme` switching, but the selected theme
only lived inside the current TUI process. Restarting the local TUI returned to
the default theme.

## Implementation

- Added a local TUI preference file at `.dscode/tui/theme.json`.
- Loaded the preference during local TUI startup.
- Persisted the selected theme after `theme`, `/theme`,
  `config theme`, or `/config theme` switches.
- Updated the theme detail panel to show the persistence path when enabled.
- Kept HTTP-runtime TUI sessions read-only for this preference, matching other
  local file-backed TUI state.

## Verification

- `cargo test theme_preferences_persist_across_tui_instances --lib`
- `cargo test theme --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

No known theme persistence blocker remains for DeepSeek-TUI parity.
