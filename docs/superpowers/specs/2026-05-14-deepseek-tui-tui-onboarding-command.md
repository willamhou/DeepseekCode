# DeepSeek-TUI TUI Onboarding Command

**Status:** implemented on 2026-05-14
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

DeepSeek-TUI exposes more of the first-run journey inside the terminal UI.
DeepSeekCode had CLI-level onboarding via `doctor`, but a user already inside
the TUI had to leave the workbench to inspect project config and model
credential readiness.

This slice adds a TUI-native checklist while keeping the command read-only and
non-interactive.

## Implementation

- Add `setup`, `onboarding`, and `doctor` command aliases in the TUI command
  palette and composer slash flow.
- Render a right-side `Setup` detail panel for the selected workspace.
- Show project config presence, effective model, base URL, API key env var
  presence, and live-model readiness.
- Parse model config from `.dscode/config.toml` without creating or modifying
  files, including active profile overrides.
- Show next commands such as `deepseek config init`, `export <ENV>=...`,
  `deepseek doctor`, `deepseek smoke`, `/provider pick`, `/model pick`, and
  `/trust list`.
- Add help, command completion, slash completion, and `docs/tui.md` coverage.

## Verification

- `cargo test setup_command_renders_onboarding_checklist --lib`
- `cargo test composer_slash_hints_include_deepseek_tui_palette_backed_commands --lib`
- `cargo fmt --check`
- `git diff --check`

## Residual Gap

This is still a checklist/detail panel, not a full wizard. A DeepSeek-TUI-style
guided first-run flow with inline language, API key, trust, and theme steps
remains future product polish.
