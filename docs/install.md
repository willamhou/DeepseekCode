# 安装

`deepseek` 是默认命令名。推荐先安装，再用 `deepseek version` 和 `deepseek doctor` 做最小验证。

## 从源码安装

从公开仓库直接安装：

```bash
cargo install --git https://github.com/willamhou/DeepSeekCode.git --locked
deepseek version
deepseek doctor --json
```

从本地 checkout 安装：

```bash
cargo install --path .
deepseek version
deepseek doctor
deepseek doctor --json
deepseek update --check
deepseek update verify-install --bin "$(command -v deepseek)"
```

如果你只想先本地构建 release binary：

```bash
cargo build --release
./target/release/deepseek version
./target/release/deepseek update verify-install --bin ./target/release/deepseek
```

## 可选本地工具

部分 DeepSeek-TUI 兼容工具会调用本机可执行文件：

- `web_run.screenshot` 打开 PDF 后用 `pdftotext` / Poppler 提取页文本。
- `pandoc_convert` 需要 `pandoc`。
- `image_ocr` 需要 `tesseract`。

## 发布前检查

本仓库的最小 release gate 是：

```bash
cargo fmt --check
cargo test -- --test-threads=1
cargo package --allow-dirty
deepseek benchmark
deepseek version
deepseek doctor
deepseek doctor --json
deepseek update --check
deepseek update package --bin target/release/deepseek
```

完整发布流程见 [发布检查清单](./release.md)。

`deepseek benchmark` 会同时检查：

- benchmark case expectations
- benchmark trend gate
- dogfood live gate

任一 gate 失败都应阻断 release。

如果新增 dogfood 失败已经完成排查，并且需要把当前 live snapshot 作为新的已知基线，必须显式运行
`deepseek benchmark --accept-live-baseline`；普通发布检查不要使用这个选项。

发布前还应至少回放一个普通写入验证任务和一个 retry 任务：

```bash
deepseek dogfood run --from-benchmark fixture-write-validate-rust-mini --notes "release replay"
deepseek dogfood run --from-benchmark fixture-retry-write-validate-python-mini --notes "release retry replay"
deepseek dogfood report --limit 5
```

外部 write-fixture 证据需要当前 checkout 之外的 disposable git 仓库。先 dry-run
检查，真实运行会复制到 isolated workdir，并在 dogfood report 里计入
`external-write-fixture`：

```bash
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --dry-run \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --benchmark-gate \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood report --limit 10
```

## Release Binary

GitHub Release 已经提供 `v0.1.1` 的 Linux x64、macOS x64、macOS arm64 和
Windows x64 包，以及对应 `.sha256` 文件。例如 Linux x64：

```bash
curl -L -o deepseek-linux-x64.tar.gz \
  https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.1/deepseek-linux-x64.tar.gz
curl -L -o deepseek-linux-x64.tar.gz.sha256 \
  https://github.com/willamhou/DeepSeekCode/releases/download/v0.1.1/deepseek-linux-x64.tar.gz.sha256
shasum -a 256 -c deepseek-linux-x64.tar.gz.sha256
tar -xzf deepseek-linux-x64.tar.gz
./deepseek version
```

本地 release binary 路径固定为：

```bash
cargo build --release
./target/release/deepseek version
./target/release/deepseek update package --bin ./target/release/deepseek
```

发布产物至少应包含：

- `deepseek` binary
- verified Cargo package output
- `release.json`（version、platform、commit、binary size）
- `install.sh`
- `rollback.sh`
- `VERIFY.md`
- `SERVICES.md`
- `services/systemd/*.service` 与 `services/launchd/*.plist` 模板
- `deepseek update verify-install` 输出
- 支持的平台说明
- 安装与升级说明链接

Cargo registry 分发目前是明确的 source-build/package-only 策略：
`Cargo.toml` 设置 `publish = false`，release workflow 会跳过 Cargo registry
发布。只有在确定 crates.io 或私有 registry 归属后才移除该策略。

`dscode` 只作为兼容别名，不作为主 release artifact 名称。

## Docker 与 npm Wrapper

本仓库提供本地 Docker artifact：

```bash
docker build -t deepseek-code:local .
docker run --rm deepseek-code:local version
```

Tag 版 `Release Matrix` workflow 会把同一个 Dockerfile 构建并推送到 GHCR：

```bash
docker pull ghcr.io/willamhou/deepseekcode:0.1.1
docker run --rm ghcr.io/willamhou/deepseekcode:0.1.1 version
```

