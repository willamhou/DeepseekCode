# Phase 9b Streaming SSE Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert `dscode` from one-shot LLM curl calls to live SSE streaming with TTY-aware ANSI rendering, matching Claude Code / Codex CLI UX.

**Architecture:** New generic SSE frame parser (`util/sse`) + `StreamEvents` trait with `TtyRenderer` impl (`ui/stream`). `ModelClient::respond` gains a `&mut dyn StreamEvents` parameter. DeepSeek client adds streaming OpenAI + Anthropic paths spawning `curl -N` and dispatching frame events. `AgentLoop` switches to renderer; `Repl` reuses unchanged.

**Tech Stack:** Rust 2021 (zero new deps), curl `-N` (no-buffer) via `std::process::Command`, hand-rolled SSE parser on `BufRead`.

**Spec:** `docs/superpowers/specs/2026-04-30-streaming-design.md`

**Baseline:** 175 tests passing, 0 warnings.

**Target:** 196 tests passing (+21), 0 warnings, 7 commits (M1–M7).

---

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/util/sse.rs` | **Create** | Generic SSE frame parser (`SseFrame`, `read_frame`) |
| `src/util/mod.rs` | Modify | Export `sse` module |
| `src/ui/stream.rs` | **Create** | `StreamEvents` trait, `NoopStreamEvents`, `TtyRenderer<W>` |
| `src/ui/mod.rs` | Modify | Export `stream` module |
| `src/model/client.rs` | Modify | `respond` adds `events: &mut dyn StreamEvents` parameter |
| `src/model/deepseek.rs` | Modify | Streaming OpenAI + Anthropic paths; SSE parsers consuming `BufRead` |
| `src/util/process.rs` | Modify | Add `StreamingProcess` + `spawn_streaming` helper |
| `src/core/loop_runtime.rs` | Modify | Use `TtyRenderer`, delete old `println!`-based step output |
| `src/repl/repl.rs` | (No change) | Existing `handle_line` reuses renamed `AgentLoop` API as-is |
| `docs/streaming.md` | **Create** | User-facing streaming docs |
| `docs/repl.md` | Modify | Drop "no streaming" limitation |
| `docs/roadmap.md` | Modify | Mark Phase 9b complete |

---

## Task M1: Generic SSE frame parser (`util/sse`)

**Files:**
- Create: `src/util/sse.rs`
- Modify: `src/util/mod.rs`

- [ ] **Step 1: Write failing test scaffold**

Create `src/util/sse.rs` with skeleton + tests only (compile error expected):

```rust
use std::io::BufRead;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

