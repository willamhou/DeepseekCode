use std::collections::BTreeMap;
use std::io::Write;

/// Streaming callback contract.
///
/// Per assistant turn, callers invoke methods in this order:
///
/// 1. `on_text_delta` — zero or more times, only with non-empty chunks
/// 2. `on_assistant_done` — exactly once, with the full concatenated text
/// 3. `on_tool_call` — zero or one time, after `on_assistant_done`
///
/// Implementations may rely on this ordering.
pub trait StreamEvents {
    fn on_text_delta(&mut self, chunk: &str);
    fn on_assistant_done(&mut self, full_text: &str);
    fn on_tool_call(&mut self, name: &str, input: &BTreeMap<String, String>);
}

/// Tri-state outcome for a tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolResultKind {
    Ok,
    Failed,
    Denied,
}

#[allow(dead_code)] // useful no-op impl for tests / future callers
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
        Self {
            out,
            use_ansi,
            text_started: false,
        }
    }

    pub fn paint_step_divider(&mut self, step_index: usize) {
        if self.use_ansi {
            let _ = writeln!(self.out, "\x1b[2m─── step {step_index} ───\x1b[0m");
        } else {
            let _ = writeln!(self.out, "─── step {step_index} ───");
        }
    }

    pub fn paint_tool_result(
        &mut self,
        kind: ToolResultKind,
        label: &str,
        observation_kind: &str,
        body: &str,
    ) {
        if self.use_ansi {
            let mark = match kind {
                ToolResultKind::Ok => "\x1b[32m✓\x1b[0m",
                ToolResultKind::Failed => "\x1b[31m✗\x1b[0m",
                ToolResultKind::Denied => "\x1b[33m⊘\x1b[0m",
            };
            let _ = writeln!(self.out, "{mark} {label} [{observation_kind}]");
        } else {
            let prefix = match kind {
                ToolResultKind::Ok => "OK",
                ToolResultKind::Failed => "ERR",
                ToolResultKind::Denied => "DENIED",
            };
            let _ = writeln!(self.out, "{prefix}: {label} [{observation_kind}]");
        }
        for line in body.lines() {
            let _ = writeln!(self.out, "  {line}");
        }
    }
}

impl<W: Write> StreamEvents for TtyRenderer<W> {
    fn on_text_delta(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
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
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    if escaped.chars().count() > 80 {
        let head: String = escaped.chars().take(80).collect();
        format!("{head}…")
    } else {
        escaped
    }
}

impl TtyRenderer<std::io::Stdout> {
    pub fn from_stdout() -> Self {
        use std::io::IsTerminal;
        let use_ansi = std::io::stdout().is_terminal();
        Self::new_with(std::io::stdout(), use_ansi)
    }
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
        let ok = render(true, |r| r.paint_tool_result(ToolResultKind::Ok, "read_file", "file_excerpt", "1: foo"));
        let bad = render(true, |r| r.paint_tool_result(ToolResultKind::Failed, "read_file", "file_excerpt", "denied"));
        assert!(ok.contains("✓"));
        assert!(ok.contains("read_file"));
        assert!(ok.contains("  1: foo"));
        assert!(ok.contains("[file_excerpt]"));
        assert!(bad.contains("✗"));
        assert!(bad.contains("  denied"));
        assert!(bad.contains("[file_excerpt]"));
    }

    #[test]
    fn paint_tool_result_denied_uses_yellow_circle_or_text() {
        let on = render(true, |r| {
            r.paint_tool_result(ToolResultKind::Denied, "run_shell", "shell_output", "policy")
        });
        let off = render(false, |r| {
            r.paint_tool_result(ToolResultKind::Denied, "run_shell", "shell_output", "policy")
        });
        assert!(on.contains("\x1b[33m⊘\x1b[0m"), "ANSI denied: {on:?}");
        assert!(on.contains("run_shell"));
        assert!(on.contains("[shell_output]"));
        assert!(off.contains("DENIED: run_shell [shell_output]"));
        assert!(off.contains("  policy"));
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
