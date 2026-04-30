use std::io::BufRead;

/// One SSE frame produced by [`read_frame`].
///
/// `event` is `None` for default-event frames (semantically `"message"`).
/// `data` may contain `\n` characters when the source had multiple
/// consecutive `data:` lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

/// Read one SSE frame from `reader`.
///
/// Recognizes LF and CRLF line endings; lone CR is **not** treated as
/// a terminator (`BufRead::read_line` splits on LF only). A leading
/// U+FEFF BOM is **not** stripped — pre-strip on the caller side if
/// needed. These omissions match the wire format of OpenAI- and
/// Anthropic-compatible servers and are not full WHATWG SSE-spec
/// compliance.
///
/// Returns `Ok(None)` on clean EOF, `Ok(Some(frame))` on a complete
/// frame (or partial frame at EOF if data was already accumulated).
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

        let (field, raw_value) = trimmed.split_once(':').unwrap_or((trimmed, ""));
        let value = raw_value.strip_prefix(' ').unwrap_or(raw_value);

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
