use std::collections::BTreeMap;

use crate::error::{app_error, AppResult};

#[derive(Debug, Clone)]
pub enum JsonValue {
    Object(BTreeMap<String, JsonValue>),
    Array(Vec<JsonValue>),
    String(String),
    Number(String),
    Bool(bool),
    Null,
}

pub fn parse_json_value(input: &str) -> AppResult<JsonValue> {
    let bytes = input.as_bytes();
    let mut index = 0;
    parse_value(bytes, &mut index)
}

pub fn parse_value(bytes: &[u8], index: &mut usize) -> AppResult<JsonValue> {
    skip_ws(bytes, index);
    if *index >= bytes.len() {
        return Err(app_error("unexpected end of json input"));
    }

    match bytes[*index] {
        b'{' => parse_object(bytes, index),
        b'[' => parse_array(bytes, index),
        b'"' => Ok(JsonValue::String(parse_string(bytes, index)?)),
        b't' => parse_bool(bytes, index),
        b'f' => parse_bool(bytes, index),
        b'n' => {
            if bytes.get(*index..*index + 4) == Some(b"null") {
                *index += 4;
                Ok(JsonValue::Null)
            } else {
                Err(app_error("invalid json token"))
            }
        }
        b'-' | b'0'..=b'9' => parse_number(bytes, index),
        _ => Err(app_error("unsupported json value")),
    }
}

pub fn parse_object(bytes: &[u8], index: &mut usize) -> AppResult<JsonValue> {
    let mut map = BTreeMap::new();
    *index += 1;

    loop {
        skip_ws(bytes, index);
        if *index >= bytes.len() {
            return Err(app_error("unterminated json object"));
        }
        if bytes[*index] == b'}' {
            *index += 1;
            break;
        }

        let key = parse_string(bytes, index)?;
        skip_ws(bytes, index);
        if bytes.get(*index) != Some(&b':') {
            return Err(app_error("expected `:` after json object key"));
        }
        *index += 1;
        let value = parse_value(bytes, index)?;
        map.insert(key, value);

        skip_ws(bytes, index);
        match bytes.get(*index) {
            Some(b',') => *index += 1,
            Some(b'}') => {
                *index += 1;
                break;
            }
            _ => return Err(app_error("expected `,` or `}` in json object")),
        }
    }

    Ok(JsonValue::Object(map))
}

pub fn parse_array(bytes: &[u8], index: &mut usize) -> AppResult<JsonValue> {
    let mut items = Vec::new();
    *index += 1;

    loop {
        skip_ws(bytes, index);
        if *index >= bytes.len() {
            return Err(app_error("unterminated json array"));
        }
        if bytes[*index] == b']' {
            *index += 1;
            break;
        }

        items.push(parse_value(bytes, index)?);
        skip_ws(bytes, index);
        match bytes.get(*index) {
            Some(b',') => *index += 1,
            Some(b']') => {
                *index += 1;
                break;
            }
            _ => return Err(app_error("expected `,` or `]` in json array")),
        }
    }

    Ok(JsonValue::Array(items))
}

pub fn parse_string(bytes: &[u8], index: &mut usize) -> AppResult<String> {
    if bytes.get(*index) != Some(&b'"') {
        return Err(app_error("expected json string"));
    }
    *index += 1;

    let start = *index;
    let mut buffer: Vec<u8> = Vec::new();
    let mut has_escape = false;

    while *index < bytes.len() {
        let byte = bytes[*index];
        match byte {
            b'"' => {
                let result = if has_escape {
                    String::from_utf8(buffer).map_err(|_| {
                        app_error("invalid utf-8 in json string after escape decoding")
                    })?
                } else {
                    std::str::from_utf8(&bytes[start..*index])
                        .map_err(|_| app_error("invalid utf-8 in json string"))?
                        .to_string()
                };
                *index += 1;
                return Ok(result);
            }
            b'\\' => {
                if !has_escape {
                    buffer.extend_from_slice(&bytes[start..*index]);
                    has_escape = true;
                }
                *index += 1;
                if *index >= bytes.len() {
                    return Err(app_error("unterminated json escape"));
                }
                match bytes[*index] {
                    b'"' => buffer.push(b'"'),
                    b'\\' => buffer.push(b'\\'),
                    b'/' => buffer.push(b'/'),
                    b'n' => buffer.push(b'\n'),
                    b'r' => buffer.push(b'\r'),
                    b't' => buffer.push(b'\t'),
                    b'b' => buffer.push(0x08),
                    b'f' => buffer.push(0x0C),
                    b'u' => {
                        *index += 1;
                        let cp = read_hex4(bytes, index)?;
                        if (0xD800..=0xDBFF).contains(&cp) {
                            if bytes.get(*index) != Some(&b'\\')
                                || bytes.get(*index + 1) != Some(&b'u')
                            {
                                return Err(app_error(
                                    "json `\\u` high surrogate not followed by `\\u`",
                                ));
                            }
                            *index += 2;
                            let low = read_hex4(bytes, index)?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(app_error("json `\\u` invalid low surrogate"));
                            }
                            let combined =
                                0x10000 + (((cp - 0xD800) << 10) | (low - 0xDC00)) as u32;
                            push_utf8(&mut buffer, combined);
                        } else if (0xDC00..=0xDFFF).contains(&cp) {
                            return Err(app_error("json `\\u` lone low surrogate"));
                        } else {
                            push_utf8(&mut buffer, cp as u32);
                        }
                        continue;
                    }
                    other => {
                        return Err(app_error(format!(
                            "json invalid escape `\\{}`",
                            other as char
                        )));
                    }
                }
                *index += 1;
            }
            _ => {
                if has_escape {
                    buffer.push(byte);
                }
                *index += 1;
            }
        }
    }

    Err(app_error("unterminated json string"))
}

