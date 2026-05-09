# 安装

`deepseek` 是默认命令名。推荐先安装，再用 `deepseek version` 和 `deepseek doctor` 做最小验证。

## 从源码安装

```bash
cargo install --path .
deepseek version
deepseek doctor
```

如果你只想先本地构建 release binary：

```bash
cargo build --release
./target/release/deepseek version
```

## 发布前检查

本仓库的最小 release gate 是：

```bash
cargo fmt --check
cargo test
deepseek benchmark
deepseek version
deepseek doctor
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

## Release Binary

本地 release binary 路径固定为：

```bash
cargo build --release
./target/release/deepseek version
```

发布产物至少应包含：

- `deepseek` binary
- 对应 commit SHA
- `deepseek version` 输出
- 支持的平台说明
- 安装与升级说明链接

`dscode` 只作为兼容别名，不作为主 release artifact 名称。

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
mkdir -p ~/.local/bin/deepseek-rollback
cp "$(command -v deepseek)" ~/.local/bin/deepseek-rollback/deepseek.previous
```

然后替换 binary，并验证：

```bash
deepseek version
deepseek doctor
```

配置文件和会话默认保存在 `.dscode/`，升级 binary 不应删除这些文件。

## 回滚

如果升级后需要回滚 release binary：

```bash
cp ~/.local/bin/deepseek-rollback/deepseek.previous "$(command -v deepseek)"
deepseek version
deepseek doctor
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

`deepseek config init` 会创建项目级 `.dscode/config.toml`、session 目录、custom command 目录和 hooks 事件目录。
如果确实要覆盖已有配置，可以显式运行 `deepseek config init --force`。
它也会创建 `.dscode/mcp.json`，用于记录项目级 MCP server 定义。

`deepseek` 会自动读取当前工作目录下的 `.env`，并在变量尚未存在于进程环境时注入。常用 DeepSeek/OpenAI-compatible 配置：

```bash
DEEPSEEK_API_KEY=...
DEEPSEEK_BASE_URL=https://api.deepseek.com
DEEPSEEK_MODEL=deepseek-coder
```

如果 `.env` 或 shell 环境里设置了 `DEEPSEEK_BASE_URL` / `DEEPSEEK_MODEL`，它们会覆盖 `.dscode/config.toml` 里的 `model.base_url` / `model.model`。

`deepseek` 每次任务开始前也会读取 workspace instruction 文件。团队共享规则可放在 repo root 或子目录的
`AGENTS.md`；已有 Claude Code 项目也可继续用 `CLAUDE.md` 或 `.claude/CLAUDE.md`，DeepseekCode 会在同一目录没有
`AGENTS*.md` 时把它们作为 fallback。个人默认指令文件是 `~/.config/dscode/AGENTS.md`，可通过
`workspace.user_instructions_file` 改路径或设为空字符串禁用。

可选 hooks 需要显式启用，默认关闭，避免 clone 下来的仓库自动执行脚本。启用后可在
`.dscode/hooks/user_prompt_submit/`、`.dscode/hooks/pre_tool_use/`、`.dscode/hooks/post_tool_use/`
放置可执行脚本；脚本通过 stdin 接收 JSON payload。`user_prompt_submit` / `pre_tool_use` 非零退出会阻断，
`post_tool_use` 非零退出只会作为 advisory observation 返回给 agent。

MCP server 配置可放在项目级 `.dscode/mcp.json` 或用户级 `~/.config/dscode/mcp.json`。当前版本支持配置发现、校验，以及 stdio server 的手动 tool discovery / invocation：

```bash
deepseek mcp init
deepseek mcp list
deepseek mcp doctor
deepseek mcp tools [server-name]
deepseek mcp call <server-name> <tool-name> '{"arg":"value"}'
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

`deepseek mcp tools` 会按 MCP lifecycle 启动 stdio server，执行 `initialize` / `notifications/initialized` / `tools/list`，并展示返回的 tool name、description 和 input schema。
`deepseek mcp call` 会显式执行 `tools/call`，参数必须是 JSON object；返回会显示 text content、structuredContent 和 tool-level error flag。

当 project/user MCP config 文件存在时，agent 运行时会暴露两个通用 bridge tools：`mcp_list_tools` 和 `mcp_call`。这使模型可以先枚举 MCP server tools，再用 JSON object arguments 调用 stdio MCP tools。

agent 侧的 `mcp_call` 默认受 `approval.require_mcp_confirmation = true` 保护；非交互运行可用 `DSCODE_AUTO_APPROVE_MCP=1` 放行。`mcp_list_tools` 只是只读发现，不要求确认；用户显式执行的 `deepseek mcp call ...` 也不会再次弹出 agent 审批。

这一版还不会把每个远端 MCP tool 动态注入为独立 agent tool；HTTP/SSE transport 和按 server/tool 的细粒度 MCP policy 仍是后续工作。

如果要做一次最小 live 请求验证：

```bash
deepseek smoke
```

## 基本用法

- `deepseek`：直接进入交互模式
- `deepseek "task"` 或 `deepseek run "task"`：执行单次任务
- `deepseek benchmark`：跑本地 benchmark 基线
- `deepseek dogfood ...`：记录或回放真实任务
- `deepseek mcp list|doctor|tools|call`：查看、校验、枚举或手动调用 MCP server tools
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
