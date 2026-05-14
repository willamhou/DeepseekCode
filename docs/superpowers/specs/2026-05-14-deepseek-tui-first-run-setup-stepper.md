# DeepSeek-TUI First-Run Setup Stepper

**Status:** implemented on 2026-05-14
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

Guided `/setup` commands existed as separate entry points. Users still had to
know the intended order: provider, model, auth, trust, theme, and language.

This slice adds a first-run stepper that sequences those controls from one TUI
entry point.

## Implementation

- Add `/setup wizard` plus `setup wizard` in the command palette.
- Render a setup wizard modal with ordered provider, model, auth, trust, theme,
  and language steps.
- Support Enter to open the selected step, n/Down/Right for next, p/Up/Left for
  previous, Home/End, and Esc close.
- Reuse existing provider/model pickers, masked auth modal, trust inspection,
  theme detail, and language-output controls.
- Update README feature and gap wording, TUI docs, install docs, and the parity
  plan audit.

## Verification

- `cargo test setup_wizard_sequences_first_run_controls --lib`
- `cargo test setup_subcommands_route_to_guided_controls --lib`
- `cargo test setup_command_renders_onboarding_checklist --lib`
- `cargo test composer_slash_hints_include_deepseek_tui_palette_backed_commands --lib`
- `cargo test composer_slash_hints_cover_deepseek_tui_command_registry_names --lib`
- `cargo check`
- `cargo build`
- `cargo fmt --check`
- `git diff --check`

## Residual Gap

The initial stepper opened each focused setup control but did not yet persist
per-step completion state or automatically advance after a picker/action
succeeds. Follow-up spec
`2026-05-14-deepseek-tui-first-run-wizard-state-polish.md` closes that gap.
