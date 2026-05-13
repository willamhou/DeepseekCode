# DeepSeek-TUI RLM Live Recovery Owner Guard

Date: 2026-05-13

Status: implemented

## Gap

`rlm_process_recover` could requeue or fail a `running` live RLM turn based only
on task/payload status. After daemon pid/epoch stamping landed, recovery also
needs to respect a live owner process so an operator does not accidentally
clobber a worker that is still running.

## Spec

- `rlm_process_recover` reads the live manifest owner status before mutating
  interrupted turns.
- If a candidate turn is `running` and the manifest `daemon_pid` is still
  alive, recovery records `action=skip_live_owner_alive` and leaves the task,
  payload, manifest `active_turn_id`, and daemon owner stamp intact.
- `force=true` overrides the guard for explicit operator takeover.
- `all=true` propagates `force` to each scanned session.
- Recovery output includes `force`, `daemon_alive`, and `daemon_stale` so
  callers can explain why a turn was skipped or recovered.

## Verification

- `cargo test rlm_process_recover_skips_live_daemon_owner_unless_forced --lib`
- `cargo test rlm_process --lib`
- `cargo test build_tool_specs_include_rlm --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

Daemon-tick stale-owner recovery is implemented in
`2026-05-13-deepseek-tui-rlm-live-daemon-auto-recover.md`. Broader lifecycle
status commands and TUI/ACP subscription polish remain separate gaps.
