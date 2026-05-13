# DeepSeek-TUI TUI Provider Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/provider [name] [model]` for switching between
DeepSeek, hosted OpenAI-compatible providers, and local inference servers.
DeepSeekCode already had the lower-level `model.base_url`,
`model.api_key_env`, and `model.model` settings, but the TUI command registry
had no provider command. Users had to edit `.dscode/config.toml` directly.

## Implementation

- Added built-in `provider` / `/provider`, `provider list`, and
  `provider <name> [model]` / `/provider <name> [model]` parsing before
  custom slash-command fallback.
- Added `TuiProviderCommand`, `TuiAction::Provider`, and
  `TuiMcpDetailKind::Provider`.
- Added project config helpers for provider summary and provider preset
  switching. Missing `.dscode/config.toml` files are initialized through the
  existing config bootstrap.
- Supported DeepSeek-TUI-style aliases for `deepseek`, `nvidia-nim` / `nim`,
  `openai`, `atlascloud`, `openrouter`, `novita`, `fireworks`, `sglang`,
  `vllm`, and `ollama`.
- Provider switching updates the selected workspace's `model.base_url`,
  `model.api_key_env`, and `model.model`. Optional `flash` / `pro` model
  shorthand is mapped to provider-specific DeepSeek V4 model ids where needed.
- Rendered `provider` as a right-side detail panel showing inferred provider,
  base URL, API-key env var, model, reasoning effort, and config path.
- Rendered `provider list` as an offline provider preset catalog with the
  current provider highlighted.
- Kept HTTP-runtime TUI provider commands local-only because they mutate local
  project config, not remote runtime state.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test command_palette_requests_provider_actions --lib`
- `cargo test handle_tui_action_manages_provider_config --lib`
- `cargo test composer_intercepts_memory_prefix_and_slash_commands --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode does not yet have DeepSeek-TUI's interactive provider picker or a
remote runtime API for provider mutation. API-key persistence remains outside
this TUI command; users still set provider keys through environment variables
referenced by `model.api_key_env`.
