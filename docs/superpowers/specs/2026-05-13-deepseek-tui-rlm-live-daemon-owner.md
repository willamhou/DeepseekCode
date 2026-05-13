# DeepSeek-TUI RLM Live Daemon Owner Status

Date: 2026-05-13

Status: implemented

## Gap

Live RLM sessions had `daemon_pid` and `daemon_epoch` manifest fields, but the
worker bridge did not stamp them while claiming a turn. Inventory could show a
running session, but it could not tell whether that owner still existed.

## Spec

- When `rlm_process_run_next` claims a queued turn and writes the live manifest
  to `status=running`, it also records:
  - `daemon_pid`: current worker process id
  - `daemon_epoch`: a fresh epoch label for that worker claim
- Normal worker completion or failure clears `daemon_pid` and `daemon_epoch`
  when the manifest returns to `idle` or `error`.
- `rlm_process_sessions include_live=true` and direct live-session inspection
  report:
  - `daemon_alive`: best-effort pid liveness, or `null` when no pid exists
  - `daemon_stale`: true when a `running` manifest points at a dead/invalid pid
  - `daemon_owner`: `none`, `current`, `external`, or `stale`
- This is a visibility and safety slice. It does not claim provider-level model
  resume or automatic cancellation of a live worker process.

## Verification

- `cargo test rlm_live_sessions_report_daemon_owner_liveness --lib`
- `cargo test rlm_process --lib`
- `cargo test runtime_daemon_tick_routes_live_rlm_turns_through_rlm_worker --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

TUI/ACP subscription polish and stateful lifecycle CLI wrappers remain open.