pub fn read_frame<R: BufRead>(_reader: &mut R) -> std::io::Result<Option<SseFrame>> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_single_data_frame() {
        let mut cur = Cursor::new(b"data: hello\n\n".to_vec());
        let frame = read_frame(&mut cur).unwrap().expect("frame");
        assert_eq!(frame.event, None);
        assert_eq!(frame.data, "hello");
    }

    #[test]
    fn concatenates_multiple_data_lines_with_newline() {
        let mut cur = Cursor::new(b"data: line1\ndata: line2\n\n".to_vec());
        let frame = read_frame(&mut cur).unwrap().expect("frame");
        assert_eq!(frame.data, "line1\nline2");
    }

    #[test]
    fn skips_comment_lines() {
        let mut cur = Cursor::new(b": this is a heartbeat\ndata: ok\n\n".to_vec());
        let frame = read_frame(&mut cur).unwrap().expect("frame");
        assert_eq!(frame.data, "ok");
    }

    #[test]
    fn captures_explicit_event_field() {
        let mut cur = Cursor::new(b"event: ping\ndata: 1\n\n".to_vec());
        let frame = read_frame(&mut cur).unwrap().expect("frame");
        assert_eq!(frame.event.as_deref(), Some("ping"));
        assert_eq!(frame.data, "1");
    }

    #[test]
    fn returns_partial_frame_at_eof_without_blank_line() {
        let mut cur = Cursor::new(b"data: trailing".to_vec());
        let frame = read_frame(&mut cur).unwrap().expect("frame");
        assert_eq!(frame.data, "trailing");
        // Next call returns None.
        let next = read_frame(&mut cur).unwrap();
        assert!(next.is_none());
    }
}
```

Modify `src/util/mod.rs`:

```rust
pub mod json;
pub mod process;
pub mod sse;
```

- [ ] **Step 2: Run to confirm failure**

Run: `~/.cargo/bin/cargo test --lib util::sse 2>&1 | tail -10`
Expected: FAIL with `unimplemented!()` panic on each test.

- [ ] **Step 3: Implement `read_frame`**

Replace the body of `read_frame` in `src/util/sse.rs`:

```rust
pub fn read_frame<R: BufRead>(reader: &mut R) -> std::io::Result<Option<SseFrame>> {
    let mut event: Option<String> = None;
    let mut data = String::new();
    let mut got_anything = false;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            if got_anything {
                return Ok(Some(SseFrame { event, data }));
            }
            return Ok(None);
        }

        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            if got_anything {
                return Ok(Some(SseFrame { event, data }));
            }
            continue;
        }
        if trimmed.starts_with(':') {
            continue;
        }

        let (field, value) = match trimmed.find(':') {
            Some(idx) => {
                let f = &trimmed[..idx];
                let mut v = &trimmed[idx + 1..];
                if let Some(stripped) = v.strip_prefix(' ') {
                    v = stripped;
                }
                (f, v)
            }
            None => (trimmed, ""),
        };

        match field {
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
                got_anything = true;
            }
            "event" => {
                event = Some(value.to_string());
                got_anything = true;
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 4: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test --lib util::sse 2>&1 | tail -10`
Expected: `test result: ok. 5 passed`

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 180 passed; 0 failed`

Run: `~/.cargo/bin/cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src/util/sse.rs src/util/mod.rs
git commit -m "feat(sse): add generic SSE frame parser

read_frame consumes any BufRead source, handles multi-data
concatenation, skips comments, captures explicit event field,
emits partial frame at EOF.

Tests: +5 (175 -> 180)"
```

---

## Task M2: StreamEvents trait + TtyRenderer (`ui/stream`)

**Files:**
- Create: `src/ui/stream.rs`
- Modify: `src/ui/mod.rs`

- [ ] **Step 1: Write failing test scaffold**

Create `src/ui/stream.rs` with skeleton + tests:

```rust
use std::collections::BTreeMap;
use std::io::Write;

pub trait StreamEvents {
    fn on_text_delta(&mut self, chunk: &str);
    fn on_assistant_done(&mut self, full_text: &str);
    fn on_tool_call(&mut self, name: &str, input: &BTreeMap<String, String>);
}

pub struct NoopStreamEvents;

impl StreamEvents for NoopStreamEvents {
    fn on_text_delta(&mut self, _chunk: &str) {}
    fn on_assistant_done(&mut self, _full_text: &str) {}
    fn on_tool_call(&mut self, _name: &str, _input: &BTreeMap<String, String>) {}
}

pub struct TtyRenderer<W: Write> {
    out: W,
    use_ansi: bool,
    text_started: bool,
}

impl<W: Write> TtyRenderer<W> {
    pub fn new_with(out: W, use_ansi: bool) -> Self {
        unimplemented!()
    }

    pub fn paint_step_divider(&mut self, _step_index: usize) {
        unimplemented!()
    }

    pub fn paint_tool_result(&mut self, _ok: bool, _label: &str, _body: &str) {
        unimplemented!()
    }
}

impl<W: Write> StreamEvents for TtyRenderer<W> {
    fn on_text_delta(&mut self, _chunk: &str) {
        unimplemented!()
    }
    fn on_assistant_done(&mut self, _full_text: &str) {
        unimplemented!()
    }
    fn on_tool_call(&mut self, _name: &str, _input: &BTreeMap<String, String>) {
        unimplemented!()
    }
}

pub fn abbreviate_for_inline(value: &str) -> String {
    let _ = value;
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(use_ansi: bool, mut act: impl FnMut(&mut TtyRenderer<&mut Vec<u8>>)) -> String {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut r = TtyRenderer::new_with(&mut buf, use_ansi);
            act(&mut r);
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn ansi_on_wraps_text_delta_in_cyan() {
        let out = render(true, |r| {
            r.on_text_delta("hello");
            r.on_assistant_done("hello");
        });
        assert!(out.starts_with("\x1b[36m"), "out: {out:?}");
        assert!(out.contains("hello"));
        assert!(out.contains("\x1b[0m"));
    }

    #[test]
    fn ansi_off_emits_plain_text() {
        let out = render(false, |r| {
            r.on_text_delta("hi");
            r.on_assistant_done("hi");
        });
        assert!(!out.contains("\x1b["), "out: {out:?}");
        assert!(out.contains("hi"));
    }

    #[test]
    fn tool_call_formats_args_inline() {
        let out = render(false, |r| {
            let mut args = BTreeMap::new();
            args.insert("path".to_string(), "src/main.rs".to_string());
            args.insert("max_lines".to_string(), "40".to_string());
            r.on_tool_call("read_file", &args);
        });
        assert!(out.contains("read_file"));
        assert!(out.contains("max_lines=40"));
        assert!(out.contains("path=src/main.rs"));
    }

    #[test]
    fn tool_call_ansi_uses_yellow() {
        let out = render(true, |r| {
            let args = BTreeMap::new();
            r.on_tool_call("git_diff", &args);
        });
        assert!(out.contains("\x1b[33m"), "out: {out:?}");
    }

    #[test]
    fn step_divider_dim_when_ansi_on_plain_when_off() {
        let on = render(true, |r| r.paint_step_divider(1));
        let off = render(false, |r| r.paint_step_divider(1));
        assert!(on.contains("\x1b[2m"));
        assert!(on.contains("step 1"));
        assert!(!off.contains("\x1b["));
        assert!(off.contains("step 1"));
    }

    #[test]
    fn paint_tool_result_uses_check_or_cross() {
        let ok = render(true, |r| r.paint_tool_result(true, "read_file", "1: foo"));
        let bad = render(true, |r| r.paint_tool_result(false, "read_file", "denied"));
        assert!(ok.contains("✓"));
        assert!(ok.contains("read_file"));
        assert!(ok.contains("  1: foo"));
        assert!(bad.contains("✗"));
        assert!(bad.contains("  denied"));
    }

    #[test]
    fn abbreviate_for_inline_escapes_whitespace_and_truncates() {
        let s = abbreviate_for_inline("a\nb\tc");
        assert_eq!(s, "a\\nb\\tc");
        let long: String = "x".repeat(200);
        let abbr = abbreviate_for_inline(&long);
        assert!(abbr.ends_with('…'));
        assert_eq!(abbr.chars().count(), 81);
    }

    #[test]
    fn noop_stream_events_does_nothing_and_stays_silent() {
        let mut e = NoopStreamEvents;
        e.on_text_delta("x");
        e.on_assistant_done("x");
        let args = BTreeMap::new();
        e.on_tool_call("none", &args);
    }
}
```

Modify `src/ui/mod.rs`:

```rust
pub mod confirm;
pub mod render;
pub mod stream;
```

- [ ] **Step 2: Run to confirm failure**

Run: `~/.cargo/bin/cargo test --lib ui::stream 2>&1 | tail -15`
Expected: 8 tests fail with `unimplemented!()` panics.

- [ ] **Step 3: Implement renderer**

Replace bodies of the unimplemented functions in `src/ui/stream.rs` with:

```rust
impl<W: Write> TtyRenderer<W> {
    pub fn new_with(out: W, use_ansi: bool) -> Self {
        Self { out, use_ansi, text_started: false }
    }

    pub fn paint_step_divider(&mut self, step_index: usize) {
        if self.use_ansi {
            let _ = writeln!(self.out, "\x1b[2m─── step {step_index} ───\x1b[0m");
        } else {
            let _ = writeln!(self.out, "─── step {step_index} ───");
        }
    }

    pub fn paint_tool_result(&mut self, ok: bool, label: &str, body: &str) {
        if self.use_ansi {
            let mark = if ok { "\x1b[32m✓\x1b[0m" } else { "\x1b[31m✗\x1b[0m" };
            let _ = writeln!(self.out, "{mark} {label}");
        } else {
            let prefix = if ok { "OK" } else { "ERR" };
            let _ = writeln!(self.out, "{prefix}: {label}");
        }
        for line in body.lines() {
            let _ = writeln!(self.out, "  {line}");
        }
    }
}

impl<W: Write> StreamEvents for TtyRenderer<W> {
    fn on_text_delta(&mut self, chunk: &str) {
        if !self.text_started {
            if self.use_ansi {
                let _ = write!(self.out, "\x1b[36m");
            }
            self.text_started = true;
        }
        let _ = write!(self.out, "{chunk}");
        let _ = self.out.flush();
    }

    fn on_assistant_done(&mut self, _full_text: &str) {
        if self.text_started {
            if self.use_ansi {
                let _ = write!(self.out, "\x1b[0m");
            }
            let _ = writeln!(self.out);
        }
        self.text_started = false;
    }

    fn on_tool_call(&mut self, name: &str, input: &BTreeMap<String, String>) {
        let mut args = String::new();
        for (i, (k, v)) in input.iter().enumerate() {
            if i > 0 {
                args.push_str(", ");
            }
            args.push_str(k);
            args.push('=');
            args.push_str(&abbreviate_for_inline(v));
        }
        if self.use_ansi {
            let _ = writeln!(self.out, "\x1b[33m🛠 {name}({args})\x1b[0m");
        } else {
            let _ = writeln!(self.out, "> {name}({args})");
        }
    }
}

pub fn abbreviate_for_inline(value: &str) -> String {
    let escaped: String = value
        .chars()
        .flat_map(|ch| -> Box<dyn Iterator<Item = char>> {
            match ch {
                '\n' => Box::new("\\n".chars()),
                '\r' => Box::new("\\r".chars()),
                '\t' => Box::new("\\t".chars()),
                other => Box::new(std::iter::once(other)),
            }
        })
        .collect();
    let total = escaped.chars().count();
    if total > 80 {
        let head: String = escaped.chars().take(80).collect();
        format!("{head}…")
    } else {
        escaped
    }
}
```

Add a constructor convenience for stdout. Append at the bottom of `src/ui/stream.rs`:

```rust
impl TtyRenderer<std::io::Stdout> {
    pub fn from_stdout() -> Self {
        use std::io::IsTerminal;
        let use_ansi = std::io::stdout().is_terminal();
        Self::new_with(std::io::stdout(), use_ansi)
    }
}
```

- [ ] **Step 4: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test --lib ui::stream 2>&1 | tail -15`
Expected: `test result: ok. 8 passed`

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 188 passed`

Run: `~/.cargo/bin/cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src/ui/stream.rs src/ui/mod.rs
git commit -m "feat(ui): add StreamEvents trait and TtyRenderer

StreamEvents (on_text_delta / on_assistant_done / on_tool_call)
plus NoopStreamEvents and TtyRenderer<W>. ANSI cyan for text,
yellow tool calls, green/red tool result marks, dim step
divider. abbreviate_for_inline escapes whitespace + truncates.

Tests: +8 (180 -> 188)"
```

---

## Task M3: ModelClient::respond gains events parameter (signature only)

**Files:**
- Modify: `src/model/client.rs`
- Modify: `src/model/deepseek.rs` (impl block)
- Modify: `src/core/loop_runtime.rs` (call site)

This task is a pure refactor. Behavior unchanged. After this lands, all 188 tests still pass and `dscode chat` looks identical to today.

- [ ] **Step 1: Update `ModelClient` trait**

Replace contents of `src/model/client.rs` with:

```rust
use crate::error::AppResult;
use crate::model::protocol::{ModelRequest, ModelResponse, TokenUsage};
use crate::ui::stream::StreamEvents;

pub trait ModelClient {
    fn respond(
        &self,
        input: ModelRequest,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)>;
}
```

- [ ] **Step 2: Update `DeepSeekClient::respond` signature**

In `src/model/deepseek.rs`, replace the `impl ModelClient for DeepSeekClient` block (lines 20–31) with:

```rust
impl ModelClient for DeepSeekClient {
    fn respond(
        &self,
        input: ModelRequest,
        _events: &mut dyn crate::ui::stream::StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        if let Ok(api_key) = env::var(&self.config.api_key_env) {
            if !api_key.trim().is_empty() {
                if let Ok(pair) = self.respond_remote(&input, &api_key) {
                    return Ok(pair);
                }
            }
        }
        Ok((self.respond_offline(input), None))
    }
}
```

(Underscore prefix suppresses unused warnings; M4 wires it up.)

- [ ] **Step 3: Update test call sites in `src/model/deepseek.rs`**

Find the four `planner().respond(request)` calls in the `mod tests` section (lines 1031, 1068, 1107, 1147, 1192). Add a `NoopStreamEvents` argument to each. Update each call:

```rust
let response = planner().respond(request, &mut crate::ui::stream::NoopStreamEvents).unwrap().0;
```

- [ ] **Step 4: Update `AgentLoop::run_with` call site**

In `src/core/loop_runtime.rs`, find the `let (response, step_usage) = client.respond(request)?;` line (~line 129) and replace with:

```rust
let mut events = crate::ui::stream::NoopStreamEvents;
let (response, step_usage) = client.respond(request, &mut events)?;
```

- [ ] **Step 5: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 188 passed`

Run: `~/.cargo/bin/cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/model/client.rs src/model/deepseek.rs src/core/loop_runtime.rs
git commit -m "refactor(model): ModelClient::respond accepts StreamEvents

Pure signature change. All call sites pass NoopStreamEvents.
M4 will wire real streaming events through the param.

Tests: 188 unchanged"
```

---

## Task M4: DeepSeek streaming SSE integration (OpenAI + Anthropic)

**Files:**
- Modify: `src/util/process.rs` (add `StreamingProcess` + `spawn_streaming`)
- Modify: `src/model/deepseek.rs` (replace remote paths with streaming variants; add `parse_openai_stream` + `parse_anthropic_stream`)

This is the heaviest task. Split into substeps.

- [ ] **Step 1: Write failing tests for OpenAI stream parser**

Append to the `mod tests` block in `src/model/deepseek.rs` (above the closing `}` of the test module):

```rust
    use crate::ui::stream::{NoopStreamEvents, StreamEvents};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    #[derive(Default)]
    struct CapturingEvents {
        chunks: RefCell<Vec<String>>,
        done: RefCell<Vec<String>>,
        tool_calls: RefCell<Vec<(String, BTreeMap<String, String>)>>,
    }

    impl StreamEvents for CapturingEvents {
        fn on_text_delta(&mut self, chunk: &str) {
            self.chunks.borrow_mut().push(chunk.to_string());
        }
        fn on_assistant_done(&mut self, full_text: &str) {
            self.done.borrow_mut().push(full_text.to_string());
        }
        fn on_tool_call(&mut self, name: &str, input: &BTreeMap<String, String>) {
            self.tool_calls
                .borrow_mut()
                .push((name.to_string(), input.clone()));
        }
    }

    #[test]
    fn parse_openai_stream_emits_text_deltas_and_finishes() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        assert_eq!(resp.message, "Hello");
        assert!(matches!(resp.action, super::ModelAction::Finish));
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt, 3);
        assert_eq!(usage.completion, 2);
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(events.done.borrow().len(), 1);
        assert!(events.tool_calls.borrow().is_empty());
    }

    #[test]
    fn parse_openai_stream_assembles_tool_call_across_chunks() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, _usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
        let calls = events.tool_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
        assert_eq!(calls[0].1.get("path").map(String::as_str), Some("a.rs"));
    }

    #[test]
    fn parse_openai_stream_returns_none_usage_when_omitted() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (_resp, usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        assert!(usage.is_none());
    }

    #[test]
    fn parse_openai_stream_errors_on_malformed_tool_arguments() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x\",\"type\":\"function\",\"function\":{\"name\":\"git_diff\",\"arguments\":\"{not_json\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let result = super::parse_openai_stream(&mut cur, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn parse_anthropic_stream_emits_text_deltas_and_message_stop() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi \"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"there\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        assert_eq!(resp.message, "hi there");
        assert!(matches!(resp.action, super::ModelAction::Finish));
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt, 10);
        assert_eq!(usage.completion, 2);
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["hi ".to_string(), "there".to_string()]);
    }

    #[test]
    fn parse_anthropic_stream_assembles_tool_use_input_json() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.rs\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, _usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
        let calls = events.tool_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
    }

    #[test]
    fn parse_anthropic_stream_keeps_initial_usage_when_message_delta_missing_input() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (_resp, usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt, 7);
        assert_eq!(usage.completion, 4);
    }

    #[test]
    fn parse_anthropic_stream_errors_on_malformed_tool_input() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"not_json\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let result = super::parse_anthropic_stream(&mut cur, &mut events);
        assert!(result.is_err());
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `~/.cargo/bin/cargo test --lib model::deepseek 2>&1 | tail -20`
Expected: 8 new tests fail with "cannot find function `parse_openai_stream` / `parse_anthropic_stream`".

