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
  Unix socket `health` method, run a control smoke through `start` -> `wait` ->
  `attach`, request `shutdown`, and verify the child exits.
- On Linux, the shell-supervisor control smoke should request `tty=true` and
  require the `native-supervisor` PTY backend so release evidence proves the
  platform PTY path, not only non-interactive shell execution.
- Treat an already-active shell-supervisor socket as a blocker so the smoke
  command never shuts down an existing workspace supervisor.
- Treat a too-long absolute shell-supervisor socket path as a blocker before
  spawning the child process, with guidance to use a short isolated `--workdir`
  such as `/tmp/dsc-smk`.
- Keep non-Unix shell-supervisor support as a warning rather than a blocker.
- Return non-zero when blockers are found.
- Update release/service docs and the parity plan so release evidence can
  include the local smoke output before clean-machine service installation.

## Verification

- `cli_from_argv_routes_agents_service_smoke`
- `service_smoke_resolves_ephemeral_loopback_addr`
- `service_smoke_json_reports_blockers_and_warnings`
- `service_smoke_shell_supervisor_control_smoke_runs_start_wait_attach`
- `service_smoke_blocks_existing_shell_supervisor_socket`
- `cargo test service_smoke --lib`
- `cargo fmt --check`
- `cargo check`
- `cargo build --bin deepseek`
- `mkdir -p /tmp/dsc-smk`
- `target/debug/deepseek agents service-smoke --bin target/debug/deepseek --workdir /tmp/dsc-smk --json`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`
