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

- `deepseek` starts the full-screen coding-agent terminal workbench when run in
  a real TTY; `deepseek chat` keeps the line-oriented REPL available.
- `deepseek run` for one-shot coding tasks.
- `deepseek tui` for a keyboard-driven terminal workbench with Plan / Agent /
  YOLO modes.
- Durable sessions, threads, turns, items, events, tasks, usage, and
  automations under `.dscode/runtime/`.
- File read/search, patch application, diff review, todo tracking, rollback
  snapshots, notes, memory, hooks, skills, and subagents.
- OpenAI-compatible single and same-turn batch tool calls, with every call run
  through the normal hook, permission, and recovery paths.
- Permission-gated shell execution plus background shell jobs, wait/poll,
  replay, attach snapshots, stdin, resize metadata, cancellation, and a
  workspace shell-supervisor protocol bridge.
- Runtime approvals support approve-once and approve-for-session, with grouped
  safe command variants and exact denial scoping.
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
deepseek update download-plan --version 0.1.1
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
deepseek
deepseek chat
deepseek run "explain the current repository structure"
```

Start the TUI explicitly:

```bash
deepseek tui
deepseek tui --demo --once
deepseek tui --entrypoint-smoke --smoke-bin "$(command -v deepseek)"
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

- broader terminal/platform proof beyond the TTY-aware default TUI entrypoint,
  PTY entrypoint smoke, and current Unix/Linux native-supervisor PTY smoke
  coverage;
- deeper model-backed live dogfood and external write-fixture sample evidence
  across disposable real repositories;
- npm registry publishing and a Homebrew tap, both blocked on credentials;
- a committed reviewed model-backed README media asset beyond deterministic TUI
  snapshots.

## Demo Asset

The README demo image is an animated SVG generated from the deterministic TUI
snapshot. Regenerate both README SVG assets with the repo-native recorder:

```bash
docs/demo/record-readme-demo.sh
```

`docs/demo/deepseek-code-tui.svg` remains as a static snapshot. For a
launch-quality release, add a short GIF/MP4 of the real model-backed loop:
open TUI, submit a coding request, apply an edit, run tests, inspect the diff.
Keep generated media under `docs/demo/`.

To capture source evidence for that model-backed demo, run the disposable
fixture recorder:

```bash
docs/demo/record-model-backed-demo.sh --dry-run
printf '%s\n' '<deepseek-api-key>' > /tmp/deepseek-demo.key
chmod 600 /tmp/deepseek-demo.key
DEEPSEEK_DEMO_KEY_FILE=/tmp/deepseek-demo.key docs/demo/record-model-backed-demo.sh
latest_log=$(ls -t docs/demo/deepseek-code-model-demo-*.log | head -n 1)
docs/demo/verify-model-backed-demo.js "$latest_log"
docs/demo/render-model-backed-demo-svg.js "$latest_log" --out docs/demo/deepseek-code-model-demo.svg
```

## Development Checks

```bash
cargo fmt --check
cargo test --lib -- --test-threads=1
cargo package --allow-dirty
node scripts/check-secrets.js
docs/demo/verify-model-backed-demo.js --self-test
docs/demo/render-model-backed-demo-svg.js --self-test
deepseek tui --demo --once
```

For npm wrapper metadata:

```bash
node npm/scripts/check-version-sync.js
DEEPSEEK_BINARY=target/debug/deepseek node npm/scripts/test-tui-entrypoint-wrapper.js
node packaging/homebrew/verify-formula.js
```

For release readiness:

```bash
deepseek update publish-status
deepseek update publish-status --dist dist-assets --npm-dist npm-dist --strict
deepseek update publish-status --json
deepseek agents service-doctor --kind all --workdir "$PWD" --bin "$(command -v deepseek)" --json
mkdir -p /tmp/dsc-smk
deepseek agents service-smoke --workdir /tmp/dsc-smk --bin "$(command -v deepseek)" --json
deepseek tui --entrypoint-smoke --smoke-bin "$(command -v deepseek)"
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
deepseek dogfood live-plan --limit 10
deepseek dogfood live-run --limit 3
deepseek dogfood live-run --limit 3 --execute
deepseek dogfood report --limit 20 \
  --require-min-runs 100 \
  --require-success-rate 90 \
  --require-live-runs 100 \
  --require-live-success-rate 90 \
  --require-recent-clean 20 \
  --require-external-write-fixtures 3 \
  --require-category write_validate:25:90 \
  --require-category recovery:25:90 \
  --require-category pr_workflow:25:90 \
  --require-live-category write_validate:25:90 \
  --require-live-category recovery:25:90 \
  --require-live-category pr_workflow:25:90
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