- [ ] **Step 3: Implement `parse_openai_stream`**

In `src/model/deepseek.rs`, add imports near the top (after the existing `use` block):

```rust
use std::io::BufRead;

use crate::ui::stream::StreamEvents;
use crate::util::sse::{read_frame, SseFrame};
```

Add helper structs and the parser. Place them above the `parse_openai_chat_completion` function:

```rust
#[derive(Default, Debug)]
struct OpenAiToolAssembly {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(crate) fn parse_openai_stream<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut full_text = String::new();
    let mut usage: Option<TokenUsage> = None;
    let mut tool_assembly: Option<OpenAiToolAssembly> = None;

    while let Some(frame) = read_frame(reader).map_err(|e| app_error(format!("sse read failed: {e}")))? {
        let data = frame.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let root = parse_root_object(data)
            .map_err(|e| tool_failure(format!("malformed openai sse frame: {e}")))?;

        if let Some(error) = root.get("error").and_then(json_as_object) {
            let message = error
                .get("message")
                .and_then(json_as_string)
                .unwrap_or("openai stream error");
            return Err(tool_failure(format!("openai api error: {message}")));
        }

        if let Some(usage_obj) = root.get("usage").and_then(json_as_object) {
            if let (Some(p), Some(c)) = (
                usage_obj.get("prompt_tokens").and_then(json_as_u64),
                usage_obj.get("completion_tokens").and_then(json_as_u64),
            ) {
                usage = Some(TokenUsage { prompt: p, completion: c });
            }
        }

        let Some(choices) = root.get("choices").and_then(json_as_array) else {
            continue;
        };
        let Some(choice) = choices.first().and_then(json_as_object) else {
            continue;
        };
        if let Some(delta) = choice.get("delta").and_then(json_as_object) {
            if let Some(content) = delta.get("content").and_then(json_as_string) {
                if !content.is_empty() {
                    events.on_text_delta(content);
                    full_text.push_str(content);
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(json_as_array) {
                for call in tool_calls {
                    let Some(call_obj) = json_as_object(call) else { continue };
                    let assembly = tool_assembly.get_or_insert_with(OpenAiToolAssembly::default);
                    if assembly.id.is_none() {
                        if let Some(id) = call_obj.get("id").and_then(json_as_string) {
                            assembly.id = Some(id.to_string());
                        }
                    }
                    if let Some(function) = call_obj.get("function").and_then(json_as_object) {
                        if assembly.name.is_none() {
                            if let Some(name) = function.get("name").and_then(json_as_string) {
                                assembly.name = Some(name.to_string());
                            }
                        }
                        if let Some(args) = function.get("arguments").and_then(json_as_string) {
                            assembly.arguments.push_str(args);
                        }
                    }
                }
            }
        }
    }

    events.on_assistant_done(&full_text);

    let action = if let Some(assembly) = tool_assembly {
        let name = assembly
            .name
            .ok_or_else(|| tool_failure("openai tool call missing function.name"))?;
        let arguments = parse_tool_arguments(&assembly.arguments)?;
        events.on_tool_call(&name, &arguments);
        ModelAction::CallTool {
            tool_name: name,
            input: ToolInput { args: arguments },
        }
    } else {
        ModelAction::Finish
    };

    let message = if full_text.is_empty() && matches!(action, ModelAction::CallTool { .. }) {
        "DeepSeek selected a tool.".to_string()
    } else if full_text.is_empty() {
        "DeepSeek returned no content.".to_string()
    } else {
        full_text
    };

    Ok((ModelResponse { message, action }, usage))
}
```

