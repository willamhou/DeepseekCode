# DeepSeekCode

`DeepSeekCode` is a DeepSeek-first terminal code agent and local TUI/runtime
workbench. The project focuses on one practical loop: inspect a repository,
edit code, run checks, use the result as feedback, and continue.

The implementation is written in Rust and ships the `deepseek` command, with
`dscode` kept as a compatibility binary.

## Current Status

This repository is an active workbench, not a polished hosted product.

- The core local agent loop is usable: read/search files, apply patches, run
  permissioned shell commands, maintain sessions, and resume work.
- The TUI has moved beyond a demo shell: it has durable sessions/threads,
  transcript rendering, cursor-aware composer and command palette input,
  transcript scrollback, approvals, cancellation, runtime tasks, usage/cost
  panels, diagnostics, compaction, automations, and local rollback commands.
- The runtime contract is file-backed first, with an HTTP/SSE mode for local
  supervisors and TUI clients.
- Compared with `DeepSeek-TUI`, the remaining gap is still material. The
  strongest parts are the agent core, regression tests, and runtime plumbing;
  the biggest missing pieces are product-level TUI polish, broader tool
  surfaces, and long-running workflow ergonomics.

## Feature Surface

- DeepSeek-first model configuration and API key handling
- Interactive REPL and one-shot task execution
- Workspace scanning, file read/search, patch application, diff review
- Permission-gated shell execution and approval flow
- Durable sessions, threads, turns, items, events, tasks, usage, and
  automations under `.dscode/runtime/`
- `deepseek tui` terminal workbench with Plan / Agent / YOLO modes
- Background agent tasks and daemon runner
- HTTP runtime with health, session, thread, task, event, usage, diagnostics,
  automation, and SSE stream endpoints
- LSP-backed and fallback diagnostics runners
- Git rollback snapshots for TUI-started turns
- MCP inventory/tooling, subagents, todo tracking, hooks, prompts, skills, and
  language profiles
- Release packaging for Cargo, npm platform wrappers, Docker, Homebrew
  formula rendering, and GitHub Actions release assets

## Quick Start

Install from a local checkout:

```bash
cargo install --path .
deepseek version
deepseek config init
deepseek doctor --json
deepseek
```

Run a one-shot task:

```bash
deepseek run "explain the current repository structure"
```

Start the TUI:

```bash
deepseek tui
deepseek tui --demo --once
```

Start the local HTTP runtime and connect the TUI to it:

```bash
deepseek serve --http --addr 127.0.0.1:13000
deepseek tui --runtime-url http://127.0.0.1:13000
```

Set `DEEPSEEK_API_KEY` in your environment for real model calls. Local `.env`
files are intentionally ignored by git.

## Development Checks

The main regression loop is:

```bash
cargo fmt --check
cargo test
cargo package --allow-dirty
deepseek tui --demo --once
```

For npm wrapper metadata:

```bash
node npm/scripts/check-version-sync.js
```

## Documentation

- [Install](./docs/install.md)
- [Architecture](./docs/architecture.md)
- [Runtime contract](./docs/runtime.md)
- [TUI workbench](./docs/tui.md)
- [REPL mode](./docs/repl.md)
- [Streaming](./docs/streaming.md)
- [Agent tasks](./docs/agents.md)
- [Todo tool](./docs/todos.md)
- [PR / CI integration](./docs/pr-integration.md)
- [Release checklist](./docs/release.md)
- [Roadmap](./docs/roadmap.md)
- [Changelog](./CHANGELOG.md)

## Public Repository Notes

This repository is public for transparency and collaboration. Public visibility
does not imply a separate open-source grant beyond the terms in
[LICENSE](./LICENSE).

Do not commit local credentials, API keys, runtime state, or private `.env`
files. The tracked examples use placeholders only.