fn read_hex4(bytes: &[u8], index: &mut usize) -> AppResult<u16> {
    if *index + 4 > bytes.len() {
        return Err(app_error("json `\\u` escape needs 4 hex digits"));
    }
    let mut value: u16 = 0;
    for _ in 0..4 {
        let digit = match bytes[*index] {
            b'0'..=b'9' => bytes[*index] - b'0',
            b'a'..=b'f' => bytes[*index] - b'a' + 10,
            b'A'..=b'F' => bytes[*index] - b'A' + 10,
            other => {
                return Err(app_error(format!(
                    "json `\\u` non-hex digit `{}`",
                    other as char
                )));
            }
        };
        value = (value << 4) | u16::from(digit);
        *index += 1;
    }
    Ok(value)
}

fn push_utf8(buffer: &mut Vec<u8>, codepoint: u32) {
    if codepoint < 0x80 {
        buffer.push(codepoint as u8);
    } else if codepoint < 0x800 {
        buffer.push(0xC0 | (codepoint >> 6) as u8);
        buffer.push(0x80 | (codepoint & 0x3F) as u8);
    } else if codepoint < 0x10000 {
        buffer.push(0xE0 | (codepoint >> 12) as u8);
        buffer.push(0x80 | ((codepoint >> 6) & 0x3F) as u8);
        buffer.push(0x80 | (codepoint & 0x3F) as u8);
    } else {
        buffer.push(0xF0 | (codepoint >> 18) as u8);
        buffer.push(0x80 | ((codepoint >> 12) & 0x3F) as u8);
        buffer.push(0x80 | ((codepoint >> 6) & 0x3F) as u8);
        buffer.push(0x80 | (codepoint & 0x3F) as u8);
    }
}

pub fn parse_bool(bytes: &[u8], index: &mut usize) -> AppResult<JsonValue> {
    if bytes.get(*index..*index + 4) == Some(b"true") {
        *index += 4;
        Ok(JsonValue::Bool(true))
    } else if bytes.get(*index..*index + 5) == Some(b"false") {
        *index += 5;
        Ok(JsonValue::Bool(false))
    } else {
        Err(app_error("invalid json boolean"))
    }
}

pub fn parse_number(bytes: &[u8], index: &mut usize) -> AppResult<JsonValue> {
    let start = *index;
    while *index < bytes.len()
        && matches!(bytes[*index], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
    {
        *index += 1;
    }
    let number = std::str::from_utf8(&bytes[start..*index])
        .map_err(|_| app_error("invalid utf8 in json number"))?;
    Ok(JsonValue::Number(number.to_string()))
}

pub fn skip_ws(bytes: &[u8], index: &mut usize) {
    while *index < bytes.len() && bytes[*index].is_ascii_whitespace() {
        *index += 1;
    }
}

pub fn json_as_string(value: &JsonValue) -> Option<&str> {
    match value {
        JsonValue::String(value) => Some(value.as_str()),
        _ => None,
    }
}

pub fn json_as_object(value: &JsonValue) -> Option<&BTreeMap<String, JsonValue>> {
    match value {
        JsonValue::Object(value) => Some(value),
        _ => None,
    }
}

pub fn json_as_array(value: &JsonValue) -> Option<&Vec<JsonValue>> {
    match value {
        JsonValue::Array(value) => Some(value),
        _ => None,
    }
}

pub fn json_as_u64(value: &JsonValue) -> Option<u64> {
    match value {
        JsonValue::Number(text) => text.parse().ok(),
        _ => None,
    }
}

pub fn parse_root_object(input: &str) -> AppResult<BTreeMap<String, JsonValue>> {
    let value = parse_json_value(input.trim())?;
    let JsonValue::Object(root) = value else {
        return Err(app_error("json root must be an object"));
    };
    Ok(root)
}

pub fn json_escape(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000C}' => output.push_str("\\f"),
            ch if (ch as u32) < 0x20 => {
                output.push_str(&format!("\\u{:04x}", ch as u32));
            }
            _ => output.push(ch),
        }
    }
    output
}

pub fn json_value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        JsonValue::Number(n) => n.clone(),
        JsonValue::String(s) => {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            out.push_str(&json_escape(s));
            out.push('"');
            out
        }
        JsonValue::Array(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_value_to_string(item));
            }
            out.push(']');
            out
        }
        JsonValue::Object(map) => {
            let mut out = String::from("{");
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(&json_escape(k));
                out.push_str("\":");
                out.push_str(&json_value_to_string(v));
            }
            out.push('}');
            out
        }
    }
}

