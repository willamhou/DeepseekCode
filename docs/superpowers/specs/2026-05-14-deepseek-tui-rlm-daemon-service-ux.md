# DeepSeek-TUI RLM Daemon Service UX

Status: implemented

## Gap

The live RLM loop now has queueing, replay/wait, HTTP SSE, runtime-event
mirroring, lifecycle commands, cancellation, recovery, and an agents-daemon
worker path. The remaining package/service UX gap was that generated and
packaged supervisor files did not explicitly tell operators that
`deepseek agents daemon --json` is also the live RLM worker loop.

## Implementation

- Generated systemd service files now describe the agents daemon as the process
  that handles due automations, pending runtime tasks, stale RLM recovery, and
  one queued live RLM turn per tick.
- Generated launchd plist files include the same operator-facing description.
- Static packaged systemd/launchd templates carry the same RLM live-worker
  explanation.
- Release-package `SERVICES.md`, `docs/agents.md`, `docs/runtime.md`, and the
  DeepSeek-TUI parity plan now document the RLM service lifecycle surface:
  `rlm-status`, `rlm-events`, and `rlm-wait`.

## Verification

- `cargo test service_templates_render_runtime_and_agent_supervisors --lib`
- `cargo test create_release_package_copies_binary_and_writes_scripts --lib`
- `cargo test agents --lib`
- `cargo test update --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP-specific RLM push subscriptions remain open. Published npm package and
Homebrew tap distribution are still tracked under the broader packaging phase.