(Imports needed: `tool_failure` may not be in scope yet — add `use crate::error::tool_failure;` at top.)

- [ ] **Step 4: Implement `parse_anthropic_stream`**

Add directly after `parse_openai_stream` in `src/model/deepseek.rs`:

```rust
#[derive(Default, Debug)]
struct AnthropicToolAssembly {
    id: Option<String>,
    name: Option<String>,
    partial_json: String,
}

pub(crate) fn parse_anthropic_stream<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut full_text = String::new();
    let mut tool_assembly: Option<AnthropicToolAssembly> = None;
    let mut usage_prompt: Option<u64> = None;
    let mut usage_completion: Option<u64> = None;

    while let Some(frame) = read_frame(reader).map_err(|e| app_error(format!("sse read failed: {e}")))? {
        let event_kind = frame.event.as_deref().unwrap_or("");
        let data = frame.data.trim();
        if data.is_empty() {
            if event_kind == "message_stop" {
                break;
            }
            continue;
        }
        let root = parse_root_object(data)
            .map_err(|e| tool_failure(format!("malformed anthropic sse frame: {e}")))?;

        match event_kind {
            "message_start" => {
                if let Some(message) = root.get("message").and_then(json_as_object) {
                    if let Some(usage_obj) = message.get("usage").and_then(json_as_object) {
                        if let Some(p) = usage_obj.get("input_tokens").and_then(json_as_u64) {
                            usage_prompt = Some(p);
                        }
                        if let Some(c) = usage_obj.get("output_tokens").and_then(json_as_u64) {
                            usage_completion = Some(c);
                        }
                    }
                }
            }
            "content_block_start" => {
                if let Some(block) = root.get("content_block").and_then(json_as_object) {
                    if block.get("type").and_then(json_as_string) == Some("tool_use") {
                        let id = block
                            .get("id")
                            .and_then(json_as_string)
                            .map(str::to_string);
                        let name = block
                            .get("name")
                            .and_then(json_as_string)
                            .map(str::to_string);
                        tool_assembly = Some(AnthropicToolAssembly {
                            id,
                            name,
                            partial_json: String::new(),
                        });
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = root.get("delta").and_then(json_as_object) {
                    let delta_type = delta.get("type").and_then(json_as_string).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(json_as_string) {
                                if !text.is_empty() {
                                    events.on_text_delta(text);
                                    full_text.push_str(text);
                                }
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) = delta.get("partial_json").and_then(json_as_string) {
                                if let Some(assembly) = tool_assembly.as_mut() {
                                    assembly.partial_json.push_str(partial);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(usage_obj) = root.get("usage").and_then(json_as_object) {
                    if let Some(c) = usage_obj.get("output_tokens").and_then(json_as_u64) {
                        usage_completion = Some(c);
                    }
                    if let Some(p) = usage_obj.get("input_tokens").and_then(json_as_u64) {
                        usage_prompt = Some(p);
                    }
                }
            }
            "message_stop" => {
                break;
            }
            "error" => {
                let message = root
                    .get("error")
                    .and_then(json_as_object)
                    .and_then(|e| e.get("message"))
                    .and_then(json_as_string)
                    .unwrap_or("anthropic stream error");
                return Err(tool_failure(format!("anthropic api error: {message}")));
            }
            _ => {}
        }
    }

    events.on_assistant_done(&full_text);

    let action = if let Some(assembly) = tool_assembly {
        let name = assembly
            .name
            .ok_or_else(|| tool_failure("anthropic tool_use missing name"))?;
        let arguments = parse_tool_arguments(&assembly.partial_json)?;
        events.on_tool_call(&name, &arguments);
        ModelAction::CallTool {
            tool_name: name,
            input: ToolInput { args: arguments },
        }
    } else {
        ModelAction::Finish
    };

    let usage = match (usage_prompt, usage_completion) {
        (Some(p), Some(c)) => Some(TokenUsage { prompt: p, completion: c }),
        _ => None,
    };

    let message = if full_text.is_empty() && matches!(action, ModelAction::CallTool { .. }) {
        "DeepSeek selected a tool.".to_string()
    } else if full_text.is_empty() {
        "DeepSeek returned no content.".to_string()
    } else {
        full_text
    };

    Ok((ModelResponse { message, action }, usage))
}
```

