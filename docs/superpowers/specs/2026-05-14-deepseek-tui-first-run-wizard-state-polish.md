# DeepSeek-TUI First-Run Wizard State Polish

**Status:** implemented on 2026-05-14
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

The first-run setup wizard sequenced provider, model, auth, trust, theme, and
language controls, but it did not show which steps were complete or return to
the next step after successful picker/action work.

## Implementation

- Add per-step `done`, `todo`, and `review` state in the setup wizard modal.
- Validate provider/model from selected workspace config.
- Validate auth from either the process environment or the selected workspace
  `.env` without rendering secret values.
- Treat trust/theme/language as reviewed once the wizard opens those controls,
  while also recognizing persisted trust/theme state where available.
- Return to the wizard and advance to the next incomplete step after provider,
  model, auth, and trust actions complete.
- Surface the same wizard progress in the read-only `/setup` checklist.

## Verification

- `cargo test setup_wizard_tracks_completion_state_and_advances --lib`
- `cargo test setup_wizard_sequences_first_run_controls --lib`
- `cargo test handle_tui_action_persists_masked_auth_credential --lib`
- `cargo check`
- `cargo fmt --check`
- `git diff --check`

## Residual Gap

This closes the first-run wizard completion/validation polish gap. The broader
onboarding surface still needs real model-backed demo evidence and external
release-channel proof for npm/Homebrew once credentials are available.