同一次 tag 发布会写入 `<version>`、`v<version>` 和 `latest` 三个 tag；镜像名会按
GHCR 要求转成小写。`v0.1.1` 的公开镜像已经通过 pull 和 `version` smoke test。

npm wrapper 位于 `npm/`，用于发布时把平台 binary 包装成 `deepseek` 命令。root 包通过 optional dependency 解析当前平台的 binary 包，例如 `@deepseek-code/cli-linux-x64`、`@deepseek-code/cli-macos-arm64`、`@deepseek-code/cli-macos-x64` 和 `@deepseek-code/cli-windows-x64`。发布前至少验证 wrapper 语法、平台包解析和本地 binary 转发：

```bash
(cd npm && npm run check:version)
(cd npm && npm test)
DEEPSEEK_BINARY=./target/release/deepseek node npm/bin/deepseek.js version
node npm/scripts/stage-platform-package.js --platform linux-x64 --binary ./target/release/deepseek
node npm/scripts/verify-platform-package.js --platform linux-x64
deepseek update publish-status
deepseek update publish-status --json
```

Release Matrix 会把每个平台的 release binary stage 到
`npm/platforms/<platform>/bin`，先 smoke-run staged package binary，再打出平台
npm tarball，并在 tag run 且配置 `NPM_TOKEN` 时先发布平台包，再发布 root wrapper
包。
正式发布前可以在下载 workflow artifacts 后运行
`deepseek update publish-status --dist dist-assets --npm-dist npm-dist --strict`
检查 npm token、平台 tarball、Homebrew tap 配置和 release `.sha256` 文件是否
齐全；加 `--json` 会输出 `deepseek.publish_status.v1`，便于 CI 或 release
脚本消费。输出中的 `public_install` 会区分 source checkout、GitHub Release、
npm、Homebrew、GHCR 和 Cargo registry 当前是 `source_available`、
`ready_to_publish`、`requires_publish` 还是 `source_only_policy`。目前 GitHub
Release 和 GHCR 已有公开验证；`v0.1.1` 的 npm 发布 job 因缺少 `NPM_TOKEN`
明确跳过，Homebrew tap job 也因缺少 `HOMEBREW_TAP_REPOSITORY` 或
`HOMEBREW_TAP_TOKEN` 跳过。在配置 registry/tap 凭据并完成外部验证前，不要把
npm/Homebrew 写成已可用。

## Runtime 服务模板

如果需要把 runtime API、durable task daemon 和 diagnostics watch worker 作为
本地长期服务运行，先在目标 workspace 生成可审阅的 supervisor 文件：

```bash
deepseek agents service --kind systemd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
deepseek agents service --kind launchd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
```

Linux 用户通常把 `services/systemd/*.service` 安装到
`~/.config/systemd/user/`，macOS 用户把 `services/launchd/*.plist` 安装到
`~/Library/LaunchAgents/`。生成内容包括 `serve --http`、`agents daemon --json`、
`diagnostics --watch --changed --json` 和 `agents shell-supervisor --json`。
`agents service --out` 同时写入 `SERVICES.md`，列出 systemd/launchd 的
install、start、status、logs、restart、stop、disable/unload 和 runtime
health-check 命令；命令只生成文件，不会自动 enable、load 或 start。

## Homebrew

Homebrew formula 模板位于 `packaging/homebrew/deepseek.rb`。它指向 GitHub
release assets：

- `deepseek-macos-arm64.tar.gz`
- `deepseek-macos-x64.tar.gz`
- `deepseek-linux-x64.tar.gz`

正式发布前必须把 formula 里的 `sha256` 占位值替换为对应 release asset 的真实
SHA-256。GitHub `Release Matrix` workflow 会为每个 archive 上传旁路
`.sha256` 文件并创建 signed artifact attestations，优先使用这些值填写
formula，确保 tap 和发布资产完全一致。安装前可用 `gh attestation verify
<archive> --repo <owner>/<repo>` 验证 provenance。然后运行：

```bash
ruby -c packaging/homebrew/deepseek.rb
brew install --build-from-source packaging/homebrew/deepseek.rb
deepseek version
deepseek doctor --json
```

Tag 发布时如果配置了 repository variable `HOMEBREW_TAP_REPOSITORY` 和 secret
`HOMEBREW_TAP_TOKEN`，Release Matrix 会在 GitHub Release assets 发布后自动渲染
并推送 tap 仓库的 `Formula/deepseek.rb`。
`deepseek update publish-status --dist <release-assets> --strict` 会把缺少 tap
变量或占位 checksum 识别为未 ready。