- [ ] **Step 5: Run new parser tests, confirm pass**

Run: `~/.cargo/bin/cargo test --lib model::deepseek 2>&1 | tail -25`
Expected: 8 new + previous tests pass.

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 196 passed`

- [ ] **Step 6: Add `StreamingProcess` helper**

Append to `src/util/process.rs` (after the `run_capture_stdout` function):

```rust
use std::io::BufReader;
use std::process::{ChildStdout, ExitStatus, Stdio};

pub struct StreamingProcess {
    child: std::process::Child,
    pub stdout: BufReader<ChildStdout>,
}

pub fn spawn_streaming(bin: &str, args: &[&str]) -> AppResult<StreamingProcess> {
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                app_error(format!("{bin} not found in PATH; install it before retrying"))
            } else {
                app_error(format!("could not invoke {bin}: {error}"))
            }
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| app_error(format!("{bin} produced no stdout pipe")))?;
    Ok(StreamingProcess {
        child,
        stdout: BufReader::new(stdout),
    })
}

impl StreamingProcess {
    pub fn finish(mut self) -> AppResult<(ExitStatus, String)> {
        // Drain remaining stdout (if any) before waiting on the child.
        use std::io::Read;
        let mut sink = Vec::new();
        let _ = self.stdout.read_to_end(&mut sink);

        let mut stderr_buf = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_buf);
        }
        let status = self
            .child
            .wait()
            .map_err(|error| app_error(format!("failed to await child: {error}")))?;
        const TAIL_LIMIT: usize = 64 * 1024;
        let tail = if stderr_buf.len() > TAIL_LIMIT {
            stderr_buf.split_off(stderr_buf.len() - TAIL_LIMIT)
        } else {
            stderr_buf
        };
        Ok((status, tail))
    }
}
```

- [ ] **Step 7: Wire streaming into `respond_remote_openai`**

Replace the `respond_remote_openai` method body in `src/model/deepseek.rs` (lines 41–92) with:

```rust
    fn respond_remote_openai(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let endpoint = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));
        let system_prompt = build_openai_tool_system_prompt(&input.system_prompt);
        let user_prompt = build_user_prompt(input);
        let tools = build_openai_tools(&input.available_tools);
        let body = format!(
            concat!(
                "{{",
                "\"model\":\"{}\",",
                "\"temperature\":0,",
                "\"max_tokens\":1024,",
                "\"stream\":true,",
                "\"stream_options\":{{\"include_usage\":true}},",
                "\"tool_choice\":\"auto\",",
                "\"tools\":{},",
                "\"messages\":[",
                "{{\"role\":\"system\",\"content\":\"{}\"}},",
                "{{\"role\":\"user\",\"content\":\"{}\"}}",
                "]",
                "}}"
            ),
            json_escape(&self.config.model),
            tools,
            json_escape(&system_prompt),
            json_escape(&user_prompt)
        );

        let auth = format!("Authorization: Bearer {api_key}");
        let args = [
            "-sS",
            "-N",
            "--max-time",
            "60",
            "-X",
            "POST",
            endpoint.as_str(),
            "-H",
            auth.as_str(),
            "-H",
            "Content-Type: application/json",
            "-H",
            "Accept: text/event-stream",
            "--data-binary",
            body.as_str(),
        ];

        let mut process = crate::util::process::spawn_streaming("curl", &args)?;
        let parsed = parse_openai_stream(&mut process.stdout, events);
        let (status, stderr_tail) = process.finish()?;
        if !status.success() {
            return Err(tool_failure(format!(
                "deepseek openai stream failed (exit {:?}): {}",
                status.code(),
                stderr_tail.trim()
            )));
        }
        parsed
    }
