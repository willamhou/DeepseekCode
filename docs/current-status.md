# DeepSeekCode 当前状态与后续路线

最后更新：2026-05-18

## 最终目标

DeepSeekCode 的目标是成为一个 DeepSeek-first 的 code agent CLI：用户在终端里运行
`deepseek` 后，可以像使用 Claude Code CLI、Codex CLI 或 DeepSeek-TUI 一样，完成真实
仓库里的读代码、改代码、跑命令、查看 diff、继续修复、恢复会话和发布前验证。

最终验收口径不是“功能列表看起来很多”，而是：

- 裸 `deepseek` 在真实 TTY 里稳定进入 coding-agent TUI；
- 模型能稳定调用文件、shell、patch、diagnostics、review、runtime、subagent、MCP/ACP 等工具；
- 一个真实代码任务可以从需求进入、修改文件、跑测试、修失败、输出 diff 和总结；
- shell/PTY、审批、回滚、secret scan、dogfood 和 CI gate 都能证明行为可靠；
- 安装分发覆盖 GitHub Release、GHCR、npm、Homebrew，并且 README 能让新用户快速理解和试用；
- 和 Claude Code CLI / Codex CLI / DeepSeek-TUI 的核心使用差距收敛到 5% 以内。

## 当前做到哪里

当前项目已经不是早期原型，可以直接用于仓库内 dogfood 和中小型代码任务。当前主入口是
`deepseek`，历史兼容入口 `dscode` 仍保留。

已经具备的核心能力：

- 终端入口：`deepseek`、`deepseek chat`、`deepseek run`、`deepseek tui`、`deepseek exec`。
- TUI：全屏 workbench、Plan / Agent / YOLO 模式、approval modal、command palette、session/thread 视图、MCP 管理、setup/onboarding、provider/model picker。
- Runtime：`.dscode/runtime/` 下持久化 sessions、threads、turns、items、events、tasks、usage、automations。
- 工具：文件读取/搜索、patch、diff、shell、background jobs、diagnostics、review、notes、memory、rollback、skills、subagents。
- 模型协议：OpenAI-compatible tool calls，同轮 batch tool calls，DeepSeek provider/model alias 兼容。
- 审批与安全：approve-once、approve-for-session、deny fingerprint、secret scan、shell/network policy、rollback snapshot。
- Shell/PTY：后台 shell job、wait/replay/attach/stdin/resize/cancel；Linux native-supervisor PTY；workspace shell-supervisor protocol bridge。
- 本轮新增：`deepseek agents shell attach <task_id> --interactive` / `--takeover`。它会进入本地 raw mode，把按键转发到 supervisor `stdin`，把 resize 转发到 supervisor `resize`，并把 stdout replay 回当前终端。它是可用的 bounded interactive attach，不是字节级 PTY fd 直连代理。
- 发布面：`v0.1.1` GitHub Release binaries、GHCR image、npm/Homebrew packaging metadata、release matrix、download-plan、publish-status、README 多语言、README TUI demo recorder。
- CI 证据：Linux/macOS/Windows bare `deepseek` TUI entrypoint smoke 已经在 CI 里通过；Windows 路径使用 ConPTY-backed smoke。

当前可以怎么用：

```bash
deepseek
deepseek chat
deepseek run "explain this repository"
deepseek tui --entrypoint-smoke --smoke-bin "$(command -v deepseek)"
deepseek agents service-smoke --workdir /tmp/dsc-smk --bin "$(command -v deepseek)" --json
```

真实模型调用需要配置 DeepSeek API key。不要把 key 写进仓库；推荐使用环境变量或仓库外文件。

## 还差什么

当前距离 Claude Code CLI / Codex CLI / DeepSeek-TUI 的成熟产品形态，主要差在以下几类：

1. Shell/PTY 深水区
   - 已有 bounded interactive attach，但还不是 byte-level PTY fd proxy。
   - Linux shell-supervisor native PTY 已有，Windows shell-supervisor ConPTY 仍需要单独实现和 CI 证明。
   - systemd/launchd 的真实安装后 service smoke 还需要外部环境证据。

2. 真实模型 dogfood 证据
   - 已有 recorder、verifier、redaction self-test 和 release gate。
   - 还缺足够多真实 disposable repo 上的 model-backed live write-fixture 样本。
   - README 目前的主 demo 是确定性 TUI SVG，还缺 review 后提交的真实 model-backed 短录屏/GIF/SVG。

3. 发布渠道
   - GitHub Release 和 GHCR 已通。
   - npm registry 和 Homebrew tap 还被凭据阻塞。
   - crates.io 是否发布仍需要明确 crate 命名、license/package policy。

4. 产品打磨
   - TUI 已能用，但还需要更多真实工作流下的性能、长输出、失败恢复、窗口 resize、旧终端兼容性验证。
   - 文档需要继续压缩成新用户能快速理解的安装、配置、试用、故障排查路径。
   - 和上游 DeepSeek-TUI 的新变化需要持续周期性 refresh。

## 下一步优先级

建议按这个顺序推进，避免在低价值 polish 上分散：

1. 完成并验证本轮 `--interactive` attach
   - 保留当前 parser、raw-key mapping、stdin/resize/replay loop。
   - 增加一个可在 CI 或非沙盒环境运行的 end-to-end smoke：启动 shell-supervisor，创建 `tty=true` job，使用 `attach --interactive --max-ms` 输入一段文本，确认 replay 可见并 clean detach。
   - 明确文档口径：这是 bounded interactive attach，不宣称 byte-perfect PTY。

2. 做 byte-level PTY proxy 设计
   - 为 Linux native-supervisor 暴露更直接的 PTY stream / attach channel。
   - 明确 resize、stdin、output、detach、EOF、Ctrl-C、Ctrl-D、SIGWINCH 的协议。
   - 设计 Windows shell-supervisor ConPTY 的等价实现。

3. 跑真实 model-backed dogfood
   - 先轮换任何已经泄漏到聊天记录里的 key。
   - 用仓库外 key 文件，例如 `/tmp/deepseek-demo.key`，执行 model-backed demo recorder。
   - 至少准备 3 到 5 个 disposable repo/write-fixture 样本，跑出可复核的 failure -> edit -> test -> diff 证据。

4. 补 README 真实录屏
   - 录制真实模型循环：打开 TUI、提交编码任务、应用修改、运行测试、查看 diff。
   - 用 verifier 拦截离线 rehearsal 和 secret 泄漏。
   - 产物放在 `docs/demo/`，README 引用 review 后的 media。

5. 完成发布渠道
   - 配置 npm token 并发布 npm wrapper。
   - 配置 Homebrew tap token 并发布 formula。
   - 决定 crates.io 是否进入 v0.2 目标。

6. 最后一轮差距审计
   - 重新拉取 DeepSeek-TUI 最新 main。
   - 和 Claude Code CLI / Codex CLI 的核心 loop 对照：入口、TUI、tool use、approval、shell、resume、diff、release、docs。
   - 只保留会影响真实用户使用的差距，目标是核心差距低于 5%。

## 当前判断

DeepSeekCode 现在已经是一个可以实际使用的 code agent CLI，尤其适合在本仓库继续 dogfood。
但它还不是“可以公开宣称等同 Claude Code CLI / Codex CLI”的成熟产品。

最准确的公开表述是：

> DeepSeekCode is usable today for dogfooding and repository work, with a full-screen TUI, durable runtime, permissioned tools, release binaries, and cross-platform entrypoint smoke. The remaining work is deeper PTY proxying, model-backed dogfood depth, and public package-channel publishing.
