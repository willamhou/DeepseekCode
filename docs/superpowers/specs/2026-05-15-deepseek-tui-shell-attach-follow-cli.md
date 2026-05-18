# DeepSeek-TUI Shell Attach Follow CLI

**Status:** implemented
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at
`/tmp/deepseek-tui-compare-20260514`, `origin/main`
`b8345488978265cd94990364edcdefbb21bc5f15`.

## Gap

DeepSeekCode already had a shell-supervisor protocol with `attach` snapshots,
terminal event replay, stdin, resize, and cancel. The human-facing
`deepseek agents shell attach` command was still a one-shot snapshot, so an
operator had to manually rerun it with the returned cursor to watch an active
supervisor-owned PTY job. That was weaker than DeepSeek-TUI-style terminal
monitoring.

## Implementation

- Added `deepseek agents shell attach <task_id> --follow`.
- Follow mode repeatedly sends bounded `attach` requests, advances via
  `next_cursor` for terminal-event logs or `next_offset` for stdout-backed logs,
  and prints only the `terminal:` payload in non-JSON mode.
- `--wait-ms` controls each attach long-poll; follow mode defaults to a
  1000ms per-request wait.
- `--poll-ms` controls the local sleep after a no-progress running snapshot.
- `--max-ms` provides a bounded follow for scripts and tests.
- `--json --follow` emits newline-delimited raw protocol responses.
- Durable process liveness probing no longer reaps zombie children that are
  still owned by the current DeepSeekCode process; the owning `Child` handle
  remains responsible for `try_wait()`, so attach/follow does not fail with
  `ECHILD` after a status probe sees a short-lived job.
- Runtime docs now describe current native-supervisor PTY support instead of
  the stale "native PTY sessions are not implemented" wording.

## Verification

- Parser coverage for `--follow`, `--poll-ms`, and `--max-ms`.
- Unit coverage for request rendering and follow cursor/payload parsing.
- Regression coverage that process liveness probes do not reap owned child
  zombies before shell manager refresh can collect the exit status.
- Real local protocol smoke:
  - start `target/debug/deepseek agents shell-supervisor --json` in a temporary
    workspace;
  - run `target/debug/deepseek agents shell start "echo alpha" --json`;
  - run `target/debug/deepseek agents shell attach <task_id> --follow --max-ms
    2000 --wait-ms 250` and observe `alpha`;
  - shut the supervisor down with `target/debug/deepseek agents shell shutdown
    --json`.
- The change remains a human CLI improvement, not a raw terminal takeover.

## Remaining

Full shell parity still needs a terminal takeover UI, Windows ConPTY, and actual
installed systemd/launchd smoke evidence outside the local template smoke.
