# DeepSeek-TUI Masked TUI Auth Wizard

**Status:** implemented on 2026-05-14
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

After `deepseek config auth [ENV] --stdin`, credential persistence was safe but
still outside the alternate-screen onboarding flow. DeepSeek-TUI has a stronger
first-run setup experience because users can complete credentials from the UI.

This slice closes the masked in-TUI credential-entry gap for the selected
workspace.

## Implementation

- Change `/setup auth` from read-only guidance to a masked credential modal.
- Support `/setup auth <ENV>` for explicit env var selection; otherwise use the
  configured `model.api_key_env`.
- Mask the API key while typing and support Enter save, Esc cancel, Backspace,
  Delete, cursor movement, and Ctrl+U clearing.
- Queue a redacted `TuiAction::AuthCredential` whose Debug output hides the
  secret value.
- Persist through the same `.env` writer as `deepseek config auth --stdin`,
  preserving unrelated variables and never rendering the key in TUI detail or
  status output.
- Update README gap wording, TUI docs, and install docs.

## Verification

- `cargo test setup_subcommands_route_to_guided_controls --lib`
- `cargo test handle_tui_action_persists_masked_auth_credential --lib`
- `cargo test setup_command_renders_onboarding_checklist --lib`
- `cargo test composer_slash_hints_include_deepseek_tui_palette_backed_commands --lib`
- `cargo test json_report_includes_onboarding_next_steps_without_secret --lib`
- `cargo check`
- `cargo build`
- `cargo fmt --check`
- `git diff --check`

## Residual Gap

The first-run setup now has a stepper, but it still lacks per-step completion
state and automatic advancement after each focused control succeeds.
