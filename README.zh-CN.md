# DeepSeekCode

[English](./README.md) | [中文](./README.zh-CN.md) | [日本語](./README.ja-JP.md)

DeepSeekCode 是一个 DeepSeek-first 的终端代码智能体，也是本地
TUI/runtime workbench。它面向真实写代码循环：阅读仓库、修改文件、运行检查、
查看结果，然后继续在同一个终端里迭代。

> 当前状态：已经可以用于 dogfood 和仓库内编码任务。`v0.1.1` 已有 GitHub
> Release 二进制包和实测可用的 GHCR 镜像；npm 与 Homebrew 发布还需要
> registry/tap 凭据，原生 PTY 和产品化打磨仍在推进中。

<p align="center">
  <img src="./docs/demo/deepseek-code-tui-demo.svg" alt="DeepSeekCode animated TUI demo recording" width="100%">
</p>

## 现在能做什么

- 在真实 TTY 中运行 `deepseek` 会直接打开全屏 coding-agent 终端
  workbench；`deepseek chat` 仍保留行式 REPL。
- 用 `deepseek run` 执行一次性代码任务。
- 用 `deepseek tui` 显式打开键盘驱动的终端 workbench，支持 Plan / Agent /
  YOLO 模式。
- 在 `.dscode/runtime/` 下持久化 sessions、threads、turns、items、
  events、tasks、usage 和 automations。
- 支持文件读取/搜索、补丁应用、diff review、todo、rollback snapshot、
  notes、memory、hooks、skills 和 subagents。
- 支持 OpenAI-compatible 的单个 tool call 与同轮 batch tool calls，每个调用都会
  走正常的 hook、permission 和 recovery 路径。
- 支持带权限门控的 shell 执行，以及后台 shell job、wait/poll、replay、
  attach snapshot、stdin、resize metadata、cancel 和 workspace
  shell-supervisor protocol bridge。
- Runtime approval 支持批准一次和本会话批准；安全命令变体按组复用，拒绝仍按
  exact fingerprint 生效。
- 支持本地 HTTP/SSE runtime、ACP stdio adapter、MCP client/server tooling，
  以及 TUI 内的 MCP 管理界面。
- 支持 guided `/setup` onboarding，包括 first-run done/todo/review 状态、
  provider/model picker、TUI masked auth，以及 CLI stdin auth 持久化。
- 支持 RLM 递归/长输入分析、model-session context、live queue status、
  event replay、cancel、recover 和 drain 控制。
- 支持 LSP-backed 与 fallback diagnostics，并能输出 JSON/JSONL watch。
- 已实测 `v0.1.1` 的 Linux x64、macOS x64、macOS arm64、Windows x64
  Release assets、GHCR 镜像，以及 npm/Homebrew 发布元数据。
- 支持 opt-in 的外部 write-fixture dogfood：先做 preflight，运行时复制到
  isolated workdir，并在 report 里统计证据。

## 快速开始

从源码安装：

```bash
cargo install --git https://github.com/willamhou/DeepSeekCode.git --locked
deepseek version
deepseek doctor --json
```

或者下载 release archive：

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

或者运行已发布的容器镜像：

```bash
docker run --rm ghcr.io/willamhou/deepseekcode:0.1.1 version
```

或者从本地 checkout 安装：

```bash
cargo install --path .
deepseek config init
printf '%s\n' '<api-key>' | deepseek config auth DEEPSEEK_API_KEY --stdin
deepseek doctor --json
```

执行一个代码任务：

```bash
deepseek
deepseek chat
deepseek run "explain the current repository structure"
```

显式启动 TUI：

```bash
deepseek tui
deepseek tui --demo --once
```

启动本地 runtime 并让 TUI 连接：

```bash
deepseek serve --http --addr 127.0.0.1:13000
deepseek tui --runtime-url http://127.0.0.1:13000
```

真实模型调用需要设置 `DEEPSEEK_API_KEY`。本地 `.env` 文件会被 git 忽略。

## 当前差距

DeepSeekCode 已经可以直接拿来写自己的代码，但还没有达到 Claude Code CLI /
Codex CLI 的产品成熟度。最大差距集中在：

- TTY-aware 默认 TUI 入口、PTY entrypoint smoke 和当前 Unix/Linux
  native-supervisor PTY smoke coverage 之外的更多 terminal/platform 证明；
- 真实 disposable 外部仓库上更厚的 live write-fixture 样本证据；
- npm registry 发布和 Homebrew tap，这两项还缺少对应凭据；
- 超出确定性 TUI snapshot、已 review 并提交的真实 model-backed README 媒体素材。

## Demo 素材

README 里的 demo 图是从确定性 TUI snapshot 生成的 animated SVG。用仓库内置
recorder 重新生成 animated 和 static 两个 SVG 素材：

```bash
docs/demo/record-readme-demo.sh
```

`docs/demo/deepseek-code-tui.svg` 保留为静态 snapshot。正式发布前建议再录一段
真实模型循环的短 GIF/MP4：打开 TUI、提交代码请求、应用修改、运行测试、查看
diff。生成素材统一放在 `docs/demo/`。

真实 model-backed demo 的源证据可以用 disposable fixture recorder 捕获：

```bash
docs/demo/record-model-backed-demo.sh --dry-run
DEEPSEEK_API_KEY=... docs/demo/record-model-backed-demo.sh
```

## 开发检查

```bash
cargo fmt --check
cargo test --lib -- --test-threads=1
cargo package --allow-dirty
deepseek tui --demo --once
deepseek tui --entrypoint-smoke --smoke-bin "$(command -v deepseek)"
```

npm wrapper 元数据检查：

```bash
node npm/scripts/check-version-sync.js
DEEPSEEK_BINARY=target/debug/deepseek node npm/scripts/test-tui-entrypoint-wrapper.js
```

发布准备状态：

```bash
deepseek update publish-status
deepseek update publish-status --dist dist-assets --npm-dist npm-dist --strict
deepseek update publish-status --json
deepseek agents service-doctor --kind all --workdir "$PWD" --bin "$(command -v deepseek)" --json
deepseek agents service-smoke --workdir "$PWD" --bin "$(command -v deepseek)" --json
deepseek tui --entrypoint-smoke --smoke-bin "$(command -v deepseek)"
```

PR/CI 工作流检查：

```bash
deepseek pr live-status owner/repo#42
deepseek pr live-status owner/repo#42 --require-write
deepseek pr live-status owner/repo#42 --json
```

外部 write-fixture 证据需要一个位于当前 checkout 之外的 disposable git 仓库。
命令会先 dry-run 检查，然后在 isolated copy 中执行，并把结果写入 dogfood report：

```bash
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --dry-run \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --benchmark-gate \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood report --limit 10
deepseek dogfood report --limit 20 \
  --require-min-runs 100 \
  --require-success-rate 90 \
  --require-recent-clean 20 \
  --require-external-write-fixtures 3 \
  --require-category write_validate:25:90 \
  --require-category recovery:25:90 \
  --require-category pr_workflow:25:90
```

## 文档

- [安装](./docs/install.md)
- [架构](./docs/architecture.md)
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

## 仓库说明

这个仓库公开用于透明协作。公开可见不代表除了 [LICENSE](./LICENSE) 之外的额外
开源授权。

不要提交本地凭据、API key、runtime state 或私有 `.env` 文件。已跟踪示例只使用
占位符。
