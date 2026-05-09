use std::cell::RefCell;
use std::rc::Rc;

use crate::core::todos::{Todo, TodoList, TodoStatus};
use crate::error::{tool_failure, AppResult};
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, parse_json_value, JsonValue,
};

const MAX_ITEMS: usize = 100;

pub struct TodoWriteTool {
    // INVARIANT: this tool's execute() must never call back into the registry while
    // `borrow_mut()` is held. Phase 10b sub-agent dispatch may need to switch to
    // Cell<Vec<Todo>> + take/replace if that invariant changes.
    pub list: Rc<RefCell<TodoList>>,
}

impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let raw_items = input.get("items").ok_or_else(|| {
            tool_failure(
                "todo_write expects an `items` field with a JSON array of \
                 {content, activeForm, status} objects",
            )
        })?;

        let parsed = parse_json_value(raw_items)
            .map_err(|e| tool_failure(format!("malformed todo items JSON: {e}")))?;
        let array = json_as_array(&parsed).ok_or_else(|| {
            tool_failure(format!(
                "`items` must be a JSON array, got {kind}",
                kind = describe_kind(&parsed),
            ))
        })?;

        if array.len() > MAX_ITEMS {
            return Err(tool_failure(format!(
                "too many todos (got {}, max {MAX_ITEMS})",
                array.len()
            )));
        }

        let mut new_items: Vec<Todo> = Vec::with_capacity(array.len());
        for (index, value) in array.iter().enumerate() {
            let obj = json_as_object(value).ok_or_else(|| {
                tool_failure(format!("todo at index {index} must be a JSON object"))
            })?;

            let content = obj
                .get("content")
                .and_then(json_as_string)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    tool_failure(format!("todo at index {index} missing field `content`"))
                })?
                .to_string();

            let active_form = obj
                .get("activeForm")
                .and_then(json_as_string)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    tool_failure(format!("todo at index {index} missing field `activeForm`"))
                })?
                .to_string();

            let status_str = obj.get("status").and_then(json_as_string).ok_or_else(|| {
                tool_failure(format!("todo at index {index} missing field `status`"))
            })?;
            let status = TodoStatus::from_label(status_str).ok_or_else(|| {
                tool_failure(format!(
                    "todo at index {index}: status must be pending|in_progress|completed (got `{status_str}`)"
                ))
            })?;

            new_items.push(Todo {
                content,
                active_form,
                status,
            });
        }

        let mut list = self.list.borrow_mut();
        list.replace(new_items);
        let summary = list.render_for_display();
        Ok(ToolOutput { summary })
    }
}

fn describe_kind(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_list() -> Rc<RefCell<TodoList>> {
        Rc::new(RefCell::new(TodoList::default()))
    }

    fn execute(items_json: &str) -> AppResult<(ToolOutput, Rc<RefCell<TodoList>>)> {
        let list = fresh_list();
        let tool = TodoWriteTool { list: list.clone() };
        let mut input = ToolInput::new();
        input
            .args
            .insert("items".to_string(), items_json.to_string());
        let output = tool.execute(input)?;
        Ok((output, list))
    }

    #[test]
    fn execute_succeeds_with_valid_items_array() {
        let body = r#"[
            {"content":"A","activeForm":"Aing","status":"pending"},
            {"content":"B","activeForm":"Bing","status":"in_progress"}
        ]"#;
        let (output, list) = execute(body).unwrap();
        assert!(output.summary.contains("2 todos"));
        let inner = list.borrow();
        assert_eq!(inner.items.len(), 2);
        assert_eq!(inner.items[0].content, "A");
        assert_eq!(inner.items[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn execute_fails_when_items_missing() {
        let list = fresh_list();
        let tool = TodoWriteTool { list };
        let input = ToolInput::new();
        let err = tool.execute(input).unwrap_err();
        assert!(err.to_string().contains("expects an `items` field"));
    }

    #[test]
    fn execute_fails_when_items_not_valid_json() {
        let err = execute("[not_json").unwrap_err();
        assert!(err.to_string().contains("malformed todo items JSON"));
    }

    #[test]
    fn execute_fails_when_todo_missing_content() {
        let body = r#"[{"activeForm":"Aing","status":"pending"}]"#;
        let err = execute(body).unwrap_err();
        assert!(err.to_string().contains("missing field `content`"));
    }

    #[test]
    fn execute_fails_when_status_invalid() {
        let body = r#"[{"content":"A","activeForm":"Aing","status":"unknown"}]"#;
        let err = execute(body).unwrap_err();
        assert!(err
            .to_string()
            .contains("must be pending|in_progress|completed"));
    }

    #[test]
    fn execute_fails_when_too_many_items() {
        let entry = r#"{"content":"X","activeForm":"Xing","status":"pending"}"#;
        let body = format!("[{}]", vec![entry; 101].join(","));
        let err = execute(&body).unwrap_err();
        assert!(err.to_string().contains("too many todos"));
    }
}