## 升级

从源码升级：

```bash
git pull
cargo install --path . --force
deepseek version
deepseek doctor
```

如果是使用 release binary 升级，先保留当前版本：

```bash
deepseek update install-package --package target/deepseek-release/deepseek-<version>-<platform>
deepseek update verify-install --bin "$(command -v deepseek)"
```

也可以显式指定目标路径和 rollback 目录：

```bash
deepseek update install-package \
  --package target/deepseek-release/deepseek-<version>-<platform> \
  --dest ~/.local/bin/deepseek \
  --backup-dir ~/.local/bin/deepseek-rollback
```

配置文件和会话默认保存在 `.dscode/`，升级 binary 不应删除这些文件。

## 回滚

如果升级后需要回滚 release binary：

```bash
deepseek update rollback
deepseek update verify-install --bin "$(command -v deepseek)"
```

如果是从源码安装，回滚到指定 commit：

```bash
git checkout <known-good-commit>
cargo install --path . --force
deepseek version
```

## 首次配置

```bash
deepseek config init
deepseek doctor
```

`deepseek config init` 会创建项目级 `.dscode/config.toml`、session 目录、custom command 目录、custom agent 目录和 hooks 事件目录。
如果确实要覆盖已有配置，可以显式运行 `deepseek config init --force`。
它也会创建 `.dscode/mcp.json`，用于记录项目级 MCP server 定义。

`deepseek doctor` 会输出 `[onboarding]` checklist：配置文件是否存在、当前 provider
的 API key 环境变量是否存在，以及下一步应该运行 `deepseek config init`、
`deepseek config auth ... --stdin`、`export DEEPSEEK_API_KEY=...`、`deepseek smoke`
还是 `deepseek tui`。自动化环境可以用
`deepseek doctor --json` 读取同名 `onboarding` 对象；该对象只包含 env var 名称和下一步命令，
不会回显密钥值。

如果希望把 API key 写入当前 workspace 的 `.env`，使用 stdin，避免把密钥放进 shell
history：

```bash
printf '%s\n' "$DEEPSEEK_API_KEY" | deepseek config auth DEEPSEEK_API_KEY --stdin
```

在 TUI 中也可以运行 `/setup wizard` 串起 provider、model、auth、trust、theme 和
language 设置。wizard 会显示每一步的 done/todo/review 状态，完成
provider/model/auth/trust 后自动回到下一步；也可以直接运行 `/setup auth` /
`/setup auth <ENV>`，用 masked 输入框把密钥写入当前选中 workspace 的 `.env`。

`deepseek` 会自动读取当前工作目录下的 `.env`，并在变量尚未存在于进程环境时注入。常用 DeepSeek/OpenAI-compatible 配置：

```bash
DEEPSEEK_API_KEY=...
DEEPSEEK_BASE_URL=https://api.deepseek.com
DEEPSEEK_MODEL=auto # auto | deepseek-v4-flash | deepseek-v4-pro | deepseek-chat
DEEPSEEK_REASONING_EFFORT=off # off | high | max | auto
DSCODE_VISION_API_KEY_ENV=OPENAI_API_KEY # optional image_analyze vision tool
DSCODE_VISION_BASE_URL=https://api.openai.com/v1
DSCODE_VISION_MODEL=gpt-4.1
```

如果 `.env` 或 shell 环境里设置了 `DEEPSEEK_BASE_URL` / `DEEPSEEK_MODEL` / `DEEPSEEK_REASONING_EFFORT`，它们会覆盖 `.dscode/config.toml` 里的 `model.base_url` / `model.model` / `model.reasoning_effort`。
`DSCODE_VISION_BASE_URL` / `DSCODE_VISION_MODEL` / `DSCODE_VISION_API_KEY_ENV`
会覆盖 `image_analyze` 使用的 `vision.base_url` / `vision.model` /
`vision.api_key_env`。
`model.model = "auto"` 会按任务复杂度路由：简单/探测任务走 `deepseek-v4-flash`，规划、审查、架构、安全、迁移和多轮恢复类任务走 `deepseek-v4-pro`；Runtime usage 会记录实际使用的模型名，而不是只记录 `auto`。
`model.reasoning_effort = "off"` 会显式发送 DeepSeek V4 `thinking.disabled`；
`"high"` / `"max"` 会发送官方 thinking mode 和 reasoning effort 参数；`"auto"` 会随模型路由在 `off` / `high` / `max` 间切换。
`reasoning_content` / `thinking_delta` 会进入 stream events；agent loop 会把最近几步的
reasoning 摘要和 assistant message 一起回放到后续请求，TUI runtime stream 也会把
reasoning delta 保存为 durable `reasoning` item。默认仍保持 `off`，直到 provider-native
reasoning transcript replay 和更完整的 thinking/tool-call 兼容性验证完成。