```

- [ ] **Step 8: Wire streaming into `respond_remote_anthropic`**

Replace the `respond_remote_anthropic` method body (lines 94–146) with:

```rust
    fn respond_remote_anthropic(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let endpoint = format!("{}/messages", self.config.base_url.trim_end_matches('/'));
        let system_prompt = build_anthropic_tool_system_prompt(&input.system_prompt);
        let user_prompt = build_user_prompt(input);
        let tools = build_anthropic_tools(&input.available_tools);
        let body = format!(
            concat!(
                "{{",
                "\"model\":\"{}\",",
                "\"max_tokens\":1024,",
                "\"stream\":true,",
                "\"tool_choice\":{{\"type\":\"auto\"}},",
                "\"tools\":{},",
                "\"system\":\"{}\",",
                "\"messages\":[",
                "{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}",
                "]",
                "}}"
            ),
            json_escape(&self.config.model),
            tools,
            json_escape(&system_prompt),
            json_escape(&user_prompt)
        );

        let api_header = format!("x-api-key: {api_key}");
        let args = [
            "-sS",
            "-N",
            "--max-time",
            "60",
            "-X",
            "POST",
            endpoint.as_str(),
            "-H",
            api_header.as_str(),
            "-H",
            "anthropic-version: 2023-06-01",
            "-H",
            "Content-Type: application/json",
            "-H",
            "Accept: text/event-stream",
            "--data-binary",
            body.as_str(),
        ];

        let mut process = crate::util::process::spawn_streaming("curl", &args)?;
        let parsed = parse_anthropic_stream(&mut process.stdout, events);
        let (status, stderr_tail) = process.finish()?;
        if !status.success() {
            return Err(tool_failure(format!(
                "deepseek anthropic stream failed (exit {:?}): {}",
                status.code(),
                stderr_tail.trim()
            )));
        }
        parsed
    }
```

- [ ] **Step 9: Update `respond_remote` and `respond` to thread events**

In `src/model/deepseek.rs`, locate `fn respond_remote` (above `respond_remote_openai`) and change its signature:

```rust
    fn respond_remote(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        match api_flavor(&self.config.base_url) {
            ApiFlavor::OpenAi => self.respond_remote_openai(input, api_key, events),
            ApiFlavor::Anthropic => self.respond_remote_anthropic(input, api_key, events),
        }
    }
```

Then update the `impl ModelClient for DeepSeekClient` block:

```rust
impl ModelClient for DeepSeekClient {
    fn respond(
        &self,
        input: ModelRequest,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        if let Ok(api_key) = env::var(&self.config.api_key_env) {
            if !api_key.trim().is_empty() {
                if let Ok(pair) = self.respond_remote(&input, &api_key, events) {
                    return Ok(pair);
                }
            }
        }
        let response = self.respond_offline(input);
        events.on_text_delta(&response.message);
        events.on_assistant_done(&response.message);
        if let ModelAction::CallTool { tool_name, input } = &response.action {
            events.on_tool_call(tool_name, &input.args);
        }
        Ok((response, None))
    }
}
```

- [ ] **Step 10: Remove dead non-streaming parser functions**

The non-streaming JSON parsers (`parse_openai_chat_completion`, `parse_openai_usage`, `parse_anthropic_messages`, `parse_anthropic_usage`) are no longer reachable from production code, but their tests still cover the JSON-shape behavior. Decision: keep them (and their tests) in place — they're referenced as `#[cfg(test)]` only. Mark them with `#[allow(dead_code)]` directly above each `fn` definition so the build stays warning-free:

