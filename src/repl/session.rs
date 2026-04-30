use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::types::AppConfig;
use crate::error::{app_error, AppResult};
use crate::model::protocol::ObservationStatus;
use crate::repl::repl::Repl;
use crate::repl::slash::validate_session_name;
use crate::repl::transcript::{Transcript, TurnRole};
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, parse_root_object, write_kv_null,
    write_kv_string, write_kv_u64, JsonValue,
};

pub const SESSION_VERSION: u32 = 1;

pub fn save(name: &str, repl: &Repl) -> AppResult<PathBuf> {
    validate_session_name(name).map_err(app_error)?;
    let dir = sessions_dir(&repl.config);
    std::fs::create_dir_all(&dir)?;
    let final_path = dir.join(format!("{name}.json"));
    let temp_path = dir.join(format!("{name}.json.tmp"));
    let body = serialize_session(name, repl);
    std::fs::write(&temp_path, body)?;
    std::fs::rename(&temp_path, &final_path)?;
    Ok(final_path)
}

pub fn load(name: &str, config: &AppConfig) -> AppResult<Repl> {
    validate_session_name(name).map_err(app_error)?;
    let path = sessions_dir(config).join(format!("{name}.json"));
    if !path.exists() {
        return Err(app_error(format!(
            "session not found: {}",
            path.display()
        )));
    }
    let content = std::fs::read_to_string(&path)?;
    parse_session(config, &content)
}

fn sessions_dir(config: &AppConfig) -> PathBuf {
    PathBuf::from(&config.workspace.session_dir)
}

pub fn serialize_session(name: &str, repl: &Repl) -> String {
    let mut out = String::from("{");
    write_kv_u64(&mut out, "version", SESSION_VERSION as u64, false);
    write_kv_string(&mut out, "name", name, true);
    write_kv_string(&mut out, "saved_at", &current_epoch_label(), true);
    match &repl.skill {
        Some(s) => write_kv_string(&mut out, "skill", s, true),
        None => write_kv_null(&mut out, "skill", true),
    }
    write_kv_u64(&mut out, "budget", repl.budget as u64, true);
    out.push_str(",\"transcript\":[");
    for (i, turn) in repl.transcript.turns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_turn(&mut out, turn);
    }
    out.push_str("],\"tokens\":{");
    write_kv_u64(&mut out, "prompt", repl.tokens_prompt, false);
    write_kv_u64(&mut out, "completion", repl.tokens_completion, true);
    out.push('}');
    out.push('}');
    out
}

fn write_turn(out: &mut String, turn: &crate::repl::transcript::Turn) {
    out.push('{');
    let role_str = match turn.role {
        TurnRole::User => "user",
        TurnRole::Assistant => "assistant",
        TurnRole::Tool => "tool",
    };
    write_kv_string(out, "role", role_str, false);
    match turn.role {
        TurnRole::User | TurnRole::Assistant => {
            write_kv_string(out, "content", &turn.content, true);
        }
        TurnRole::Tool => {
            write_kv_string(out, "name", turn.tool_name.as_deref().unwrap_or(""), true);
            out.push_str(",\"input\":{");
            if let Some(input) = &turn.tool_input {
                for (i, (k, v)) in input.iter().enumerate() {
                    write_kv_string(out, k, v, i > 0);
                }
            }
            out.push('}');
            write_kv_string(
                out,
                "output",
                turn.tool_output.as_deref().unwrap_or(""),
                true,
            );
            let status_str = match turn.status {
                ObservationStatus::Ok => "ok",
                ObservationStatus::Failed => "failed",
            };
            write_kv_string(out, "status", status_str, true);
        }
    }
    out.push('}');
}

fn current_epoch_label() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch+{secs}")
}

