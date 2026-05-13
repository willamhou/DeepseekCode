# DeepSeek-TUI TUI Tokens And Cost Commands

Status: implemented

## Gap

DeepSeek-TUI exposes `/tokens` for active context, last input/output, cache,
cumulative token usage, cost, message counts, and model, plus `/cost` for
approximate session spend. DeepSeekCode already persisted durable usage records
and rendered usage summaries in the task panel, but the TUI command registry had
no direct `/tokens` or `/cost` equivalents.

## Implementation

- Extended `TuiUsageSummary` with prompt/completion totals and latest
  prompt/completion token telemetry from durable `UsageRecord`s.
- Added built-in `tokens` / `/tokens` and `cost` / `/cost` parsing before
  custom slash-command fallback.
- Added `TuiMcpDetailKind::Tokens` and `TuiMcpDetailKind::Cost` for scrollable
  right-side detail panels.
- Rendered `/tokens` with active-thread context usage, last input/output token
  counts, cache hit/miss rate, prompt/output totals, cumulative tokens,
  approximate cost, runtime item count, and transcript line count.
- Rendered `/cost` with approximate total, input, and output cost, usage record
  count, total tokens, cache hit/miss rate, and explicit telemetry caveats.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test tokens --lib`
- `cargo test cost --lib`
- `cargo test composer_intercepts_memory_prefix_and_slash_commands --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode does not yet persist provider model labels on TUI thread records,
so `/tokens` cannot render DeepSeek-TUI's exact active model line from the
durable TUI summary. Model/provider switcher parity remains a separate command
registry gap.
