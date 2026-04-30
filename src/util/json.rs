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

    let mut output = String::new();
    let mut escaped = false;

    while *index < bytes.len() {
        let byte = bytes[*index];
        *index += 1;

        if escaped {
            match byte {
                b'"' => output.push('"'),
                b'\\' => output.push('\\'),
                b'n' => output.push('\n'),
                b'r' => output.push('\r'),
                b't' => output.push('\t'),
                _ => output.push(byte as char),
            }
            escaped = false;
            continue;
        }

        match byte {
            b'\\' => escaped = true,
            b'"' => return Ok(output),
            _ => output.push(byte as char),
        }
    }

    Err(app_error("unterminated json string"))
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
            _ => output.push(ch),
        }
    }
    output
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
}