pub fn parse_session(config: &AppConfig, content: &str) -> AppResult<Repl> {
    let root = parse_root_object(content)?;

    let version = root
        .get("version")
        .and_then(json_as_u64)
        .ok_or_else(|| app_error("session missing or non-numeric `version`"))?;
    if version != SESSION_VERSION as u64 {
        return Err(app_error(format!(
            "unsupported session version: {version} (expected {SESSION_VERSION})"
        )));
    }

    let budget = root
        .get("budget")
        .and_then(json_as_u64)
        .ok_or_else(|| app_error("session missing `budget`"))? as usize;
    if budget == 0 || budget > 200 {
        return Err(app_error(format!(
            "session budget out of range: {budget}"
        )));
    }

    let skill = match root.get("skill") {
        Some(JsonValue::Null) => None,
        Some(value) => Some(
            json_as_string(value)
                .ok_or_else(|| app_error("session `skill` must be string or null"))?
                .to_string(),
        ),
        None => return Err(app_error("session missing `skill` (use null to clear)")),
    };

    let transcript_raw = root
        .get("transcript")
        .and_then(json_as_array)
        .ok_or_else(|| app_error("session missing `transcript` array"))?;
    let mut transcript = Transcript::default();
    for value in transcript_raw {
        let turn = parse_turn(value)?;
        transcript.turns.push(turn);
    }

    let tokens_obj = root
        .get("tokens")
        .and_then(json_as_object)
        .ok_or_else(|| app_error("session missing `tokens`"))?;
    let tokens_prompt = tokens_obj
        .get("prompt")
        .and_then(json_as_u64)
        .ok_or_else(|| app_error("session tokens missing `prompt`"))?;
    let tokens_completion = tokens_obj
        .get("completion")
        .and_then(json_as_u64)
        .ok_or_else(|| app_error("session tokens missing `completion`"))?;

    let mut repl = Repl::new(config.clone(), skill);
    repl.budget = budget;
    repl.transcript = transcript;
    repl.tokens_prompt = tokens_prompt;
    repl.tokens_completion = tokens_completion;
    Ok(repl)
}

