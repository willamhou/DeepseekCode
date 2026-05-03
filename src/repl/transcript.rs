use std::collections::BTreeMap;

use crate::model::protocol::ObservationStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<BTreeMap<String, String>>,
    pub tool_output: Option<String>,
    pub status: ObservationStatus,
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub turns: Vec<Turn>,
}

impl Transcript {
    pub fn push_user(&mut self, content: impl Into<String>) {
        self.turns.push(Turn {
            role: TurnRole::User,
            content: content.into(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            status: ObservationStatus::Ok,
        });
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.turns.push(Turn {
            role: TurnRole::Assistant,
            content: content.into(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            status: ObservationStatus::Ok,
        });
    }

    pub fn push_tool(
        &mut self,
        name: impl Into<String>,
        input: BTreeMap<String, String>,
        output: impl Into<String>,
        status: ObservationStatus,
    ) {
        self.turns.push(Turn {
            role: TurnRole::Tool,
            content: String::new(),
            tool_name: Some(name.into()),
            tool_input: Some(input),
            tool_output: Some(output.into()),
            status,
        });
    }

    pub fn clear(&mut self) {
        self.turns.clear();
    }
}

use crate::core::observations::summarize_for_kind;
use crate::model::protocol::ObservationKind;

const RECENT_ASSISTANT_TURNS_KEPT_FULL: usize = 3;

impl Transcript {
    pub fn render_for_prompt(&self) -> String {
        if self.turns.is_empty() {
            return String::new();
        }
        let assistant_indices: Vec<usize> = self
            .turns
            .iter()
            .enumerate()
            .filter_map(|(i, t)| (t.role == TurnRole::Assistant).then_some(i))
            .collect();
        let keep_full_after = assistant_indices
            .len()
            .saturating_sub(RECENT_ASSISTANT_TURNS_KEPT_FULL);
        let assistants_kept_full: std::collections::BTreeSet<usize> = assistant_indices
            .iter()
            .skip(keep_full_after)
            .copied()
            .collect();

        let mut user_n = 0usize;
        let mut assistant_n = 0usize;
        let mut out = String::from("Conversation so far:\n\n");

        for (i, turn) in self.turns.iter().enumerate() {
            match turn.role {
                TurnRole::User => {
                    user_n += 1;
                    out.push_str(&format!("[user {user_n}]: {}\n\n", turn.content));
                }
                TurnRole::Assistant => {
                    assistant_n += 1;
                    if assistants_kept_full.contains(&i) {
                        out.push_str(&format!("[assistant {assistant_n}]: {}\n\n", turn.content));
                    } else {
                        let head = turn
                            .content
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim();
                        out.push_str(&format!(
                            "[assistant {assistant_n}]: {head} (truncated assistant turn {assistant_n})\n\n",
                        ));
                    }
                }
                TurnRole::Tool => {
                    let name = turn.tool_name.as_deref().unwrap_or("?");
                    let kind = ObservationKind::from_tool_name(name);
                    let trimmed_output = turn
                        .tool_output
                        .as_ref()
                        .map(|o| summarize_for_kind(o, kind))
                        .unwrap_or_default();
                    let status_label = match turn.status {
                        crate::model::protocol::ObservationStatus::Ok => "ok",
                        crate::model::protocol::ObservationStatus::Failed => "failed",
                    };
                    let input_repr = if name == "todo_write" {
                        turn.tool_input
                            .as_ref()
                            .and_then(|m| m.get("items"))
                            .and_then(|s| crate::util::json::parse_json_value(s).ok())
                            .and_then(|v| match v {
                                crate::util::json::JsonValue::Array(a) => {
                                    Some(format!("items=<{} todos>", a.len()))
                                }
                                _ => None,
                            })
                            .unwrap_or_else(|| "items=<malformed>".to_string())
                    } else {
                        turn.tool_input
                            .as_ref()
                            .map(|map| {
                                let parts: Vec<String> = map
                                    .iter()
                                    .map(|(k, v)| format!("{k}={v}"))
                                    .collect();
                                parts.join(", ")
                            })
                            .unwrap_or_default()
                    };
                    out.push_str(&format!(
                        "[tool] {name}({input_repr}) -> {status_label}\n{trimmed_output}\n\n",
                    ));
                }
            }
        }

        out.push_str("(end of conversation; respond to the latest user message above)\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_user_records_a_user_turn() {
        let mut t = Transcript::default();
        t.push_user("hello");
        assert_eq!(t.turns.len(), 1);
        assert_eq!(t.turns[0].role, TurnRole::User);
        assert_eq!(t.turns[0].content, "hello");
    }

    #[test]
    fn push_assistant_records_an_assistant_turn() {
        let mut t = Transcript::default();
        t.push_assistant("done");
        assert_eq!(t.turns.last().unwrap().role, TurnRole::Assistant);
    }

    #[test]
    fn push_tool_records_input_and_output() {
        let mut t = Transcript::default();
        let mut input = BTreeMap::new();
        input.insert("path".to_string(), "x.rs".to_string());
        t.push_tool("read_file", input, "contents", ObservationStatus::Ok);
        let last = t.turns.last().unwrap();
        assert_eq!(last.role, TurnRole::Tool);
        assert_eq!(last.tool_name.as_deref(), Some("read_file"));
        assert_eq!(last.tool_output.as_deref(), Some("contents"));
        assert_eq!(
            last.tool_input.as_ref().unwrap().get("path").map(String::as_str),
            Some("x.rs"),
        );
    }

    #[test]
    fn clear_empties_turns() {
        let mut t = Transcript::default();
        t.push_user("a");
        t.push_assistant("b");
        t.clear();
        assert!(t.turns.is_empty());
    }

    #[test]
    fn render_returns_empty_for_empty_transcript() {
        let t = Transcript::default();
        assert!(t.render_for_prompt().is_empty());
    }

    #[test]
    fn render_includes_user_and_assistant_turns() {
        let mut t = Transcript::default();
        t.push_user("ask 1");
        t.push_assistant("answer 1");
        t.push_user("ask 2");
        let rendered = t.render_for_prompt();
        assert!(rendered.contains("[user 1]: ask 1"));
        assert!(rendered.contains("[assistant 1]: answer 1"));
        assert!(rendered.contains("[user 2]: ask 2"));
        assert!(rendered.contains("(end of conversation"));
    }

    #[test]
    fn render_truncates_old_assistant_turns_beyond_three() {
        let mut t = Transcript::default();
        for i in 1..=5 {
            t.push_user(format!("ask {i}"));
            t.push_assistant(format!("long\nbody\nof\nturn\n{i}\nwith\nseveral\nlines"));
        }
        let rendered = t.render_for_prompt();
        assert!(rendered.contains("(truncated assistant turn 1)"));
        assert!(rendered.contains("(truncated assistant turn 2)"));
        assert!(!rendered.contains("(truncated assistant turn 3)"));
        assert!(!rendered.contains("(truncated assistant turn 4)"));
        // The last 3 assistants (3,4,5) keep full body
        assert!(rendered.contains("[assistant 5]: long"));
    }

    #[test]
    fn render_summarises_tool_output_per_kind() {
        let mut t = Transcript::default();
        let mut input = BTreeMap::new();
        input.insert("path".to_string(), "x.rs".to_string());
        let huge: String = (0..200).map(|i| format!("line{i}\n")).collect();
        t.push_tool("read_file", input, huge, ObservationStatus::Ok);
        let rendered = t.render_for_prompt();
        assert!(rendered.contains("[tool] read_file(path=x.rs) -> ok"));
        assert!(rendered.contains("line0"));
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn render_for_prompt_elides_todo_write_input_to_count() {
        let mut transcript = Transcript::default();
        let mut input = std::collections::BTreeMap::new();
        input.insert(
            "items".to_string(),
            r#"[{"content":"A","activeForm":"Aing","status":"pending"},{"content":"B","activeForm":"Bing","status":"in_progress"}]"#.to_string(),
        );
        transcript.push_tool(
            "todo_write",
            input,
            "2 todos: 0 completed, 1 in_progress, 1 pending",
            crate::model::protocol::ObservationStatus::Ok,
        );
        let render = transcript.render_for_prompt();
        assert!(render.contains("items=<2 todos>"));
        assert!(!render.contains(r#""content":"A""#), "raw JSON must be elided: {render}");
    }

    #[test]
    fn render_for_prompt_elides_malformed_todo_write_input_as_malformed() {
        let mut transcript = Transcript::default();
        let mut input = std::collections::BTreeMap::new();
        input.insert("items".to_string(), "[not_json".to_string());
        transcript.push_tool(
            "todo_write",
            input,
            "ok",
            crate::model::protocol::ObservationStatus::Ok,
        );
        let render = transcript.render_for_prompt();
        assert!(render.contains("items=<malformed>"));
    }
}
