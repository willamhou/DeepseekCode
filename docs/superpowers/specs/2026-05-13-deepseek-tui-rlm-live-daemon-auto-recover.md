# DeepSeek-TUI RLM Live Daemon Auto Recovery

Date: 2026-05-13

Status: implemented

## Gap

Live RLM recovery existed as an explicit tool, and daemon owner liveness was
visible in manifests, but the runtime daemon did not automatically repair stale
running turns before claiming more work.

## Spec

- Each `deepseek agents daemon` tick runs `rlm_process_recover all=true` before
  claiming a queued live RLM turn.
- Recovery uses the default owner guard: turns owned by a live daemon pid are
  skipped unless an operator later calls `rlm_process_recover force=true`.
- The daemon tick records:
  - `recovered_rlm_turns`
  - `failed_rlm_recoveries`
- JSON daemon output emits `rlm_recovery_completed` when recovery mutates at
  least one turn and `rlm_recovery_failed` when recovery itself fails.
- After stale recovery requeues a turn, the same tick may claim and run one
  queued live RLM turn through the existing worker path.

## Verification

- `cargo test runtime_daemon_tick_recovers_stale_live_rlm_owner_before_running_queue --lib`
- `cargo test runtime_daemon_tick_routes_live_rlm_turns_through_rlm_worker --lib`
- `cargo test rlm_process --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
