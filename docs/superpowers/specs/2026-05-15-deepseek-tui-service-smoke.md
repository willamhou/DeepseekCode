# DeepSeek-TUI Service Smoke

## Context

`deepseek agents service-doctor` closes the static service-template preflight
gap, but the remaining DeepSeek-TUI / Claude Code / Codex parity gap still
calls out service/runtime proof beyond template rendering. DeepSeek-TUI's
Lighthouse deployment doctor checks running processes and localhost health.
DeepSeekCode needs a local, non-installing smoke command that can exercise the
release binary's long-lived surfaces without requiring systemd or launchd.

## Spec

- Add `deepseek agents service-smoke`.
- Accept `--bin`, `--workdir`, `--addr`, `--timeout-ms`, and `--json`.
- Default `--bin` to the current executable, `--workdir` to the current
  directory, `--addr` to `127.0.0.1:0`, and timeout to 5000 ms.
- Start the selected binary as `serve --http --addr <resolved-addr> --once`,
  probe `/health`, and verify the child exits successfully.
- Start the selected binary as `agents shell-supervisor --json`, probe the
  Unix socket `health` method, request `shutdown`, and verify the child exits.
- Treat an already-active shell-supervisor socket as a blocker so the smoke
  command never shuts down an existing workspace supervisor.
- Keep non-Unix shell-supervisor support as a warning rather than a blocker.
- Return non-zero when blockers are found.
- Update release/service docs and the parity plan so release evidence can
  include the local smoke output before clean-machine service installation.

## Verification

- `cli_from_argv_routes_agents_service_smoke`
- `service_smoke_resolves_ephemeral_loopback_addr`
- `service_smoke_json_reports_blockers_and_warnings`
- `service_smoke_blocks_existing_shell_supervisor_socket`
- `cargo test service_smoke --lib`
- `cargo fmt --check`
- `cargo check`
- `cargo build --bin deepseek`
- `target/debug/deepseek agents service-smoke --bin target/debug/deepseek --workdir target --json`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`
