# DeepSeek-TUI Runtime Diagnostics Broker Parity

Date: 2026-05-13

## Gap

DeepSeek-TUI keeps diagnostics close to the runtime/workbench so foreground
clients do not have to repeatedly spawn independent language-server probes.
DeepSeekCode had local CLI diagnostics and opt-in post-edit diagnostics, while
the parity plan still described the broader cross-process broker as an open
gap.

## Scope

- Verify the HTTP runtime diagnostics broker that exposes `/v1/diagnostics`.
- Verify HTTP-runtime TUI sessions call that broker for diagnostics commands.
- Update the parity plan so the remaining gap is the narrower standalone daemon
  protocol, not the already-landed HTTP runtime broker.

## Acceptance

- `GET /v1/diagnostics` advertises the diagnostics endpoint and schema.
- `POST /v1/diagnostics` accepts `changed` / `paths` input and returns
  `deepseek.runtime.diagnostics.v1`.
- The runtime process owns the warmed diagnostics session, allowing repeated
  runtime clients to share that broker.
- HTTP TUI diagnostics actions call `/v1/diagnostics` instead of running a
  separate local diagnostics process.
- Runtime docs and the DeepSeek-TUI parity plan state the remaining boundary.

## Implementation

- Confirmed `RuntimeDiagnosticsBroker` in `src/cli/commands/serve.rs` owns a
  warmed `WarmDiagnosticSession` behind the HTTP runtime state.
- Confirmed `POST /v1/diagnostics` serializes the broker result as
  `deepseek.runtime.diagnostics.v1`.
- Confirmed `run_remote_tui_diagnostics` in `src/cli/commands/tui.rs` posts
  HTTP-runtime diagnostics commands to `/v1/diagnostics`.
- Updated the parity plan remaining list to call out only the missing standalone
  diagnostics daemon protocol.

## Verification

- `cargo test diagnostics_endpoint_runs_via_runtime_broker --lib` passed.
- `cargo test remote_diagnostics_status_summarizes_runtime_report --lib`
  passed.
- `cargo test run_tui_diagnostics_reports_status --lib` passed.

## Remaining Differences

- There is no dedicated diagnostics-daemon protocol independent of `serve
  --http`; plain local CLI diagnostics still runs in the invoking process unless
  the user routes through the HTTP runtime.