`deepseek` 每次任务开始前也会读取 workspace instruction 文件。团队共享规则可放在 repo root 或子目录的
`AGENTS.md`；已有 Claude Code 项目也可继续用 `CLAUDE.md` 或 `.claude/CLAUDE.md`，DeepseekCode 会在同一目录没有
`AGENTS*.md` 时把它们作为 fallback。个人默认指令文件是 `~/.config/dscode/AGENTS.md`，可通过
`workspace.user_instructions_file` 改路径或设为空字符串禁用。

可选 hooks 需要显式启用，默认关闭，避免 clone 下来的仓库自动执行脚本。启用后可在
`.dscode/hooks/session_start/`、`.dscode/hooks/user_prompt_submit/`、`.dscode/hooks/pre_tool_use/`、
`.dscode/hooks/permission_request/`、`.dscode/hooks/post_tool_use/`、`.dscode/hooks/subagent_start/`、
`.dscode/hooks/subagent_stop/`、`.dscode/hooks/session_stop/`、`.dscode/hooks/pre_compact/`、
`.dscode/hooks/shell_env/`
放置可执行脚本；脚本通过 stdin 接收 JSON payload。`user_prompt_submit` / `pre_tool_use` /
`permission_request` 非零退出会阻断，其他 hook 非零退出只会作为 advisory observation 返回给 agent。
`shell_env` 会在 `run_shell` / `exec_shell` / `task_shell_start` 前运行，把 stdout 中的
`KEY=VALUE` / `export KEY=VALUE` 行注入该次 shell 环境；回传给模型的只包含 key 名，不包含值。

MCP server 配置可放在项目级 `.dscode/mcp.json` 或用户级 `~/.config/dscode/mcp.json`。当前版本支持配置发现、校验，以及 stdio / HTTP / SSE server 的手动 tool 和 prompt discovery / invocation：

```bash
deepseek mcp init
deepseek mcp add-self [--name deepseek] [--workspace /path/to/workspace]
deepseek mcp add <server-name> --command <cmd> [--arg <arg>]...
deepseek mcp add <server-name> --url http://localhost:3000/mcp
deepseek mcp get <server-name>
deepseek mcp enable <server-name>
deepseek mcp disable <server-name>
deepseek mcp remove <server-name>
deepseek mcp validate
deepseek mcp list
deepseek mcp doctor
deepseek mcp tools [server-name]
deepseek mcp call <server-name> <tool-name> '{"arg":"value"}'
deepseek mcp prompts [server-name]
deepseek mcp prompt <server-name> <prompt-name> '{"arg":"value"}'
deepseek mcp resources [server-name]
deepseek mcp resource-templates [server-name]
deepseek mcp resource <server-name> <resource-uri>
```

配置格式兼容常见 `mcpServers` 对象，例如：

```json
{
  "mcpServers": {
    "example-filesystem": {
      "disabled": true,
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    }
  }
}
```

`deepseek mcp add-self` 会把当前 `deepseek` binary 注册成一个 stdio MCP
server。默认写入用户级 `~/.config/dscode/mcp.json`，server 名为
`deepseek`；加 `--project` 可写入当前项目级 `.dscode/mcp.json`，加
`--workspace <PATH>` 会让生成的 server entry 以该目录运行
`deepseek serve --mcp --workspace <PATH>`。`serve --mcp` 暴露只读 workspace /
runtime tools、runtime resources，以及 `review_code`、`explain_code`、
`plan_task` 这几个内置 prompt templates。需要 trusted MCP client 直通调用
`run_shell` 时，可在 MCP server 环境里设置
`DSCODE_MCP_ENABLE_SIDE_EFFECTS=1`；需要把 `run_shell` 接入 durable approval
flow 时，可设置 `DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1`，或用
`DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id>` 绑定已有 runtime thread。trusted
direct 模式只暴露现有 `run_shell` allowlist 允许的命令；durable approval 模式
还会暴露 `apply_patch`，但每次写入都会先记录 `permission_request kind=write`
并等待 approval，且继续复用 patch scope validation，不开放任意 shell 或不受限
文件写入。