```rust
#[allow(dead_code)]
fn parse_openai_chat_completion(body: &str) -> AppResult<ModelResponse> {
```

```rust
#[allow(dead_code)]
fn parse_openai_usage(body: &str) -> Option<TokenUsage> {
```

```rust
#[allow(dead_code)]
fn parse_anthropic_messages(body: &str) -> AppResult<ModelResponse> {
```

```rust
#[allow(dead_code)]
fn parse_anthropic_usage(body: &str) -> Option<TokenUsage> {
```

- [ ] **Step 11: Run all tests, confirm green**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 196 passed`

Run: `~/.cargo/bin/cargo build 2>&1 | tail -10`
Expected: `Finished` with 0 warnings.

- [ ] **Step 12: Commit**

```bash
git add src/model/deepseek.rs src/util/process.rs
git commit -m "feat(deepseek): stream SSE for OpenAI and Anthropic paths

curl spawned with -N + --max-time 60 + Accept: text/event-stream.
parse_openai_stream and parse_anthropic_stream consume any
BufRead, dispatch text deltas and tool-call assemblies via
StreamEvents, return (ModelResponse, Option<TokenUsage>).

Offline fallback also drives StreamEvents so the renderer sees
the same color treatment regardless of source.

Tests: +8 (188 -> 196)"
```

---

## Task M5: AgentLoop renderer integration

**Files:**
- Modify: `src/core/loop_runtime.rs`

- [ ] **Step 1: Replace step output with renderer**

In `src/core/loop_runtime.rs`, replace the `for step in 0..steps {` loop body (lines 113–182) with:

```rust
        let mut renderer = crate::ui::stream::TtyRenderer::from_stdout();
        for step in 0..steps {
            let request = ModelRequest {
                system_prompt: build_system_prompt(skill),
                task: context.task.clone(),
                profile_name: profile.name.clone(),
                profile_hints: profile.hints.clone(),
                primary_file: primary_file.clone(),
                suggested_test_command: suggested_test_command.clone(),
                available_tools: registry
                    .names_for_policy(&policy)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                observations: compact_observations(&observations),
            };

            renderer.paint_step_divider(step + 1);
            let (response, step_usage) = client.respond(request, &mut renderer)?;
            if let Some(usage) = step_usage {
                total_usage.prompt += usage.prompt;
                total_usage.completion += usage.completion;
            }
            last_message = response.message.clone();

            match response.action {
                ModelAction::CallTool { tool_name, input } => {
                    let event_input = input.args.clone();
                    match registry.execute_with_policy(&tool_name, input, &policy) {
                        Ok(output) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let summary = summarize_for_kind(&output.summary, kind);
                            renderer.paint_tool_result(true, &tool_name, &summary);
                            let event_output = summary.clone();
                            let event_name = tool_name.clone();
                            observations.push(Observation::ok(tool_name, summary));
                            tool_events.push(ToolEvent {
                                tool_name: event_name,
                                input: event_input,
                                output: event_output,
                                status: crate::model::protocol::ObservationStatus::Ok,
                            });
                        }
                        Err(error) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let summary = summarize_for_kind(&error.to_string(), kind);
                            renderer.paint_tool_result(false, &tool_name, &summary);
                            let event_output = summary.clone();
                            let event_name = tool_name.clone();
                            observations.push(Observation::failed(tool_name, summary));
                            tool_events.push(ToolEvent {
                                tool_name: event_name,
                                input: event_input,
                                output: event_output,
                                status: crate::model::protocol::ObservationStatus::Failed,
                            });
                        }
                    }
                }
                ModelAction::Finish => {
                    break;
                }
            }
        }
```

(Note: the `let label = match crate::error::classify(...)` branch from the old code is dropped — `paint_tool_result` already differentiates ok/fail visually. The `kind.label()` was only used in the deleted `println!` text and is no longer needed.)

- [ ] **Step 2: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 196 passed`

Run: `~/.cargo/bin/cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 warnings.

- [ ] **Step 3: Smoke check (offline planner path)**

Run: `~/.cargo/bin/cargo run -- run "list files" 2>&1 | head -30`
Expected: Output contains:
- `─── step 1 ───` (or ANSI dim equivalent if TTY)
- Yellow tool call line `🛠 list_files(...)` (or `> list_files(...)` for non-TTY)
- Green/red `✓` or `✗` mark on tool result

- [ ] **Step 4: Commit**

```bash
git add src/core/loop_runtime.rs
git commit -m "feat(core): drive AgentLoop output through TtyRenderer

step dividers, tool result marks, and tool-call args now go
through the renderer rather than ad-hoc println!. Streaming
text deltas land naturally inside the per-step span.

