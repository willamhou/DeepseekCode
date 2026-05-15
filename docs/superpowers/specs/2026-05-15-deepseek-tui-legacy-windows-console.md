# DeepSeek-TUI Legacy Windows Console Parity

## Context

DeepSeek-TUI commit `b834548` calms rendering for plain Windows PowerShell or
cmd.exe sessions hosted by legacy ConHost. DeepSeekCode does not have animated
chrome or synchronized-output wrapping, but it did always enable crossterm
mouse capture for every interactive TUI session.

## Scope

- Detect unmarked legacy Windows console hosts when no modern terminal markers
  are present.
- Keep mouse capture enabled for non-Windows hosts and modern Windows terminal
  hosts.
- Skip mouse capture on legacy Windows ConHost while still entering the
  alternate screen and preserving keyboard navigation.
- Document the compatibility behavior.

## Acceptance

1. Non-Windows hosts keep mouse capture enabled by default.
2. Windows hosts with no modern terminal marker disable mouse capture.
3. Windows hosts with markers such as `WT_SESSION`, `TERM_PROGRAM`, `ANSICON`,
   or WezTerm/Alacritty/ConEmu markers keep mouse capture enabled.
4. Focused TUI tests and formatting pass.

## Verification

- `/home/willamhou/.cargo/bin/cargo test legacy_windows_console --lib`
- `/home/willamhou/.cargo/bin/cargo test terminal_restore_guard_tracks_active_state_until_disarmed --lib`
- `/home/willamhou/.cargo/bin/cargo test tui --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo test --lib -- --test-threads=1`
