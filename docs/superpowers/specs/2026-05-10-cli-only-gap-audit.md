# Claude/Codex CLI-Only Gap Audit

最后更新：`2026-05-10`
状态：`post CLI-12 parallel subagent audit`

## Scope

本审计只对比：

- Claude Code CLI
- Codex CLI
- `DeepseekCode` CLI

明确不计入：

- IDE extension
- Codex app / cloud tasks
- browser / computer-use app surface
- GitHub Action / hosted automation
- Slack / Linear / other product integrations

因此，本文件会覆盖比完整产品面更窄的 gap。完整产品面对齐仍见：

- `docs/superpowers/specs/2026-05-10-claude-codex-gap-audit-v2.md`
- `docs/superpowers/plans/2026-05-10-claude-codex-gap-closure-v2.md`

## Conclusion

CLI-only 口径下，CLI-12 后续补齐 live JSONL、subagent baseline、parallel subagent/thread management、MCP prompt slash support、native image payload support、REPL compaction/pre_compact wiring 与 release package/install verifier 后的当前差距估计：

- 相对 Claude Code CLI：`6% - 9%`
- 相对 Codex CLI：`6% - 8%`
- 综合 CLI-only residual gap：`6% - 9%`

这比完整产品面的 `38% - 42%` 小很多。原因是 `DeepseekCode` 的强项正好集中在 CLI local coding loop：读代码、搜索、patch、shell、diff、REPL、resume、workspace instructions、hooks、MCP、subagent、benchmark/dogfood。

但 CLI-only 仍不能标记完成。CLI-12 已补上 `exec` live JSONL/stdin/resume、自定义 subagent 管理、parallel `dispatch_subagents`、subagent thread artifacts/switching、20 个 subagent benchmark cases、扩展 hooks、REPL `/compact` + `pre_compact`、动态 MCP schema 注入、MCP prompt slash commands、OpenAI/Anthropic native image payloads、`deepseek update package/install-package/rollback/verify-install`。剩余差距主要来自：在线 dogfood 样本厚度还不足以把稳定性证据打到目标要求。

## Official Baseline

只使用 CLI 相关官方资料作为对照：

- Claude Code overview: `https://docs.anthropic.com/en/docs/claude-code/overview`
- Claude Code CLI reference: `https://docs.anthropic.com/en/docs/claude-code/cli-reference`
- Claude Code memory: `https://docs.anthropic.com/en/docs/claude-code/memory`
- Claude Code hooks: `https://docs.anthropic.com/en/docs/claude-code/hooks`
- Claude Code MCP: `https://docs.anthropic.com/en/docs/claude-code/mcp`
- Claude Code subagents: `https://docs.anthropic.com/en/docs/claude-code/sub-agents`
- OpenAI Codex CLI features: `https://developers.openai.com/codex/cli/features`
- OpenAI Codex CLI reference: `https://developers.openai.com/codex/cli/reference`
- OpenAI Codex hooks: `https://developers.openai.com/codex/hooks`
- OpenAI Codex MCP: `https://developers.openai.com/codex/mcp`
- OpenAI Codex subagents: `https://developers.openai.com/codex/subagents`
- OpenAI Codex AGENTS.md: `https://developers.openai.com/codex/guides/agents-md`

## Current Repo Evidence

Current verified CLI state:

- CLI commands in `src/cli/app.rs`: `chat/repl/interactive`, `run`, `exec`, `agents`, `benchmark`, `dogfood`, `diff`, `resume`, `config`, `doctor`, `smoke`, `pr`, `mcp`, `completion`, `update`, `version`
- REPL and slash docs in `docs/repl.md`
- Scriptable exec docs in `docs/exec.md`
- Custom subagent docs in `docs/agents.md`
- Workspace instructions in `src/core/instructions.rs`
- Hooks implementation in `src/core/hooks.rs`
- REPL transcript compaction in `src/repl/transcript.rs` and `/compact` in `src/repl/slash.rs`
- MCP CLI and transports in `src/cli/commands/mcp.rs`
- Dynamic MCP tools in `src/tools/mcp.rs` and `src/tools/registry.rs`
- Native `exec --image` payload construction in `src/model/deepseek.rs`
- Single and parallel subagent dispatch in `src/tools/dispatch_subagent.rs`
- Subagent thread listing/switching in `src/cli/commands/agents.rs`
- Release package/install/rollback/verifier flow in `src/cli/commands/update.rs`
- Benchmark manifest `.dscode/benchmarks.txt`: 67 cases, including 20 `subagent` category cases
- Dogfood ledger `.dscode/dogfood/latest.md`: 33 runs

Current verification:

- `/home/willamhou/.cargo/bin/cargo test --offline`: `611 passed`
- `/home/willamhou/codes/DeepseekCode/target/debug/deepseek benchmark --manifest /tmp/deepseek-cli-gap-benchmark/subagent-benchmarks.txt --out /tmp/deepseek-cli-gap-benchmark/subagent-report.md`: `20/20` subagent cases
- Previous pre-expansion default offline benchmark: `49/49`
- Benchmark live gate: `pass`
- Benchmark trend gate: warmup skip after adding the 49th case

## CLI-Only Gap Table

