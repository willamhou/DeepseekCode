use crate::error::AppResult;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use std::fs;
use std::path::{Path, PathBuf};

pub struct ListFilesTool;

impl Tool for ListFilesTool {
    fn name(&self) -> &'static str {
        "list_files"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let root = input.get("root").unwrap_or(".");
        let max_depth = input
            .get("max_depth")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3);
        let limit = input
            .get("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(40);

        let mut files = Vec::new();
        visit(Path::new(root), Path::new(root), 0, max_depth, limit, &mut files)?;

        Ok(ToolOutput {
            summary: if files.is_empty() {
                "No files found.".to_string()
            } else {
                files.join("\n")
            },
        })
    }
}

fn visit(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    limit: usize,
    files: &mut Vec<String>,
) -> AppResult<()> {
    if files.len() >= limit || depth > max_depth {
        return Ok(());
    }

    let mut entries = fs::read_dir(current)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect::<Vec<PathBuf>>();
    entries.sort();

    for path in entries {
        if files.len() >= limit {
            break;
        }

        let name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
        if should_skip(name) {
            continue;
        }

        let display = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();

        if path.is_dir() {
            files.push(format!("{display}/"));
            visit(root, &path, depth + 1, max_depth, limit, files)?;
        } else {
            files.push(display);
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
