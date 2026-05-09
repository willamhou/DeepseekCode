use crate::error::app_error;
use crate::error::AppResult;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use std::fs;
use std::path::{Path, PathBuf};

pub struct SearchTextTool;

impl Tool for SearchTextTool {
    fn name(&self) -> &str {
        "search_text"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let query = input
            .get("query")
            .ok_or_else(|| app_error("search_text requires a query"))?;
        let root = input.get("root").unwrap_or(".");
        let limit = input
            .get("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(20);

        let mut matches = Vec::new();
        search_dir(Path::new(root), Path::new(root), query, limit, &mut matches)?;

        Ok(ToolOutput {
            summary: if matches.is_empty() {
                format!("No matches for `{query}`.")
            } else {
                matches.join("\n")
            },
        })
    }
}

fn search_dir(
    root: &Path,
    current: &Path,
    query: &str,
    limit: usize,
    matches: &mut Vec<String>,
) -> AppResult<()> {
    if matches.len() >= limit {
        return Ok(());
    }

    let mut entries = fs::read_dir(current)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect::<Vec<PathBuf>>();
    entries.sort();

    for path in entries {
        if matches.len() >= limit {
            break;
        }

        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if should_skip(name) {
            continue;
        }

        if path.is_dir() {
            search_dir(root, &path, query, limit, matches)?;
            continue;
        }

        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };

        for (index, line) in content.lines().enumerate() {
            if matches.len() >= limit {
                break;
            }

            if line.contains(query) {
                let display = path.strip_prefix(root).unwrap_or(&path).display();
                matches.push(format!("{display}:{}: {}", index + 1, line.trim()));
            }
        }
    }

    Ok(())
}

fn should_skip(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | "dist" | ".dscode" | "__pycache__"
    )
}