pub fn write_quoted(out: &mut String, value: &str) {
    out.push('"');
    out.push_str(&json_escape(value));
    out.push('"');
}

pub fn write_kv_string(out: &mut String, key: &str, value: &str, leading_comma: bool) {
    if leading_comma {
        out.push(',');
    }
    write_quoted(out, key);
    out.push(':');
    write_quoted(out, value);
}

pub fn write_kv_u64(out: &mut String, key: &str, value: u64, leading_comma: bool) {
    if leading_comma {
        out.push(',');
    }
    write_quoted(out, key);
    out.push(':');
    out.push_str(&value.to_string());
}

pub fn write_kv_null(out: &mut String, key: &str, leading_comma: bool) {
    if leading_comma {
        out.push(',');
    }
    write_quoted(out, key);
    out.push_str(":null");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_handles_quotes_and_backslashes() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn json_escape_handles_control_chars() {
        assert_eq!(json_escape("a\nb\rc\td"), "a\\nb\\rc\\td");
    }

    #[test]
    fn write_quoted_emits_quoted_string() {
        let mut out = String::new();
        write_quoted(&mut out, "hello");
        assert_eq!(out, "\"hello\"");
    }

    #[test]
    fn write_kv_string_round_trips_via_parser() {
        let mut out = String::from("{");
        write_kv_string(&mut out, "k", "v", false);
        out.push('}');
        let root = parse_root_object(&out).unwrap();
        assert_eq!(root.get("k").and_then(json_as_string), Some("v"));
    }

    #[test]
    fn write_kv_u64_round_trips_via_parser() {
        let mut out = String::from("{");
        write_kv_u64(&mut out, "n", 42, false);
        out.push('}');
        let root = parse_root_object(&out).unwrap();
        assert_eq!(root.get("n").and_then(json_as_u64), Some(42));
    }

    #[test]
    fn write_kv_with_leading_comma_separates_pairs() {
        let mut out = String::from("{");
        write_kv_string(&mut out, "a", "1", false);
        write_kv_string(&mut out, "b", "2", true);
        out.push('}');
        assert_eq!(out, r#"{"a":"1","b":"2"}"#);
    }

    #[test]
    fn parse_string_decodes_unicode_escape_sequence() {
        let value = parse_json_value(r#""\u4e2d""#).unwrap();
        match value {
            JsonValue::String(s) => assert_eq!(s, "中"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_string_decodes_surrogate_pair_to_emoji() {
        // \uD83D\uDE00 = U+1F600 (😀)
        let value = parse_json_value(r#""\uD83D\uDE00""#).unwrap();
        match value {
            JsonValue::String(s) => assert_eq!(s, "😀"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_string_decodes_b_and_f_escapes() {
        let value = parse_json_value(r#""\b\f""#).unwrap();
        match value {
            JsonValue::String(s) => assert_eq!(s, "\u{0008}\u{000C}"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_string_decodes_solidus_escape() {
        let value = parse_json_value(r#""\/path\/to""#).unwrap();
        match value {
            JsonValue::String(s) => assert_eq!(s, "/path/to"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_string_passes_raw_utf8_bytes_through_unchanged() {
        // Raw UTF-8 bytes (no escape) for "中文" should round-trip.
        let value = parse_json_value(r#""中文""#).unwrap();
        match value {
            JsonValue::String(s) => assert_eq!(s, "中文"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn parse_string_rejects_unknown_escape() {
        let result = parse_json_value(r#""\q""#);
        assert!(result.is_err(), "unknown escape should error");
    }

    #[test]
    fn parse_string_rejects_lone_high_surrogate() {
        let result = parse_json_value(r#""\uD83D""#);
        assert!(result.is_err(), "lone high surrogate should error");
    }

    #[test]
    fn parse_string_rejects_lone_low_surrogate() {
        let result = parse_json_value(r#""\uDE00""#);
        assert!(result.is_err(), "lone low surrogate should error");
    }

    #[test]
    fn json_escape_escapes_control_characters_below_0x20() {
        let escaped = json_escape("a\x1bb\x07c");
        assert_eq!(escaped, "a\\u001bb\\u0007c");
    }

    #[test]
    fn json_escape_handles_b_and_f() {
        let escaped = json_escape("\u{0008}\u{000C}");
        assert_eq!(escaped, "\\b\\f");
    }

    #[test]
    fn json_value_to_string_round_trips_nested_array() {
        let input = r#"[{"k":"v","n":42},[1,2],null,true]"#;
        let parsed = parse_json_value(input).unwrap();
        let rewritten = json_value_to_string(&parsed);
        let reparsed = parse_json_value(&rewritten).unwrap();
        let reparsed_str = json_value_to_string(&reparsed);
        assert_eq!(rewritten, reparsed_str);
    }

    #[test]
    fn json_value_to_string_escapes_special_chars_in_strings() {
        let value = JsonValue::String("a\"b\\c\nd\u{0008}".to_string());
        let s = json_value_to_string(&value);
        assert_eq!(s, r#""a\"b\\c\nd\b""#);
    }
}
