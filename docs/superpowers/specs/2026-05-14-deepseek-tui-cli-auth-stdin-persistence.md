# DeepSeek-TUI CLI Auth Stdin Persistence

**Status:** implemented on 2026-05-14
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

The TUI guided setup now points users at provider/model/trust/theme/language
controls, but auth persistence still required manually editing `.env` or
exporting environment variables. DeepSeek-TUI has a stronger first-run
credential setup path.

This slice closes the safe CLI-level credential persistence gap without putting
secrets in project config or command-line arguments.

## Implementation

- Add `deepseek config auth [ENV] --stdin`.
- Default `ENV` to the current configured `model.api_key_env` when omitted.
- Read the API key only from stdin so it does not appear in shell history.
- Persist the value to the selected workspace `.env`, preserving unrelated
  variables and replacing an existing assignment for the same env var.
- Reject empty, multiline, quoted, or whitespace-containing secrets.
- Print only the env var name and `.env` path; never echo the secret value.
- Update TUI setup guidance, install docs, and README gap wording.

## Verification

- `cargo test cli_from_argv_routes_config_auth_stdin --lib`
- `cargo test cli_from_argv_rejects_config_auth_without_stdin --lib`
- `cargo test persist_auth_secret_updates_dotenv_without_printable_secret --lib`
- `cargo test setup_command_renders_onboarding_checklist --lib`
- `cargo fmt --check`
- `git diff --check`

## Residual Gap

This is still a CLI stdin path, not a masked in-TUI credential-entry wizard.
The TUI points users to the safe command, but it does not yet collect secrets
inside the alternate-screen UI.
