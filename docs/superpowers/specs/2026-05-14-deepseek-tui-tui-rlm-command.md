# DeepSeek-TUI Parity: TUI RLM Command

## Context

DeepSeek-TUI exposes `/rlm [N] <file_or_text>` with `/recursive` as an alias.
The command sends a model instruction that opens a persistent recursive
language model context at bounded depth `0..=3`. DeepSeekCode already has the
underlying `rlm_process` live-session tooling, daemon runner, and HTTP runtime
message submission path, but the TUI slash-command entry point was missing.

## Goals

- Add `rlm [0-3] <file_or_text>` / `/rlm [0-3] <file_or_text>` command palette
  and composer support.
- Add `recursive` / `/recursive` aliases.
- Default omitted depth to `1`; reject depth values outside `0..=3`.
- Render `rlm help` / `/rlm help` in the right-side detail panel.
- Route the command through the existing active-thread `SubmitUserMessage`
  action so local and HTTP runtime TUI sessions share the same behavior.
- Detect existing file targets relative to the selected workspace and instruct
  the agent to pass them as `file_path`; otherwise pass the target as
  `content`.

## Design

The TUI parser produces `TuiRlmCommand::Start { max_depth, target }` or help.
Starting RLM does not directly write RLM daemon manifests from the UI. Instead,
it mirrors DeepSeek-TUI's model-instruction shape by queueing an active-thread
message that asks the agent to call:

```text
rlm_process live=true session_id="slash_rlm_<thread>" max_depth=N file_path|content=...
```

This keeps the UI small, avoids duplicating `rlm_process` manifest/payload
logic, and lets existing local or remote runtime message handling trigger the
same agent tool path.

## Acceptance

- `/rlm <target>` queues an active-thread message with default depth `1`.
- `/rlm 2 <target>` queues an active-thread message with depth `2`.
- `/recursive <target>` behaves as an alias for `/rlm`.
- `/rlm 4 <target>` is rejected with a depth-range error.
- `/rlm help` renders usage, alias, workspace, and active-thread context.
- Existing file targets are represented as `file_path`; non-file targets are
  represented as `content`.
- Tests cover command routing, default/depth parsing, invalid depth rejection,
  alias handling, and help detail rendering.