| Dimension | Weight | Current Score | CLI gap | Evidence | Missing to reach <10% |
|---|---:|---:|---:|---|---|
| Interactive / scriptable CLI UX | 18 | 17 | 6% | REPL, `run`, `exec`, stdin, live JSONL, resume follow-up, diff, cost/todos | Richer approval overlay |
| Core local coding loop | 20 | 17 | 15% | read/search/patch/shell/diff, validation recovery, `49/49` benchmark | Higher live dogfood success, fewer heuristic-only paths, online model stability |
| Subagent orchestration | 13 | 13 | 0% | bounded dispatch, parallel `dispatch_subagents`, custom `.dscode/agents/*.md`, `deepseek agents list/show/validate`, child summaries, thread artifacts, `deepseek agents threads/show-thread/switch/current`, 20 subagent benchmark cases | More live multi-agent dogfood evidence |
| Hooks / policy events | 12 | 11 | 8% | prompt/session/tool/permission/subagent/pre-compact events, structured allow/deny/add_context, REPL `/compact` triggers `pre_compact` before transcript rewrite | More hook fixtures |
| MCP / tool schema UX | 14 | 13 | 7% | stdio/HTTP/SSE, manual call, bridge, opt-in dynamic tools, schema cache/injection, argument-aware prompts, `prompts/list` / `prompts/get`, REPL `/mcp/...` and `/mcp__...` prompt commands | Broader schema/prompt edge cases |
| Memory / rules / commands | 8 | 7 | 13% | AGENTS/CLAUDE loading, custom slash commands, skills, custom agent discovery | Better discovery/listing and validation UX |
| Model / context / multimodal CLI | 9 | 7 | 22% | DeepSeek-first, OpenAI/Anthropic-compatible transport paths, streaming parsers, capability reporting, `exec --image` file refs plus native OpenAI/Anthropic image payloads | dedicated Codex-class coding model path, larger reliable context, web/search cache story |
| Install / update / distribution | 6 | 6 | 0% | install docs, config init, version, completion, `deepseek update --check/--print-command`, `package`, `verify-install`, `install-package`, `rollback` | Hosted release publishing/signing remains out of CLI-only scope |
| **Total** | **100** | **91** | **9%** |  | Current CLI-only gap estimate: `6% - 9%` after rounding for benchmark strength and remaining live-stability risk |

## Six Concrete Gaps

### 1. Interactive Maturity

Current:

- `deepseek` enters the full-screen TUI in real TTYs; `deepseek chat` enters
  the line-oriented REPL.
- `deepseek run "task"` handles one-shot tasks.
- `/save`, `/load`, `/todos`, `/cost`, `/diff`, custom slash commands exist.

Gap:

- `exec --json` now streams `assistant_delta`, `tool_call`, `permission_request`, and `tool_result` events during execution.
- Approval UX is functional but not as mature as Codex/Claude TUI overlays.

### 2. Subagent Maturity

Current:

- `dispatch_subagent` can run bounded child loops.
- `dispatch_subagents` can run up to 4 independent child tasks concurrently and returns consolidated per-thread metadata.
- Child summary includes touched files and `meta.child_next_action`.
- Nested dispatch is bounded.
- Parallel dispatch writes `.dscode/agent-threads/*.md` artifacts.
- `deepseek agents threads`, `show-thread`, `switch`, `current`, and `clear-current` expose thread inspection/switching.

Gap:

- Custom project/user subagent files and `deepseek agents` management now exist.
- The benchmark manifest now includes 20 subagent-category cases, with a subagent-only verifier run passing `20/20`.
- Remaining gap is live multi-agent dogfood depth rather than the primary CLI/tool surface.

### 3. Hooks Event Surface

Current:

- `session_start`
- `session_stop`
- `user_prompt_submit`
- `pre_tool_use`
- `permission_request`
- `post_tool_use`
- `subagent_start`
- `subagent_stop`
- `pre_compact` runs before REPL `/compact` rewrites older transcript turns into a summary.
- Hook stdout supports structured `allow` / `deny` / `add_context` / `system_message`.

Gap:

- Hook benchmark fixtures remain thinner than the target state.

### 4. MCP / Tool Schema UX

Current:

- MCP config discovery exists.
- `tools/list` and `tools/call` work across stdio, HTTP, and legacy SSE.
- Agent has generic bridge tools and opt-in dynamic MCP tools.

Gap:

- Dynamic MCP tools now inject first-class remote schemas when representable, with an `arguments` wrapper fallback.
- Permission prompts now include remote server/tool and an argument summary.
- MCP prompts now have `deepseek mcp prompts [server]`, `deepseek mcp prompt <server> <prompt> [json-args]`, and REPL `/mcp/<server>/<prompt>` plus Claude-style `/mcp__server__prompt`.
- Remaining gap is broader schema/prompt edge-case coverage rather than the primary prompt-command surface.

### 5. Model And Context Capability

Current:

- DeepSeek-first transport exists.
- OpenAI-compatible and Anthropic-compatible parsing paths exist.
- Offline planner gives deterministic benchmark coverage.

Gap:

- Online model stability remains a product risk.
- No dedicated Codex-class coding model path.
- `exec --image` validates local image files and sends native payloads for recognized OpenAI/Anthropic vision-capable profiles; DeepSeek text-only profiles keep explicit file references.
- No built-in web/search/cache story comparable to Claude/Codex CLI workflows.

### 6. Install / Upgrade / Distribution

Current:

- `docs/install.md`
- `deepseek version`
- `deepseek config init`
- shell completion generation
- `deepseek update package` creates a local release directory with binary, `release.json`, install script, rollback script, and verifier instructions.
- `deepseek update verify-install` runs `version`, `config init --force`, `doctor`, `exec --json`, and a one-case benchmark in an isolated directory.
- `deepseek update install-package` backs up the current binary and installs a local release package.
- `deepseek update rollback` restores the backup binary.

Gap:

- Hosted release publishing, signing, and package registry distribution remain outside this local CLI-only closure.

## CLI-Only Target State

To claim CLI-only gap `<10%`, require all remaining items:

1. Dogfood has at least 100 CLI runs, with `recovery`, `write_validate`, and `pr_workflow` success rates all `>=90%`.

Until then, the implementation surface is in the high-single-digit gap range, but the completion claim remains blocked on live dogfood evidence.
