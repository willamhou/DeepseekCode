# DeepSeekCode

[English](./README.md) | [中文](./README.zh-CN.md) | [日本語](./README.ja-JP.md)

DeepSeekCode is a DeepSeek-first terminal coding agent and local TUI/runtime
workbench. It is built for the loop you actually use while programming:
inspect a repository, edit files, run checks, review the result, and keep
iterating from the same terminal.

> Status: usable for dogfooding and repository work. `v0.1.1` has GitHub
> Release binaries and a verified GHCR image; npm and Homebrew publishing still
> need registry/tap credentials, and native PTY/product polish remains in
> progress.

<p align="center">
  <img src="./docs/demo/deepseek-code-tui-demo.svg" alt="DeepSeekCode animated TUI demo recording" width="100%">
</p>

## What Works Today

- `deepseek run` for one-shot coding tasks.
- `deepseek tui` for a keyboard-driven terminal workbench with Plan / Agent /
  YOLO modes.
- Durable sessions, threads, turns, items, events, tasks, usage, and
  automations under `.dscode/runtime/`.
- File read/search, patch application, diff review, todo tracking, rollback
  snapshots, notes, memory, hooks, skills, and subagents.
- Permission-gated shell execution plus background shell jobs, wait/poll,
  replay, attach snapshots, stdin, resize metadata, cancellation, and a
  workspace shell-supervisor protocol bridge.
- Local HTTP/SSE runtime, ACP stdio adapter, MCP client/server tooling, and TUI
  MCP management screens.
- Guided `/setup` onboarding with first-run done/todo/review state,
  provider/model pickers, masked TUI auth, and CLI stdin auth persistence.
- RLM helpers for recursive/long-input analysis, model-session context, live
  queue status, event replay, cancellation, recovery, and drain controls.
- LSP-backed and fallback diagnostics runners with JSON/JSONL watch output.
- Verified `v0.1.1` release assets for Linux x64, macOS x64, macOS arm64, and
  Windows x64, plus a GHCR image and npm/Homebrew packaging metadata.
- Opt-in external write-fixture dogfood runs with preflight, isolated workdir
  copies, and report evidence counters.

## Quick Start

Install from source:

```bash
cargo install --git https://github.com/willamhou/DeepSeekCode.git --locked
deepseek version
deepseek doctor --json
```

Or download a release archive:

```bash
curl -L -o deepseek-linux-x64.tar.gz \
  https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.1/deepseek-linux-x64.tar.gz
curl -L -o deepseek-linux-x64.tar.gz.sha256 \
  https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.1/deepseek-linux-x64.tar.gz.sha256
shasum -a 256 -c deepseek-linux-x64.tar.gz.sha256
tar -xzf deepseek-linux-x64.tar.gz
./deepseek version
```

Or run the published container:

```bash
docker run --rm ghcr.io/willamhou/deepseekcode:0.1.1 version
```

Or use a local checkout:

```bash
cargo install --path .
deepseek config init
printf '%s\n' '<api-key>' | deepseek config auth DEEPSEEK_API_KEY --stdin
deepseek doctor --json
```

Run a coding task:

```bash
deepseek run "explain the current repository structure"
```

Start the TUI:

```bash
deepseek tui
deepseek tui --demo --once
```

Start the local runtime and connect the TUI:

```bash
deepseek serve --http --addr 127.0.0.1:13000
deepseek tui --runtime-url http://127.0.0.1:13000
```

Set `DEEPSEEK_API_KEY` for real model calls. Local `.env` files are ignored by
git.

## Current Gap

DeepSeekCode is close enough to use as its own coding CLI, but it is not yet at
Claude Code CLI / Codex CLI polish. The largest remaining gaps are:

- native supervisor-owned PTY attach/stdin/resize/replay/wait/cancel;
- successful live external write-fixture evidence across disposable real
  repositories;
- npm registry publishing and a Homebrew tap, both blocked on credentials;
- richer model-backed demo evidence beyond deterministic TUI snapshots.

## Demo Asset

The README demo image is an animated SVG generated from the deterministic TUI
snapshot:

```bash
svg-term --command "bash -lc 'target/debug/deepseek tui --demo --once | sed -e \"s/^\\\"//\" -e \"s/\\\"$//\" | while IFS= read -r line; do printf \"%s\\n\" \"\$line\"; sleep 0.08; done; sleep 1.5'" \
  --out docs/demo/deepseek-code-tui-demo.svg \
  --width 122 \
  --height 36 \
  --window \
  --no-cursor
```

`docs/demo/deepseek-code-tui.svg` remains as a static snapshot. For a
launch-quality release, add a short GIF/MP4 of the real model-backed loop:
open TUI, submit a coding request, apply an edit, run tests, inspect the diff.
Keep generated media under `docs/demo/`.

## Development Checks

```bash
cargo fmt --check
cargo test --lib -- --test-threads=1
cargo package --allow-dirty
deepseek tui --demo --once
```

For npm wrapper metadata:

```bash
node npm/scripts/check-version-sync.js
```

For release readiness:

```bash
deepseek update publish-status
deepseek update publish-status --dist dist-assets --npm-dist npm-dist --strict
deepseek update publish-status --json
```

For PR/CI workflow checks:

```bash
deepseek pr live-status owner/repo#42
deepseek pr live-status owner/repo#42 --require-write
deepseek pr live-status owner/repo#42 --json
```

For opt-in external write-fixture evidence, use a disposable git repository
outside this checkout. The command dry-runs preflight first, then runs against
an isolated copy and records the result in the dogfood report:

```bash
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --dry-run \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --benchmark-gate \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood report --limit 10
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

## Repository Notes

This repository is public for transparency and collaboration. Public visibility
does not imply a separate open-source grant beyond the terms in
[LICENSE](./LICENSE).

Do not commit local credentials, API keys, runtime state, or private `.env`
files. The tracked examples use placeholders only.
