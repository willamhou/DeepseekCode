#![allow(dead_code)] // remove when M3 wires up TtyRenderer / M5 wires up /todos slash

use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub content: String,
    pub active_form: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Default)]
pub struct TodoList {
    pub items: Vec<Todo>,
}

impl TodoList {
    pub fn replace(&mut self, items: Vec<Todo>) {
        self.items = items;
    }

    pub fn render_for_prompt(&self) -> String {
        let mut out = String::new();
        for item in &self.items {
            let _ = writeln!(&mut out, "- [{}] {}", item.status.label(), item.content);
        }
        out
    }

    pub fn render_for_display(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.render_compact_summary());
        for item in &self.items {
            let visible = match item.status {
                TodoStatus::InProgress => &item.active_form,
                _ => &item.content,
            };
            let label = format!("[{}]", item.status.label());
            let _ = write!(&mut out, "\n  {label:<14} {visible}");
        }
        out
    }

    pub fn render_compact_summary(&self) -> String {
        if self.items.is_empty() {
            return "no todos".to_string();
        }
        let mut completed = 0usize;
        let mut in_progress = 0usize;
        let mut pending = 0usize;
        for item in &self.items {
            match item.status {
                TodoStatus::Completed => completed += 1,
                TodoStatus::InProgress => in_progress += 1,
                TodoStatus::Pending => pending += 1,
            }
        }
        format!(
            "{total} todos: {completed} completed, {in_progress} in_progress, {pending} pending",
            total = self.items.len(),
        )
    }

    pub fn snapshot(&self) -> Vec<Todo> {
        self.items.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_label_accepts_three_legal_values() {
        assert_eq!(TodoStatus::from_label("pending"), Some(TodoStatus::Pending));
        assert_eq!(TodoStatus::from_label("in_progress"), Some(TodoStatus::InProgress));
        assert_eq!(TodoStatus::from_label("completed"), Some(TodoStatus::Completed));
    }

    #[test]
    fn from_label_returns_none_for_illegal_values() {
        assert_eq!(TodoStatus::from_label("done"), None);
        assert_eq!(TodoStatus::from_label(""), None);
        assert_eq!(TodoStatus::from_label("PENDING"), None);
    }

    #[test]
    fn label_round_trips_with_from_label() {
        for &v in &[TodoStatus::Pending, TodoStatus::InProgress, TodoStatus::Completed] {
            assert_eq!(TodoStatus::from_label(v.label()), Some(v));
        }
    }

    fn make_todo(c: &str, a: &str, s: TodoStatus) -> Todo {
        Todo { content: c.to_string(), active_form: a.to_string(), status: s }
    }

    #[test]
    fn replace_overwrites_existing_items() {
        let mut list = TodoList::default();
        list.replace(vec![make_todo("X", "Xing", TodoStatus::Pending)]);
        assert_eq!(list.items.len(), 1);
        list.replace(vec![
            make_todo("Y", "Ying", TodoStatus::InProgress),
            make_todo("Z", "Zing", TodoStatus::Completed),
        ]);
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[0].content, "Y");
        assert_eq!(list.items[1].content, "Z");
    }

    #[test]
    fn render_for_prompt_uses_status_content_format() {
        let mut list = TodoList::default();
        list.replace(vec![
            make_todo("A", "Aing", TodoStatus::Pending),
            make_todo("B", "Bing", TodoStatus::InProgress),
            make_todo("C", "Cing", TodoStatus::Completed),
        ]);
        let s = list.render_for_prompt();
        assert_eq!(s, "- [pending] A\n- [in_progress] B\n- [completed] C\n");
    }

    #[test]
    fn render_for_display_uses_active_form_for_in_progress_only() {
        let mut list = TodoList::default();
        list.replace(vec![
            make_todo("Run tests", "Running tests", TodoStatus::InProgress),
            make_todo("Refactor", "Refactoring", TodoStatus::Pending),
            make_todo("Read", "Reading", TodoStatus::Completed),
        ]);
        let s = list.render_for_display();
        assert!(s.contains("Running tests"), "in_progress should use active_form: {s}");
        assert!(!s.contains("Refactoring"), "pending should NOT use active_form: {s}");
        assert!(s.contains("Refactor"));
        assert!(!s.contains("Reading"), "completed should NOT use active_form: {s}");
        assert!(s.contains("Read"));
    }

    #[test]
    fn render_compact_summary_counts_each_status() {
        let mut list = TodoList::default();
        list.replace(vec![
            make_todo("A", "Aing", TodoStatus::Completed),
            make_todo("B", "Bing", TodoStatus::Completed),
            make_todo("C", "Cing", TodoStatus::InProgress),
            make_todo("D", "Ding", TodoStatus::Pending),
            make_todo("E", "Eing", TodoStatus::Pending),
        ]);
        let s = list.render_compact_summary();
        assert!(s.contains("5 todos"), "summary: {s}");
        assert!(s.contains("2 completed"), "summary: {s}");
        assert!(s.contains("1 in_progress"), "summary: {s}");
        assert!(s.contains("2 pending"), "summary: {s}");
    }

    #[test]
    fn render_compact_summary_for_empty_list_returns_no_todos() {
        let list = TodoList::default();
        assert_eq!(list.render_compact_summary(), "no todos");
    }

    #[test]
    fn render_for_display_first_line_equals_render_compact_summary() {
        // NEW-1 contract: first line of display == compact summary
        let mut list = TodoList::default();
        list.replace(vec![
            make_todo("X", "Xing", TodoStatus::InProgress),
            make_todo("Y", "Ying", TodoStatus::Pending),
        ]);
        let display = list.render_for_display();
        let summary = list.render_compact_summary();
        let first_line = display.lines().next().unwrap_or("");
        assert_eq!(first_line, summary);
    }

    #[test]
    fn is_empty_tracks_state_changes() {
        let mut list = TodoList::default();
        assert!(list.is_empty());
        list.replace(vec![make_todo("X", "Xing", TodoStatus::Pending)]);
        assert!(!list.is_empty());
        list.replace(vec![]);
        assert!(list.is_empty());
    }
}
