use std::env;
use std::process::Command;
use std::time::Instant;

use crate::cli::app::{SmokeArgs, SmokeFlavor};
use crate::config::load::load_or_default;
use crate::config::types::ModelConfig;
use crate::error::AppResult;
use crate::error::app_error;

const DEFAULT_PROMPT: &str = "Reply with the single word `pong` and nothing else.";
const MAX_TOKENS: u32 = 16;

pub fn run(args: SmokeArgs) -> AppResult<()> {
    let config = load_or_default()?;
    let model = config.model;
    let flavor = args.flavor.unwrap_or_else(|| detect_flavor(&model.base_url));
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_PROMPT.to_string());

    println!("DeepseekCode smoke");
    println!("  flavor: {}", flavor_label(flavor));
    println!("  base_url: {}", model.base_url);
    println!("  model: {}", model.model);
    let endpoint = endpoint_for(flavor, &model.base_url);
    println!("  endpoint: {endpoint}");
    println!("  prompt: {prompt}");

    let api_key = match env::var(&model.api_key_env) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => {
            return Err(app_error(format!(
                "{} is not set; export it before running `dscode smoke`",
                model.api_key_env
            )));
        }
    };

    let body = build_request_body(flavor, &model, &prompt);
    let started = Instant::now();
    let probe = call_remote(flavor, &endpoint, &api_key, &body)?;
    let elapsed = started.elapsed();

    println!();
    println!("[result]");
    println!("  http_status: {}", probe.status);
    println!("  duration_ms: {}", elapsed.as_millis());

    if !is_success_status(&probe.status) {
        println!("  outcome: failure");
        if !probe.body.trim().is_empty() {
            println!("  body: {}", clip(&probe.body, 400));
        }
        return Err(app_error(format!(
            "smoke request returned non-success status {}",
            probe.status
        )));
    }

    let reply = extract_reply(flavor, &probe.body)
        .unwrap_or_else(|| "(no text content returned)".to_string());
    println!("  outcome: success");
    println!("  reply: {}", clip(&reply, 200));
    Ok(())
}

fn detect_flavor(base_url: &str) -> SmokeFlavor {
    if base_url.trim_end_matches('/').ends_with("/anthropic") {
        SmokeFlavor::Anthropic
    } else {
        SmokeFlavor::OpenAi
    }
}

fn flavor_label(flavor: SmokeFlavor) -> &'static str {
    match flavor {
        SmokeFlavor::OpenAi => "OpenAI-compatible",
        SmokeFlavor::Anthropic => "Anthropic-compatible",
    }
}

fn endpoint_for(flavor: SmokeFlavor, base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let normalized = match flavor {
        SmokeFlavor::OpenAi => trimmed.trim_end_matches("/anthropic").to_string(),
        SmokeFlavor::Anthropic => {
            if trimmed.ends_with("/anthropic") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/anthropic")
            }
        }
    };

    match flavor {
        SmokeFlavor::OpenAi => format!("{normalized}/chat/completions"),
        SmokeFlavor::Anthropic => format!("{normalized}/messages"),
    }
}

fn build_request_body(flavor: SmokeFlavor, model: &ModelConfig, prompt: &str) -> String {
    match flavor {
        SmokeFlavor::OpenAi => format!(
            concat!(
                "{{",
                "\"model\":\"{model}\",",
                "\"temperature\":0,",
                "\"max_tokens\":{max_tokens},",
                "\"messages\":[",
                "{{\"role\":\"user\",\"content\":\"{prompt}\"}}",
                "]",
                "}}"
            ),
            model = json_escape(&model.model),
            max_tokens = MAX_TOKENS,
            prompt = json_escape(prompt),
        ),
        SmokeFlavor::Anthropic => format!(
            concat!(
                "{{",
                "\"model\":\"{model}\",",
                "\"max_tokens\":{max_tokens},",
                "\"messages\":[",
                "{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{prompt}\"}}]}}",
                "]",
                "}}"
            ),
            model = json_escape(&model.model),
            max_tokens = MAX_TOKENS,
            prompt = json_escape(prompt),
        ),
    }
}

struct ProbeResponse {
    status: String,
    body: String,
}

fn call_remote(
    flavor: SmokeFlavor,
    endpoint: &str,
    api_key: &str,
    body: &str,
) -> AppResult<ProbeResponse> {
    let separator = "---SMOKE-STATUS---";
    let write_format = format!("\n{separator}%{{http_code}}");
    let mut command = Command::new("curl");
    command.args([
        "-sS",
        "--max-time",
        "20",
        "-X",
        "POST",
        endpoint,
        "-H",
        "Content-Type: application/json",
        "-w",
        &write_format,
    ]);

    match flavor {
        SmokeFlavor::OpenAi => {
            command.args(["-H", &format!("Authorization: Bearer {api_key}")]);
        }
        SmokeFlavor::Anthropic => {
            command.args(["-H", &format!("x-api-key: {api_key}")]);
            command.args(["-H", "anthropic-version: 2023-06-01"]);
        }
    }

    command.args(["--data-binary", body]);

    let output = match command.output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(app_error(
                "curl is not installed; install curl to use `dscode smoke`",
            ));
        }
        Err(error) => return Err(app_error(format!("curl failed to start: {error}"))),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            "curl exited with non-zero status".to_string()
        } else {
            stderr
        };
        return Err(app_error(format!("smoke request failed: {message}")));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let (body, status) = split_status(&stdout, separator)
        .ok_or_else(|| app_error("smoke request did not include http status footer"))?;

    Ok(ProbeResponse { status, body })
}

