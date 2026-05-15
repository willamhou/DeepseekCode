# DeepSeek-TUI CJK Wrap Parity

## Context

DeepSeek-TUI commit `f7eb17b` hard-wraps CJK/no-whitespace runs in diff and
pager surfaces so long terminal-width text does not overflow or make scroll
math disagree with the rendered view. DeepSeekCode already clipped short TUI
previews by terminal display width, but transcript, detail, and MCP manager
panels still relied on widget wrapping for long no-whitespace text.

## Scope

- Add a shared display-width wrapper for TUI text surfaces.
- Hard-wrap overlong CJK/no-whitespace runs by terminal display width.
- Prefer whitespace boundaries for ordinary English text.
- Use wrapped line counts for scroll bounds in transcript, MCP detail, and MCP
  manager views.
- Document the behavior in the TUI and parity docs.

## Acceptance

1. Long CJK text splits into multiple lines without any wrapped line exceeding
   the target terminal display width.
2. Existing newline boundaries are preserved while CJK/no-whitespace runs are
   still split.
3. Ordinary English text prefers whitespace boundaries when wrapping.
4. Focused TUI tests and full library checks pass.

## Verification

- `/home/willamhou/.cargo/bin/cargo test wrap_ --lib`
- `/home/willamhou/.cargo/bin/cargo test handle_tui_action_lists_and_shows_skills --lib`
- `/home/willamhou/.cargo/bin/cargo test tui --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo test --lib -- --test-threads=1`