`deepseek mcp add` 默认也写入用户级 config；加 `--project` 可写项目级
config。stdio server 使用 `--command` 和重复 `--arg`，HTTP/SSE server 使用
`--url` 和可选 `--transport http|sse`。`--env KEY=VALUE` 和
`--header KEY=VALUE` 可重复；`--disabled` 会创建但不启用该 server。

`deepseek mcp tools` 会按 MCP lifecycle 对 stdio server、HTTP MCP endpoint 或旧式 SSE server 执行 `initialize` / `notifications/initialized` / `tools/list`，并展示返回的 tool name、description 和 input schema。
`deepseek mcp call` 会显式执行 `tools/call`，参数必须是 JSON object；返回会显示 text content、structuredContent 和 tool-level error flag。HTTP transport 通过 JSON-RPC POST 调用，并会续传服务端返回的 `Mcp-Session-Id`；SSE transport 会先读取 `endpoint` 事件，再向 endpoint POST JSON-RPC 并从 SSE stream 匹配 response。
`deepseek mcp prompts` / `deepseek mcp prompt` 对同一批 transport 执行 `prompts/list` / `prompts/get`；在 REPL 中也可以用 `/mcp/<server>/<prompt> [json]` 或 Claude 风格 `/mcp__server__prompt [json]` 把 MCP prompt 作为下一轮用户输入提交。
`deepseek mcp resources` / `deepseek mcp resource` 对 stdio / HTTP / SSE MCP
server 执行 `resources/list` / `resources/read`，用于读取远端 server 暴露的
只读资源内容。`deepseek mcp resource-templates` 执行
`resources/templates/list`，用于查看可参数化的 resource URI 模板。
在本地 file-backed `deepseek tui` 中，命令面板的 `mcp` / `mcp manager`
会打开 full-width MCP manager screen，展示 merged inventory、config
sources 和常用操作；`mcp manager tools|prompts|resources|resource-templates [server-name]`
会把发现结果渲染到 full-width manager screen；短命令
`mcp tools|prompts|resources|resource-templates [server-name]` 继续使用可滚动的
右侧详情面板；`Esc` 或 `mcp close` 可回到主 workbench。
TUI 中未带 scope 的 `mcp add/enable/disable/remove` 写项目级 config；
`mcp user add/enable/disable/remove ...` 写用户级 config。
`deepseek mcp validate` 和 TUI `mcp validate` 会对 enabled servers 做
tools 硬验证，并汇总 prompts/resources/resource-templates 的可用性和数量。

当 project/user MCP config 文件存在时，agent 运行时会暴露通用 bridge tools：
`mcp_list_tools`、`mcp_call`、`mcp_list_prompts`、`mcp_get_prompt`、
`mcp_list_resources`、`mcp_read_resource` 和
`mcp_list_resource_templates`。这使模型可以先枚举 MCP server
tools/prompts/resources/templates，再用 JSON object arguments 调用 stdio /
HTTP / SSE MCP tools、读取只读 resources，或获取远端 prompt messages。

如果你信任配置里的 MCP servers，可设置 `mcp.expose_remote_tools = true`。开启后，agent 启动时会发现 enabled MCP server tools，并以 `mcp__server__tool` 形式注入为独立 agent tool；能安全表示的远端 `inputSchema` 会注入为一等参数，无法表示时才回退到 `arguments` JSON object string。

agent 侧的 `mcp_call` 和动态 MCP tools 默认受 `approval.require_mcp_confirmation = true` 保护；非交互运行可用 `DSCODE_AUTO_APPROVE_MCP=1` 放行。确认提示会显示 server/tool 和参数摘要。还可以用 `approval.mcp_call_allowlist = ["server/tool", "server/*", "*/tool"]` 限制 agent 能调用的远端 MCP tool；空数组表示不限制。`mcp_list_tools`、`mcp_list_prompts`、`mcp_get_prompt`、`mcp_list_resources`、`mcp_read_resource` 和 `mcp_list_resource_templates` 是只读发现/读取，不要求确认；用户显式执行的 `deepseek mcp call ...` 也不会再次弹出 agent 审批。

如果要做一次最小 live 请求验证：

```bash
deepseek smoke
```

如果要给本地 supervisor、editor integration 或 CI 读取机器可解析健康状态：

