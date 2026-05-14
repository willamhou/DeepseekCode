# DeepSeek-TUI Service Lifecycle Guide

Date: 2026-05-14

Status: implemented

## Gap

DeepSeekCode already rendered systemd and launchd templates for the HTTP
runtime, agents daemon, diagnostics worker, and shell supervisor. The remaining
operator gap was lifecycle clarity: after rendering templates, users still had
to infer install, start, status, log, restart, stop, unload, and health-check
commands from scattered docs.

## Implementation

- `deepseek agents service --out <dir>` now writes `<dir>/SERVICES.md` next to
  generated service templates.
- The guide records the selected binary, workspace, runtime address, worker
  interval, daemon budget, and generated files.
- For systemd renders, it includes `systemctl --user` install/start/status/log,
  restart, stop, and disable commands.
- For launchd renders, it includes `launchctl load`, `launchctl list`,
  `/tmp/deepseek-*.log` tailing, `launchctl kickstart`, and unload commands.
- The guide also includes runtime checks:
  - `curl -fsS /v1/health`
  - `deepseek doctor --json`
  - `deepseek agents rlm-status --json`
  - `deepseek agents shell status --json`
  - `deepseek diagnostics --changed --json`
- Package `SERVICES.md`, install docs, runtime docs, and release smoke docs now
  point operators at the generated lifecycle guide.

## Verification

- `cargo test render_agent_services_writes_lifecycle_guide --lib`
- `cargo test service_templates_render_runtime_and_agent_supervisors --lib`
- `cargo test create_release_package_copies_binary_and_writes_scripts --lib`
- `cargo test agents --lib -- --test-threads=1`
- `cargo test update --lib -- --test-threads=1`
- `cargo fmt --check`
- `cargo check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining

This closes local lifecycle guidance for rendered service managers. Remaining
service/distribution evidence is an actual installed systemd or launchd smoke
on a clean machine, plus the still-open Windows ConPTY shell path.
