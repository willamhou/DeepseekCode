# DeepSeek-TUI RLM CLI Stateful Lifecycle

Date: 2026-05-14

Status: implemented

## Gap

Read-only RLM lifecycle commands exposed status, events, and long-poll waits,
but local operators still lacked direct terminal commands for the state-changing
live RLM controls already available as tools. DeepSeek-TUI-style supervisor
workflows need those controls without requiring an MCP client or model-visible
tool call.

## Spec

- Add `deepseek agents rlm-cancel <session_id> [task_id] [--all] [--force]
  [--reason TEXT] [--json]`.
- Add `deepseek agents rlm-recover [session_id] [--all] [--mode requeue|fail]
  [--dry-run] [--force] [--limit N] [--reason TEXT] [--json]`.
- Add `deepseek agents rlm-stop <session_id> [--reason TEXT] [--json]`.
- Add `deepseek agents rlm-run-next <session_id> [task_id] [--dry-run]
  [--json]`.
- Add `deepseek agents rlm-drain <session_id> [--max-turns N] [--dry-run]
  [--json]`.
- Reuse the existing `rlm_process_*` tool implementations so CLI and
  agent/MCP/ACP surfaces mutate the same manifests, runtime tasks, payloads,
  and event logs.
- Keep `--json` as the exact tool-output contract and provide concise default
  terminal summaries.

## Verification

- `cargo test cli_from_argv_routes_agents_subcommands --lib`
- `cargo test rlm_cli_stateful_lifecycle_args_build_tool_inputs --lib`
- `cargo test rlm_process_cancel --lib`
- `cargo test rlm_process_recover --lib`
- `cargo test rlm_process_stop --lib`
- `cargo test rlm_process_drain --lib`
- `cargo test completion --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
