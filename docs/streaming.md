# Streaming Output

`deepseek` streams LLM output token-by-token over Server-Sent Events
(SSE) by default. Both OpenAI-compatible (`/chat/completions`) and
Anthropic-compatible (`/messages`) base URLs are supported.

## What you see

When stdout is a TTY, the renderer applies ANSI:
- Cyan: assistant text (streamed live)
- Yellow `🛠 name(args)`: tool call (rendered after args fully assembled)
- Green `✓ name`: tool succeeded
- Red `✗ name`: tool failed
- Dim `─── step N ───`: step divider

Pipe stdout to a file (`deepseek run "..." > out.txt`) and ANSI is
suppressed automatically — `is_terminal()` detects the redirection.

## Implementation

`curl -sS -N --max-time 60` is spawned per LLM call. The `-N`
(no-buffer) flag is critical — without it curl batches output by
buffer-fill rather than by SSE frame boundary. Frames are read by
`util::sse::read_frame` from a `BufRead` over child stdout. Per
protocol:

- OpenAI: `delta.content` chunks dispatch to `on_text_delta` live.
  `delta.tool_calls[]` accumulates across frames into a single
  `OpenAiToolAssembly` and is rendered once `finish_reason ==
  "tool_calls"`. Final `usage` frame (requested via
  `stream_options.include_usage`) carries token counts. `[DONE]`
  closes the stream.
- Anthropic: `event: text_delta` chunks dispatch live. `event:
  input_json_delta` accumulates `partial_json` for tool-use blocks,
  parsed once `content_block_stop` arrives. `message_delta` carries
  output tokens; `message_stop` closes the stream.

## Failure modes

- Curl exit non-zero → `tool_failure` with stderr tail (last 64 KB)
- HTTP 4xx/5xx errors emit an `error` frame → `tool_failure(api error)`
- Malformed SSE → `tool_failure(malformed sse frame: ...)`
- Tool args JSON unparseable after assembly → `tool_failure(...)`

Already-streamed tokens are not rolled back on error — the red `✗`
mark appears after them, matching Claude Code / Codex behavior. The
parser still calls `on_assistant_done` exactly once even on error,
preserving the `StreamEvents` trait contract so the renderer can
close cyan ANSI cleanly.

## Offline planner

When `DEEPSEEK_API_KEY` is unset, the offline planner runs locally
and still drives `StreamEvents`, so the renderer paints the same
colors. Tokens arrive in one block rather than progressively.

## Limitations (Phase 9c candidates)

- Ctrl+C does not interrupt an in-flight stream
- `up`/`down` arrow history not implemented (use `rlwrap deepseek`)
- Patch / shell tool output not streamed (rendered in one block)
- Syntax highlight not applied
- Color theme not user-configurable
