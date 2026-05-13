# DeepSeek-TUI TUI Cache Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/cache [count|inspect|warmup]` for recent prefix-cache
telemetry, prompt layer hash inspection, and cache warmup. DeepSeekCode already
persisted durable aggregate usage summaries with prompt cache hit/miss tokens,
but the TUI command registry had no direct cache detail command and did not
persist DeepSeek-TUI's per-turn prompt layer hash ring.

## Implementation

- Added built-in `cache [count|inspect|warmup]` / `/cache
  [count|inspect|warmup]` parsing before custom slash-command fallback.
- Added `TuiMcpDetailKind::Cache` for the right-side detail panel.
- Routed command-palette `cache` and composer `/cache` without creating model
  turns or runtime actions.
- Rendered `cache [count]` as a read-only durable usage summary with usage
  record count, requested turn count, prompt tokens, latest prompt tokens,
  cache hit/miss tokens, accounted cache tokens, hit rate, cache chart, context,
  and approximate cost.
- Rendered `cache inspect` with an explicit durable-state limitation: prompt
  layer hashes and prompt text are not persisted in TUI usage records.
- Rendered `cache warmup` with an explicit non-mutating limitation: the command
  does not send a warmup model request from the TUI.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test cache --lib`
- `cargo test composer_intercepts_memory_prefix_and_slash_commands --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode still lacks DeepSeek-TUI's per-turn cache history ring, prompt
layer hash persistence, and real warmup action. This slice closes the visible
TUI command surface with honest durable telemetry and leaves mutating warmup
semantics for a future runtime-backed design.