```bash
deepseek doctor --json
```

JSON 模式只读取本地配置、capability、skills、MCP 路径和必要 binary 状态；不会执行 live network probe。

如果要试用本地 runtime skeleton：

```bash
deepseek serve --http
curl http://127.0.0.1:8765/health
curl http://127.0.0.1:8765/runtime
```

`serve --http` 当前公开 health、runtime metadata、file-backed sessions、threads、turn records、task metadata records、automation metadata records、JSON/SSE event replay、token/cache/cost usage records、usage summary / 1M-context policy 和 non-destructive thread compaction endpoint；
并支持 active automation 手动 trigger 成 pending task、pending task 被外部 runner claim 成 running；本地后台执行可用 `deepseek agents daemon` 轮询同一 runtime store，并对超过 800k latest-context tokens 的 thread 做 non-destructive compaction。systemd/launchd runtime、agents daemon、diagnostics watch 和 shell-supervisor protocol bridge 文件可由 `deepseek agents service` 渲染。schema 草案见
[`docs/runtime.md`](./runtime.md)。

## 基本用法

- `deepseek`：直接进入交互模式
- `deepseek "task"` 或 `deepseek run "task"`：执行单次任务
- `deepseek tui [--demo]`：启动 ratatui/crossterm 全屏 workbench shell；`--once` 可输出 CI 快照；command palette 支持 `mcp` full-width manager screen 和项目级 `mcp init/add/enable/disable/remove/validate`
- `deepseek benchmark`：跑本地 benchmark 基线
- `deepseek dogfood ...`：记录或回放真实任务
- `deepseek update`：打印 source checkout 安装命令和 release package/verify 提示
- `deepseek update package`：生成本地 release package（binary、manifest、install/rollback scripts）
- `deepseek update verify-install`：在隔离目录验证 version/config/doctor/exec JSONL/benchmark sample
- `deepseek update install-package` / `deepseek update rollback`：安装本地 release package 或回滚到备份 binary
- `deepseek update publish-status [--dist ... --npm-dist ... --strict --json]`：检查 npm/Homebrew 发布所需 token、tap 配置、平台包和 release checksum
- `deepseek pr live-status <pr> [--require-write --json]`：只读检查真实 GitHub PR 是否具备 live review/retry fixture 前置条件
- `deepseek config network allow|deny <host>`：把网络 host 策略写回项目 `.dscode/config.toml`，用于持久化 web/search/fetch 的允许或拒绝规则
- `deepseek agents run-task <task-id>`：认领并执行 pending durable runtime task，写回同一 thread 的 turns/items/usage/status
- `deepseek agents daemon [--interval-ms 1000] [--budget N]`：本地轮询 `.dscode/runtime`，触发到期 automation、执行 thread-linked pending task，并自动追加 non-destructive compaction summary
- `deepseek diagnostics [--changed] [--json] [paths...]` / `deepseek diagnostics --watch --json ...`：运行本地语言诊断；watch 模式会在同一进程内复用 warmed stdio LSP session，失败时回退到 compiler/type-check checker；JSON 模式输出 `deepseek.diagnostics.report.v1` 或 newline-delimited `deepseek.diagnostics.daemon_tick.v1`；`deepseek agents service` 可为 `diagnostics --watch --changed --json` 和 `agents shell-supervisor --json` 生成常驻 worker 模板
- `deepseek restore snapshot [label]` / `list` / `show <id>` / `revert-turn <id> [--apply]`：管理 rollback snapshots（tracked diff + untracked files、目录 metadata、Unix special files）
- `deepseek serve --http`：启动本地 runtime skeleton，提供 `/health` 与 `/runtime`
- `deepseek mcp init|add|add-self|get|remove|enable|disable|validate|list|doctor|tools|prompts|resources|resource-templates|call|prompt|resource`：管理、校验、枚举或手动调用 MCP server tools/prompts/resources
- `deepseek completion bash|zsh|fish`：生成 shell completion 脚本

## Shell Completion

```bash
mkdir -p ~/.local/share/bash-completion/completions
deepseek completion bash > ~/.local/share/bash-completion/completions/deepseek
```

```bash
mkdir -p ~/.zfunc
deepseek completion zsh > ~/.zfunc/_deepseek
```

```bash
mkdir -p ~/.config/fish/completions
deepseek completion fish > ~/.config/fish/completions/deepseek.fish
```

`dscode` 仍可作为兼容别名使用，但主文档和主命令统一为 `deepseek`。
