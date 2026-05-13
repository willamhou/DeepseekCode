# DeepSeek-TUI RLM Live Daemon Runner

## Status

Implemented.

## Goal

Narrow the resident live RLM daemon gap by wiring queued live `rlm_process`
turns into the existing `deepseek agents daemon` service loop.

Before this slice, live RLM turns were durable and could be handled with manual
`rlm_process_run_next` / `rlm_process_drain`, but the built-in daemon's generic
runtime-task runner could also claim `kind=rlm_process` tasks without updating
RLM manifests, payloads, or event logs.

## Behavior

- `deepseek agents daemon` now scans live RLM manifests and runs the oldest
  queued `rlm_process` turn through `rlm_process_run_next` once per tick.
- Generic runtime task execution skips `kind=rlm_process` so live RLM turns
  cannot bypass their persisted payload and event-log state machine.
- JSON daemon ticks include `executed_rlm_turns` and `failed_rlm_turns`.
- JSON mode emits `rlm_turn_completed` or `rlm_turn_failed` records for the
  daemon-handled live turn.
- Existing systemd/launchd service templates already run `deepseek agents
  daemon --json`, so those templates now provide a local live RLM worker loop.

## Verification

- `runtime_daemon_tick_routes_live_rlm_turns_through_rlm_worker` verifies that a
  queued live RLM turn is completed by the daemon through the RLM worker path,
  records `turn_started` / `turn_completed`, and is not counted as a generic
  runtime task.
- Regression commands:
  - `cargo test runtime_daemon --lib`
  - `cargo test rlm_process --lib`
  - `cargo test agents --lib`
  - `cargo test serve --lib`
  - `cargo fmt --check`
  - `cargo check`
  - `git diff --check`

## Remaining Gap

DeepSeekCode still needs TUI/ACP subscription polish and explicit RLM daemon
lifecycle commands.
