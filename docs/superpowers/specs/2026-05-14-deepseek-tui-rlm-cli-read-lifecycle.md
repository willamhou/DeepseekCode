# DeepSeek-TUI RLM CLI Read Lifecycle

Date: 2026-05-14

Status: implemented

## Gap

DeepSeekCode had live RLM daemon lifecycle data through agent-visible tools,
MCP/ACP bridges, and HTTP SSE, but local operators still had to invoke those
tools indirectly. DeepSeek-TUI-style workflows benefit from direct terminal
commands that can inspect live RLM state without starting a model worker.

## Spec

- Add `deepseek agents rlm-status [session_id] [--limit N] [--json]`.
- Add `deepseek agents rlm-events <session_id> [--cursor N|--since-seq N]
  [--limit N] [--json]`.
- Add `deepseek agents rlm-wait <session_id> [--cursor N|--since-seq N]
  [--limit N] [--timeout-ms N] [--poll-interval-ms N] [--json]`.
- Reuse the existing `rlm_process_status`, `rlm_process_events`, and
  `rlm_process_wait` implementations so CLI, MCP/ACP, and agent surfaces report
  the same JSON contract.
- Keep these commands read-only; stateful CLI wrappers for cancel/recover/stop
  remain a separate lifecycle slice.

## Verification

- `cargo test cli_from_argv_routes_agents_subcommands --lib`
- `cargo test rlm_cli_read_lifecycle_args_build_tool_inputs --lib`
- `cargo test rlm_process_events --lib`
- `cargo test rlm_process_status --lib`
- `cargo test build_tool_specs_include_rlm --lib`
- `cargo test completion --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
