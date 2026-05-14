# DeepSeek-TUI Parity: TUI Review Command

## Context

DeepSeek-TUI exposes `/review <target>` as a first-class command. DeepSeekCode
already has an agent-visible deterministic `review` tool, but the TUI slash
surface currently treats `/review` as a custom slash command unless a project
command file exists.

## Goals

- Add `review <target>` / `/review <target>` command-palette and composer
  support before custom slash fallback.
- Add `review help` / `/review help` detail rendering.
- Run the existing local `ReviewTool` with the selected workspace as `cwd`.
- Render the deterministic JSON review output in the right-side detail panel.
- Reject missing targets with a concise usage error.

## Design

`TuiApp` parses review commands into `TuiReviewCommand` and queues a local
file-backed `TuiAction::ReviewTarget`. The CLI action handler invokes
`ReviewTool::default()` with `target`, `cwd`, and a bounded output size. The
result is displayed under a Review detail kind.

This keeps the command deterministic and local. Semantic child-agent review
remains available to the agent-visible tool but is not enabled by default from
the TUI command.

## Acceptance

- `/review <target>` queues a local review action instead of a custom slash
  fallback.
- The local action renders `ReviewTool` JSON output in the detail panel.
- `/review` without a target shows usage.
- `/review help` renders behavior and selected workspace details.
- Tests cover parser/action queuing and local action handling.