fn split_status(payload: &str, separator: &str) -> Option<(String, String)> {
    let index = payload.rfind(separator)?;
    let body = payload[..index].trim_end_matches('\n').to_string();
    let status = payload[index + separator.len()..].trim().to_string();
    if status.is_empty() {
        return None;
    }
    Some((body, status))
}

fn is_success_status(status: &str) -> bool {
    status.starts_with('2')
}

fn extract_reply(flavor: SmokeFlavor, body: &str) -> Option<String> {
    match flavor {
        SmokeFlavor::OpenAi => extract_openai_reply(body),
        SmokeFlavor::Anthropic => extract_anthropic_reply(body),
    }
}

fn extract_openai_reply(body: &str) -> Option<String> {
    extract_first_string_after_path(body, &["choices", "message", "content"])
}

fn extract_anthropic_reply(body: &str) -> Option<String> {
    extract_first_string_after_path(body, &["content", "text"])
}

fn extract_first_string_after_path(body: &str, path_keys: &[&str]) -> Option<String> {
    let mut cursor = 0;
    for key in path_keys {
        cursor += find_key_position(&body[cursor..], key)?;
    }
    let after = &body[cursor..];
    let quote = after.find('"')?;
    decode_json_string(&after[quote + 1..])
}

fn find_key_position(slice: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\"");
    let mut start = 0;
    while let Some(local) = slice[start..].find(&needle) {
        let absolute = start + local;
        let after = absolute + needle.len();
        let rest = &slice[after..];
        let next_non_ws = rest.bytes().position(|byte| !byte.is_ascii_whitespace());
        if let Some(offset) = next_non_ws {
            if rest.as_bytes()[offset] == b':' {
                return Some(after + offset + 1);
            }
        }
        start = after;
    }
    None
}

fn decode_json_string(slice: &str) -> Option<String> {
    let mut output = String::new();
    let bytes = slice.as_bytes();
    let mut index = 0;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            match byte {
                b'n' => output.push('\n'),
                b'r' => output.push('\r'),
                b't' => output.push('\t'),
                b'\\' => output.push('\\'),
                b'"' => output.push('"'),
                _ => output.push(byte as char),
            }
            escaped = false;
            index += 1;
            continue;
        }

        match byte {
            b'\\' => escaped = true,
            b'"' => return Some(output),
            _ => output.push(byte as char),
        }
        index += 1;
    }

    None
}

fn clip(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let head = trimmed.chars().take(limit).collect::<String>();
    format!("{head}…")
}

fn json_escape(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_anthropic_when_base_url_has_suffix() {
        assert!(matches!(
            detect_flavor("https://api.deepseek.com/anthropic"),
            SmokeFlavor::Anthropic
        ));
        assert!(matches!(
            detect_flavor("https://api.deepseek.com"),
            SmokeFlavor::OpenAi
        ));
    }

    #[test]
    fn endpoint_for_normalizes_paths() {
        assert_eq!(
            endpoint_for(SmokeFlavor::OpenAi, "https://api.deepseek.com"),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            endpoint_for(SmokeFlavor::OpenAi, "https://api.deepseek.com/anthropic"),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            endpoint_for(SmokeFlavor::Anthropic, "https://api.deepseek.com"),
            "https://api.deepseek.com/anthropic/messages"
        );
        assert_eq!(
            endpoint_for(SmokeFlavor::Anthropic, "https://api.deepseek.com/anthropic"),
            "https://api.deepseek.com/anthropic/messages"
        );
    }

    #[test]
    fn build_request_body_contains_prompt_and_model() {
        let model = ModelConfig {
            base_url: "https://api.deepseek.com".to_string(),
            model: "deepseek-chat".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
        };
        let openai = build_request_body(SmokeFlavor::OpenAi, &model, "ping");
        assert!(openai.contains("\"model\":\"deepseek-chat\""));
        assert!(openai.contains("\"role\":\"user\""));
        assert!(openai.contains("\"content\":\"ping\""));

        let anthropic = build_request_body(SmokeFlavor::Anthropic, &model, "ping");
        assert!(anthropic.contains("\"model\":\"deepseek-chat\""));
        assert!(anthropic.contains("\"type\":\"text\""));
        assert!(anthropic.contains("\"text\":\"ping\""));
    }

    #[test]
    fn split_status_separates_body_and_code() {
        let payload = "{\"choices\":[]}\n---SEP---200";
        let (body, status) = split_status(payload, "---SEP---").unwrap();
        assert_eq!(body, "{\"choices\":[]}");
        assert_eq!(status, "200");
    }

    #[test]
    fn split_status_returns_none_when_separator_missing() {
        assert!(split_status("no separator here", "---SEP---").is_none());
    }

    #[test]
    fn extract_openai_reply_finds_assistant_text() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#;
        assert_eq!(extract_openai_reply(body).as_deref(), Some("pong"));
    }

    #[test]
    fn extract_openai_reply_decodes_escape_sequences() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"line1\nline2"}}]}"#;
        assert_eq!(
            extract_openai_reply(body).as_deref(),
            Some("line1\nline2")
        );
    }

    #[test]
    fn extract_anthropic_reply_finds_first_text_block() {
        let body = r#"{"id":"msg_1","content":[{"type":"text","text":"pong"}]}"#;
        assert_eq!(extract_anthropic_reply(body).as_deref(), Some("pong"));
    }

    #[test]
    fn is_success_status_recognizes_2xx() {
        assert!(is_success_status("200"));
        assert!(is_success_status("204"));
        assert!(!is_success_status("400"));
        assert!(!is_success_status("500"));
    }

    #[test]
    fn clip_truncates_long_values() {
        let long = "a".repeat(300);
        let clipped = clip(&long, 200);
        assert!(clipped.ends_with('…'));
        assert_eq!(clipped.chars().count(), 201);
    }
}
