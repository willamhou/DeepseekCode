use crate::error::app_error;
use crate::error::AppResult;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use std::fs;
use std::path::Path;

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let path = input
            .get("path")
            .ok_or_else(|| app_error("read_file requires a path"))?;
        let path = Path::new(path);

        if path.is_dir() {
            return Err(app_error("read_file path points to a directory"));
        }

        let content = fs::read_to_string(path)?;
        let max_lines = input
            .get("max_lines")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(80);

        let excerpt = content
            .lines()
            .take(max_lines)
            .enumerate()
            .map(|(index, line)| format!("{:>4} {}", index + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ToolOutput { summary: excerpt })
    }
}
