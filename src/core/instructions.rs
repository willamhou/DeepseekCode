use std::path::{Path, PathBuf};

use crate::config::types::WorkspaceConfig;
use crate::error::AppResult;

const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    pub path: PathBuf,
    pub content: String,
    pub truncated: bool,
}

pub fn load_workspace_instructions(
    cwd: &Path,
    workspace: &WorkspaceConfig,
) -> AppResult<Vec<InstructionFile>> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut files = Vec::new();

    if let Some(user_path) = workspace.user_instructions_file() {
        if user_path.is_file() {
            files.push(read_instruction_file(user_path)?);
        }
    }

    let root = find_project_root(&cwd);
    for dir in instruction_dirs(&root, &cwd) {
        if let Some(path) = instruction_file_for_dir(&dir) {
            files.push(read_instruction_file(path)?);
        }
    }

    Ok(files)
}

pub fn render_workspace_instructions(files: &[InstructionFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }

    let mut rendered = String::from(
        "Workspace instructions (loaded before acting; later files are more specific):\n",
    );
    for file in files {
        rendered.push_str(&format!("\n### {}\n", file.path.display()));
        rendered.push_str(file.content.trim());
        rendered.push('\n');
        if file.truncated {
            rendered.push_str("[truncated to 32768 bytes]\n");
        }
    }
    Some(rendered)
}

fn read_instruction_file(path: PathBuf) -> AppResult<InstructionFile> {
    let content = std::fs::read_to_string(&path)?;
    let (content, truncated) = truncate_utf8(&content, MAX_INSTRUCTION_BYTES);
    Ok(InstructionFile {
        path,
        content,
        truncated,
    })
}

fn instruction_file_for_dir(dir: &Path) -> Option<PathBuf> {
    [
        dir.join("AGENTS.override.md"),
        dir.join("AGENTS.md"),
        dir.join("CLAUDE.md"),
        dir.join(".claude/CLAUDE.md"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn instruction_dirs(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut cursor = cwd;
    loop {
        dirs.push(cursor.to_path_buf());
        if cursor == root {
            break;
        }
        let Some(parent) = cursor.parent() else {
            break;
        };
        cursor = parent;
    }
    dirs.reverse();
    dirs
}

fn find_project_root(cwd: &Path) -> PathBuf {
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

fn truncate_utf8(content: &str, max_bytes: usize) -> (String, bool) {
    if content.len() <= max_bytes {
        return (content.to_string(), false);
    }

    let mut end = max_bytes;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (content[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-instructions-{name}-{}-{suffix}",
            std::process::id()
        ))
    }

    #[test]
    fn load_workspace_instructions_orders_user_then_project_chain() {
        let root = temp_root("order");
        let nested = root.join("crates/app");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        let user = root.join("user/AGENTS.md");
        std::fs::create_dir_all(user.parent().unwrap()).unwrap();
        std::fs::write(&user, "user rules").unwrap();
        std::fs::write(root.join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(nested.join("AGENTS.md"), "nested rules").unwrap();

        let mut workspace = WorkspaceConfig::default();
        workspace.user_instructions_file = user.display().to_string();

        let files = load_workspace_instructions(&nested, &workspace).unwrap();

        assert_eq!(
            files
                .iter()
                .map(|file| file.content.as_str())
                .collect::<Vec<_>>(),
            ["user rules", "root rules", "nested rules"],
        );
    }

    #[test]
    fn load_workspace_instructions_prefers_override_then_agents_then_claude() {
        let root = temp_root("precedence");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "base").unwrap();
        std::fs::write(root.join("AGENTS.override.md"), "override").unwrap();

        let mut workspace = WorkspaceConfig::default();
        workspace.user_instructions_file.clear();

        let files = load_workspace_instructions(&root, &workspace).unwrap();
        assert_eq!(files[0].content, "override");

        std::fs::remove_file(root.join("AGENTS.override.md")).unwrap();
        std::fs::remove_file(root.join("AGENTS.md")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "claude").unwrap();

        let files = load_workspace_instructions(&root, &workspace).unwrap();
        assert_eq!(files[0].content, "claude");
    }

    #[test]
    fn render_workspace_instructions_includes_sources_and_truncation_marker() {
        let files = [InstructionFile {
            path: PathBuf::from("AGENTS.md"),
            content: "rule".to_string(),
            truncated: true,
        }];

        let rendered = render_workspace_instructions(&files).unwrap();

        assert!(rendered.contains("### AGENTS.md"));
        assert!(rendered.contains("rule"));
        assert!(rendered.contains("truncated to 32768 bytes"));
    }
}