Tests: 196 unchanged"
```

---

## Task M6: Repl + dogfood across all entry points

**Files:** No code changes expected. (`Repl::handle_line` already calls `AgentLoop::run_with`, which now uses the renderer.)

- [ ] **Step 1: Verify `dscode chat` streams correctly**

Run: `DEEPSEEK_API_KEY=dummy_offline_test ~/.cargo/bin/cargo run -- chat`
Wait for `> ` prompt, then type:
```
list files in src
```
Press enter. Confirm output includes step divider, yellow tool call, green tool result, all on stdout (not interspersed with `> ` prompt on stderr).

Type `/quit` to exit.

- [ ] **Step 2: Verify `dscode run` non-TTY ANSI is suppressed**

Run: `~/.cargo/bin/cargo run -- run "list files" 2>/dev/null | cat -v | head -20`
Expected: Output text with NO `^[[` ANSI escape sequences (since stdout is piped, not a TTY).

- [ ] **Step 3: Verify `dscode pr review` streams**

If a real PR exists in a connected repo:
```
~/.cargo/bin/cargo run -- pr review <PR_NUMBER>
```
Expected: streaming markdown review with cyan text.

If no real PR available, skip — the offline planner path was already covered in M5 step 3.

- [ ] **Step 4: Verify `dscode chat` with API key uses real stream**

If `DEEPSEEK_API_KEY` is set with a real key:
```
DEEPSEEK_API_KEY=$REAL_KEY ~/.cargo/bin/cargo run -- chat
```
Type a question. Confirm tokens appear progressively (visibly typed character-by-character or word-by-word), not all at once after a delay.

If no real API key available, document this limitation and skip — offline path was confirmed in M5 step 3.

- [ ] **Step 5: Run full test suite again**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 196 passed`

- [ ] **Step 6: Commit (only if any tweaks were necessary; otherwise skip)**

If M6 found a regression and required a fix, commit that fix. Otherwise, no commit on this task.

---

## Task M7: Documentation + roadmap

**Files:**
- Create: `docs/streaming.md`
- Modify: `docs/repl.md`
- Modify: `docs/roadmap.md`

- [ ] **Step 1: Write `docs/streaming.md`**

Create `docs/streaming.md` with:

```markdown
# Streaming Output

`dscode` streams LLM output token-by-token over Server-Sent Events
(SSE) by default. Both OpenAI-compatible (`/chat/completions`) and
Anthropic-compatible (`/messages`) base URLs are supported.

## What you see

When stdout is a TTY, the renderer applies ANSI:
- Cyan: assistant text (streamed live)
- Yellow `🛠 name(args)`: tool call (rendered after args fully assembled)
- Green `✓ name`: tool succeeded
- Red `✗ name`: tool failed
- Dim `─── step N ───`: step divider

Pipe stdout to a file (`dscode run "..." > out.txt`) and ANSI is
suppressed automatically — `is_terminal()` detects the redirection.

## Implementation

`curl -sS -N --max-time 60` is spawned per LLM call. The `-N`
(no-buffer) flag is critical — without it curl batches output by
buffer-fill rather than by SSE frame boundary. Frames are read by
`util::sse::read_frame` from a `BufRead` over child stdout. Per
protocol:

- OpenAI: `delta.content` chunks dispatch to `on_text_delta` live.
  `delta.tool_calls[]` accumulates across frames into a single
  `OpenAiToolAssembly` and is rendered once `finish_reason ==
  "tool_calls"`. Final `usage` frame (requested via
  `stream_options.include_usage`) carries token counts. `[DONE]`
  closes the stream.
- Anthropic: `event: text_delta` chunks dispatch live. `event:
  input_json_delta` accumulates `partial_json` for tool-use blocks,
  parsed once `content_block_stop` arrives. `message_delta` carries
  output tokens; `message_stop` closes the stream.

## Failure modes

- Curl exit non-zero → `tool_failure` with stderr tail (last 64 KB)
- HTTP 4xx/5xx errors emit an `error` frame → `tool_failure(api error)`
- Malformed SSE → `tool_failure(malformed sse frame: ...)`
- Tool args JSON unparseable after assembly → `tool_failure(...)`

Already-streamed tokens are not rolled back on error — the red `✗`
mark appears after them, matching Claude Code / Codex behavior.

## Offline planner

When `DEEPSEEK_API_KEY` is unset, the offline planner runs locally
and still drives `StreamEvents`, so the renderer paints the same
colors. Tokens arrive in one block rather than progressively.

## Limitations (Phase 9c candidates)

- Ctrl+C does not interrupt an in-flight stream
- `up`/`down` arrow history not implemented (use `rlwrap dscode chat`)
- Patch / shell tool output not streamed (rendered in one block)
- Syntax highlight not applied
- Color theme not user-configurable
```

- [ ] **Step 2: Update `docs/repl.md`**

In `docs/repl.md`, find the "v1 limitations" section and remove the streaming line. Replace:

```markdown
## v1 limitations

- No streaming token output; the planner runs to completion before
  printing the final assistant message.
- No up/down arrow history. Use `rlwrap dscode chat` for a quick
```

with:

```markdown
## v1 limitations

- No up/down arrow history. Use `rlwrap dscode chat` for a quick
```

(Other bullets stay unchanged.)

Also append a short paragraph to the end of the "Cross-turn context" section:

```markdown
Streaming token output is enabled by default — see
[`docs/streaming.md`](streaming.md) for protocol detail and color rules.
```

- [ ] **Step 3: Update `docs/roadmap.md`**

In `docs/roadmap.md`, find the Phase 9b entry and mark it complete:

Replace whatever line currently denotes Phase 9b with:

```markdown
- [x] **Phase 9b — Streaming SSE** (2026-04-30)
  - Generic `util::sse::read_frame` framework
  - `StreamEvents` + `TtyRenderer` (cyan / yellow / green / red ANSI conditional on TTY)
  - `ModelClient::respond` accepts `&mut dyn StreamEvents`
  - DeepSeek streaming for OpenAI + Anthropic protocols (`curl -N`)
  - Offline planner also drives `StreamEvents` for color parity
  - 175 → 196 tests, 0 new dependencies
```

- [ ] **Step 4: Run final test suite**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 196 passed`

Run: `~/.cargo/bin/cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add docs/streaming.md docs/repl.md docs/roadmap.md
git commit -m "docs: Phase 9b streaming SSE complete

- New docs/streaming.md describes protocol + color rules + failure modes
- Drop 'no streaming' bullet from docs/repl.md v1 limitations
- Mark Phase 9b complete in docs/roadmap.md"
```

---

## Final verification

After M7 commit:

- [ ] **Step 1: Sanity-check git history**

Run: `git log --oneline main..HEAD 2>&1 | head -10`
Expected: 5–7 commits in order: M1 → M2 → M3 → M4 → (optional M5/M6) → M7. Each commit message matches the format.

- [ ] **Step 2: Lint check**

Run: `~/.cargo/bin/cargo clippy --all-targets -- -D warnings 2>&1 | tail -10`
Expected: No clippy warnings.

Run: `~/.cargo/bin/cargo fmt --check 2>&1 | tail -5`
Expected: No diffs.

- [ ] **Step 3: Final test run**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `test result: ok. 196 passed; 0 failed`

- [ ] **Step 4: Hand off**

Phase 9b is feature-complete. Two follow-up options for the controller (not the implementer):
1. Open a PR to `main` with a manual review pass first.
2. Push and merge directly if the controller's quality gates passed.
