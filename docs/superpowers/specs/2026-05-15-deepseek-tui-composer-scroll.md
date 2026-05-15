# DeepSeek-TUI Composer Scroll Parity

## Context

DeepSeek-TUI commit `7c8c71e` fixes terminals, especially Windows console
hosts, where mouse wheel input can arrive as arrow keys while the composer has
focus. DeepSeekCode always enables mouse capture in the TUI, but composer focus
previously swallowed transcript scroll keys such as `PageUp` / `PageDown` and
left Windows arrow-as-scroll behavior unavailable.

## Scope

- Keep transcript scrollback reachable with `PageUp` / `PageDown` while the
  composer is focused.
- Default composer `Up` / `Down` to transcript scrolling on Windows.
- Preserve non-Windows composer `Up` / `Down` behavior.
- Document the focused-composer scroll keys.

## Acceptance

1. Focused composer `PageUp` scrolls transcript history without mutating draft
   text.
2. Focused composer `PageDown` returns toward the latest transcript content.
3. Windows default arrow-scroll detection can be tested independently of the
   host platform.
4. Focused TUI tests and formatting pass.

## Verification

- `/home/willamhou/.cargo/bin/cargo test composer_page_keys_scroll_transcript_while_focused --lib`
- `/home/willamhou/.cargo/bin/cargo test composer_arrows_scroll --lib`
- `/home/willamhou/.cargo/bin/cargo test composer_arrow_scroll --lib`
- `/home/willamhou/.cargo/bin/cargo test tui --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo test --lib -- --test-threads=1`
