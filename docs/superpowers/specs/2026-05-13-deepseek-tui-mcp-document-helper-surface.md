# DeepSeek-TUI MCP Document Helper Surface

Date: 2026-05-13

Status: completed

## Gap

`pandoc_convert` and `image_ocr` were already agent-visible DeepSeek-TUI
compatibility helpers and documented in the MCP tool table, but `serve --mcp`
and ACP's inherited tool bridge did not actually expose them.

## Spec

1. Expose `image_ocr` through MCP/ACP as a local read-only helper that invokes
   `tesseract` only after validating a workspace-relative image path.
2. Expose `pandoc_convert` through MCP/ACP for inline text conversion.
3. Treat `pandoc_convert output_path=...` as write mode in MCP/ACP: require
   durable runtime approvals before invoking `pandoc`, and reject it in default
   read-only sessions.
4. Keep `image_analyze` out of this slice because it calls an external
   model/vision API and needs a separate MCP model-token/network contract.
5. Add focused tests that validate MCP list/ACP list visibility and pre-spawn
   validation paths that do not require local `pandoc` or `tesseract`.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_handles_document_helpers_before_spawning_binaries --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes_workspace_and_runtime_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_list_new_session_is_read_only --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_ --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_ --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- MCP `tools/list` now advertises `pandoc_convert` and `image_ocr`.
- MCP `tools/call` dispatches `image_ocr` to the existing local OCR tool.
- MCP `tools/call pandoc_convert` runs inline conversions directly, but routes
  `output_path` calls through durable write approval before executing.
- ACP inherits both tools through the session-scoped MCP state adapter.