fn parse_turn(value: &JsonValue) -> AppResult<crate::repl::transcript::Turn> {
    let map = json_as_object(value).ok_or_else(|| app_error("turn must be a json object"))?;
    let role = map
        .get("role")
        .and_then(json_as_string)
        .ok_or_else(|| app_error("turn missing `role`"))?;
    match role {
        "user" => {
            let content = map
                .get("content")
                .and_then(json_as_string)
                .ok_or_else(|| app_error("user turn missing `content`"))?
                .to_string();
            Ok(crate::repl::transcript::Turn {
                role: TurnRole::User,
                content,
                tool_name: None,
                tool_input: None,
                tool_output: None,
                status: ObservationStatus::Ok,
            })
        }
        "assistant" => {
            let content = map
                .get("content")
                .and_then(json_as_string)
                .ok_or_else(|| app_error("assistant turn missing `content`"))?
                .to_string();
            Ok(crate::repl::transcript::Turn {
                role: TurnRole::Assistant,
                content,
                tool_name: None,
                tool_input: None,
                tool_output: None,
                status: ObservationStatus::Ok,
            })
        }
        "tool" => {
            let name = map
                .get("name")
                .and_then(json_as_string)
                .ok_or_else(|| app_error("tool turn missing `name`"))?
                .to_string();
            let input_obj = map
                .get("input")
                .and_then(json_as_object)
                .ok_or_else(|| app_error("tool turn missing `input` object"))?;
            let mut input = BTreeMap::new();
            for (k, v) in input_obj {
                let s = json_as_string(v).ok_or_else(|| {
                    app_error(format!("tool input `{k}` must be a string"))
                })?;
                input.insert(k.clone(), s.to_string());
            }
            let output = map
                .get("output")
                .and_then(json_as_string)
                .ok_or_else(|| app_error("tool turn missing `output`"))?
                .to_string();
            let status_str = map
                .get("status")
                .and_then(json_as_string)
                .ok_or_else(|| app_error("tool turn missing `status`"))?;
            let status = match status_str {
                "ok" => ObservationStatus::Ok,
                "failed" => ObservationStatus::Failed,
                other => {
                    return Err(app_error(format!(
                        "tool turn has unknown status `{other}`"
                    )))
                }
            };
            Ok(crate::repl::transcript::Turn {
                role: TurnRole::Tool,
                content: String::new(),
                tool_name: Some(name),
                tool_input: Some(input),
                tool_output: Some(output),
                status,
            })
        }
        other => Err(app_error(format!("unknown turn role `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::AppConfig;

    fn config_with_temp_session_dir() -> (AppConfig, TempDir) {
        let dir = TempDir::new();
        let mut cfg = AppConfig::default();
        cfg.workspace.session_dir = dir.path().to_string_lossy().into_owned();
        (cfg, dir)
    }

    fn fixture_repl() -> Repl {
        let mut r = Repl::new(AppConfig::default(), Some("pr-review".to_string()));
        r.budget = 30;
        r.transcript.push_user("hello");
        r.transcript.push_assistant("world");
        let mut input = BTreeMap::new();
        input.insert("path".to_string(), "x.rs".to_string());
        r.transcript
            .push_tool("read_file", input, "contents", ObservationStatus::Ok);
        r.tokens_prompt = 12;
        r.tokens_completion = 5;
        r
    }

    #[test]
    fn save_load_round_trip_preserves_render_for_prompt() {
        let (cfg, _tmp) = config_with_temp_session_dir();
        let mut original = fixture_repl();
        original.config = cfg.clone();
        let original_render = original.transcript.render_for_prompt();
        save("e2e-render", &original).unwrap();

        let loaded = load("e2e-render", &cfg).unwrap();
        let loaded_render = loaded.transcript.render_for_prompt();

        assert_eq!(
            original_render, loaded_render,
            "render_for_prompt must be byte-identical across save/load"
        );
        assert!(loaded_render.contains("[user 1]: hello"));
        assert!(loaded_render.contains("[assistant 1]: world"));
        assert!(loaded_render.contains("[tool] read_file(path=x.rs) -> ok"));
        assert_eq!(loaded.tokens_prompt + loaded.tokens_completion, 17);
    }

    #[test]
    fn save_then_load_round_trip_preserves_state() {
        let (cfg, _tmp) = config_with_temp_session_dir();
        let mut original = fixture_repl();
        original.config = cfg.clone();
        let path = save("my-session", &original).unwrap();
        assert!(path.exists());

        let loaded = load("my-session", &cfg).unwrap();
        assert_eq!(loaded.budget, 30);
        assert_eq!(loaded.skill.as_deref(), Some("pr-review"));
        assert_eq!(loaded.transcript.turns.len(), 3);
        assert_eq!(loaded.transcript.turns[0].content, "hello");
        assert_eq!(loaded.transcript.turns[1].content, "world");
        assert_eq!(
            loaded.transcript.turns[2].tool_name.as_deref(),
            Some("read_file"),
        );
        assert_eq!(loaded.tokens_prompt, 12);
        assert_eq!(loaded.tokens_completion, 5);
    }

    #[test]
    fn parse_session_rejects_unknown_version() {
        let cfg = AppConfig::default();
        let body = r#"{"version":99,"name":"x","saved_at":"-","skill":null,"budget":20,"transcript":[],"tokens":{"prompt":0,"completion":0}}"#;
        let err = parse_session(&cfg, body).unwrap_err();
        assert!(err.to_string().contains("unsupported session version"));
    }

    #[test]
    fn parse_session_rejects_unknown_role() {
        let cfg = AppConfig::default();
        let body = r#"{"version":1,"name":"x","saved_at":"-","skill":null,"budget":20,"transcript":[{"role":"system","content":"hi"}],"tokens":{"prompt":0,"completion":0}}"#;
        let err = parse_session(&cfg, body).unwrap_err();
        assert!(err.to_string().contains("unknown turn role"));
    }

    #[test]
    fn parse_session_rejects_out_of_range_budget() {
        let cfg = AppConfig::default();
        let body = r#"{"version":1,"name":"x","saved_at":"-","skill":null,"budget":999,"transcript":[],"tokens":{"prompt":0,"completion":0}}"#;
        let err = parse_session(&cfg, body).unwrap_err();
        assert!(err.to_string().contains("budget out of range"));
    }

    #[test]
    fn load_returns_error_when_file_missing() {
        let (cfg, _tmp) = config_with_temp_session_dir();
        let err = load("does-not-exist", &cfg).unwrap_err();
        assert!(err.to_string().contains("session not found"));
    }

    #[test]
    fn save_creates_session_directory_if_missing() {
        let (cfg, tmp) = config_with_temp_session_dir();
        std::fs::remove_dir_all(tmp.path()).ok();
        let mut r = fixture_repl();
        r.config = cfg.clone();
        let path = save("first", &r).unwrap();
        assert!(path.exists());
    }

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("dscode_session_test_{nanos}"));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        pub fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
