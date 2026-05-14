# DeepSeek-TUI Web Search/Fetch Parity Spec

日期：2026-05-13

对比对象：`Hmbown/DeepSeek-TUI`，`main` HEAD `3382242`

## 背景

DeepSeek-TUI exposes `web_search`, `fetch_url`, and a richer `web.run` browsing
surface. Its base prompt explicitly tells agents to prefer these structured
network tools over shelling out to `curl`.

DeepSeekCode already had local search tools, but no agent-visible network read
tools, so prompts or model plans that call `web_search` / `fetch_url` failed at
the registry/schema layer.

## 目标

- Add `web_search` as an agent-visible read-only network tool.
- Add `fetch_url` as an agent-visible read-only network tool.
- Support DeepSeek-TUI-compatible `web_search` inputs: `query`, `q`, and
  JSON-string `search_query` array with `q` / `max_results`.
- Support `fetch_url` inputs: `url`, `format`, `max_bytes`, and `timeout_ms`.
- Expose both tools through the default registry, model schemas, MCP tools, and
  ACP session tools.
- Keep the implementation testable without live internet access.

## 非目标

- This slice does not implement DeepSeek-TUI's full `web.run`
  search/open/click/find/screenshot state machine.
- This slice does not add Tavily/Bocha provider configuration.
- This slice does not add durable web result refs across process exits.

## 验收标准

1. `fetch_url` fetches HTTP/HTTPS URLs and returns status, content type,
   truncation metadata, and decoded content.
2. `web_search` returns ranked URLs/snippets from a configured local test
   search endpoint and supports `search_query` array compatibility input.
3. Registry/model/MCP/ACP tool lists include `web_search` and `fetch_url`.
4. localhost/private network targets are blocked by default and only allowed
   when `DSCODE_ALLOW_LOCAL_FETCH=1`.

## 实现结果

- `src/tools/web.rs` adds `WebSearchTool` and `FetchUrlTool`.
- `fetch_url` supports direct HTTP with the standard library and HTTPS through
  `curl`, with bounded output and basic HTML-to-text extraction.
- `web_search` uses Bing HTML search by default, keeps DuckDuckGo available via
  `DSCODE_WEB_SEARCH_PROVIDER=duckduckgo`, and supports
  `DSCODE_WEB_SEARCH_URL_TEMPLATE` for deterministic local tests.
- `src/tools/registry.rs`, `src/model/deepseek.rs`, and
  `src/cli/commands/serve.rs` expose both tools to agent/MCP/ACP flows.
- `docs/runtime.md` documents inputs and localhost/private host blocking.

## 验证

- `cargo test web_search`
- `cargo test fetch_url`
- `cargo test build_tool_specs_include_web_search_and_fetch_url`
- `cargo test default_registry_includes_read_only_git_history_tools`
- `cargo test mcp_tools_list_includes_workspace_and_runtime_tools`
- `cargo test acp_session_tools_list_new_session_is_read_only`
- `cargo fmt --check`
- `git diff --check`
- `cargo test`（914 passed）
- `cargo package --allow-dirty`
