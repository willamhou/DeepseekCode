# DeepSeek-TUI TUI Model Commands

Status: implemented

## Gap

DeepSeek-TUI exposes `/model [name]` for model inspection/switching and
`/models` for available model discovery. DeepSeekCode already supported
`model.model = "auto"` and DeepSeek V4 routing in the core model layer, but
the TUI command registry had no direct model command. Users had to edit
`.dscode/config.toml` outside the workbench.

## Implementation

- Added built-in `model` / `/model`, `model show` / `/model show`,
  `model <name>` / `/model <name>`, and `models` / `/models` parsing before
  custom slash-command fallback.
- Added `TuiModelCommand`, `TuiAction::Model`, and `TuiMcpDetailKind::Model`.
- Routed model commands through the selected session workspace, matching other
  local project-config TUI commands.
- Added local config helpers for reading model config and updating
  `model.model` in `.dscode/config.toml`; missing config files are initialized
  through the existing project config bootstrap.
- Supported common shorthand aliases: `auto`, `flash`, `pro`, `chat`, `coder`,
  and `reasoner`.
- Rendered `model` as a right-side detail panel showing project model,
  reasoning effort, base URL, API-key env var, and config path.
- Rendered `models` as an offline DeepSeekCode model catalog with the current
  project model highlighted.
- Refreshed local TUI agent-run config before starting a run so a model change
  made in the workbench can affect the next submitted turn.
- Kept HTTP-runtime TUI model commands local-only because they mutate local
  project config, not remote runtime state.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test command_palette_requests_model_actions --lib`
- `cargo test handle_tui_action_manages_model_config --lib`
- `cargo test composer_intercepts_memory_prefix_and_slash_commands --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

The interactive local model picker is covered by
`2026-05-14-deepseek-tui-tui-model-picker.md`. DeepSeekCode still does not have
online `/models` API fetch, and remote HTTP-runtime model changes need a runtime
API contract before they can be safely exposed.
