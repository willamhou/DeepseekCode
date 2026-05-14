# DeepSeek-TUI parity: shell supervisor manifest refresh

Status: implemented
Date: 2026-05-14

## Gap

The shell supervisor protocol now reports real active durable shell-job counts,
but `.dscode/shell-supervisor/manifest.json` was still written only at daemon
startup. Manifest-only observers could therefore see stale `active_jobs` and
`updated_at` values even while protocol clients received fresh `status`
responses.

## Implementation

- After a successful durable shell job scan, each supervisor protocol response
  refreshes the workspace supervisor manifest when the state directory exists.
- The refreshed manifest preserves the daemon epoch, socket, protocol method
  inventory, and secret-free `control_token_hash = null` shape.
- If manifest refresh fails, the protocol response remains available and
  includes `manifest_refresh_error` for diagnostics.

## Verification

- `cargo test shell_supervisor_protocol_refreshes_manifest_job_count --lib`
- `cargo test shell_supervisor --lib`
- `cargo check`
- `cargo fmt --check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining Gap

This keeps read-only supervisor metadata current. It still does not implement
native supervisor-owned PTY process start, attach, stdin, resize, replay, wait,
or cancel.
