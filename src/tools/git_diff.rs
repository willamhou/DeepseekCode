use std::process::Command;

use crate::error::AppResult;
use crate::tools::types::{Tool, ToolInput, ToolOutput};

pub struct GitDiffTool;

impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn execute(&self, _input: ToolInput) -> AppResult<ToolOutput> {
        let output = Command::new("git").args(["diff", "--", "."]).output()?;
        let summary = if output.stdout.is_empty() {
            "No local diff.".to_string()
        } else {
            String::from_utf8_lossy(&output.stdout).to_string()
        };

        Ok(ToolOutput { summary })
    }
}
