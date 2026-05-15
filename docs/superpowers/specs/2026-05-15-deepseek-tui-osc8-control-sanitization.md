# DeepSeek-TUI OSC 8 Control Sanitization Parity

## Context

DeepSeek-TUI commit `e12b4f1` keeps wrapped OSC 8 hyperlinks whole in its
markdown renderer, preventing hyperlink escape sequences from being split while
text wraps. DeepSeekCode's current ratatui panels are plain-text surfaces, not a
styled markdown renderer, so the safer equivalent is to remove unsupported
terminal control wrappers before width wrapping while keeping the visible label
text.

## Scope

- Strip OSC, CSI, DCS, and related terminal escape wrappers from TUI display
  text before clipping or wrapping.
- Preserve visible text inside OSC 8 hyperlinks.
- Prevent raw escape sequences and hidden OSC 8 URLs from entering transcript,
  MCP detail, or manager render buffers.
- Keep CJK/display-width wrapping behavior from the previous slice.
- Document the behavior in TUI and parity docs.

## Acceptance

1. OSC 8 link wrappers are removed while the visible link label remains.
2. Wrapped TUI text contains no raw ESC byte or hidden hyperlink URL from the
   OSC target.
3. Clipping still respects terminal display width after sanitization.
4. Transcript rendering does not leak OSC 8 escape bytes into the test backend.
5. Focused TUI tests and full library checks pass.

## Verification

- `/home/willamhou/.cargo/bin/cargo test sanitize_tui_text --lib`
- `/home/willamhou/.cargo/bin/cargo test terminal_controls --lib`
- `/home/willamhou/.cargo/bin/cargo test render_transcript_strips_osc8_link_wrappers --lib`
- `/home/willamhou/.cargo/bin/cargo test tui --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo test --lib -- --test-threads=1`
