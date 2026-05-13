use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::config::types::WorkspaceConfig;
use crate::error::{app_error, AppResult};

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

pub fn init_project_instructions_at(workspace: &Path) -> AppResult<PathBuf> {
    std::fs::create_dir_all(workspace)?;
    ensure_dscode_gitignored(workspace)?;
    let path = workspace.join("AGENTS.md");
    if path.exists() {
        return Err(app_error(format!(
            "AGENTS.md already exists: {}",
            path.display()
        )));
    }
    std::fs::write(&path, render_project_instructions(workspace))?;
    Ok(path)
}

fn ensure_dscode_gitignored(workspace: &Path) -> AppResult<()> {
    if !workspace.join(".git").exists() {
        return Ok(());
    }
    let path = workspace.join(".gitignore");
    let entry = ".dscode/";
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing.lines().any(|line| {
            let trimmed = line.trim();
            trimmed == entry || trimmed == ".dscode"
        }) {
            return Ok(());
        }
        let mut updated = existing;
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(entry);
        updated.push('\n');
        std::fs::write(path, updated)?;
        return Ok(());
    }
    std::fs::write(path, format!("{entry}\n"))?;
    Ok(())
}

fn render_project_instructions(workspace: &Path) -> String {
    let mut doc = String::new();
    doc.push_str("# Project Instructions\n\n");
    doc.push_str("This file provides context for AI coding agents working in this project.\n\n");
    doc.push_str(&detect_project_instructions(workspace));
    doc.push_str("\n## Guidelines\n\n");
    doc.push_str("- Follow existing code style and project conventions.\n");
    doc.push_str("- Keep changes focused and easy to review.\n");
    doc.push_str("- Add or update tests when behavior changes.\n");
    doc.push_str("- Run the relevant formatter, tests, and checks before committing.\n");
    doc.push_str("\n## Notes\n\n");
    doc.push_str("<!-- Add project-specific instructions here. -->\n");
    doc
}

fn detect_project_instructions(workspace: &Path) -> String {
    let mut section = String::new();
    if workspace.join("Cargo.toml").exists() {
        section.push_str("## Project Type: Rust\n\n");
        section.push_str("### Common Commands\n\n");
        section.push_str("- Build: `cargo build`\n");
        section.push_str("- Test: `cargo test`\n");
        section.push_str("- Check: `cargo check`\n");
        section.push_str("- Format: `cargo fmt`\n");
        section.push_str("- Lint: `cargo clippy`\n");
        if let Some(name) = read_cargo_package_name(&workspace.join("Cargo.toml")) {
            let _ = write!(section, "\n### Package\n\n{name}\n");
        }
    } else if workspace.join("package.json").exists() {
        section.push_str("## Project Type: JavaScript / TypeScript\n\n");
        section.push_str("### Common Commands\n\n");
        section.push_str("- Install: `npm install`\n");
        section.push_str("- Test: `npm test`\n");
        section.push_str("- Build: `npm run build`\n");
        section.push_str("- Start: `npm start`\n");
        if workspace.join("next.config.js").exists() || workspace.join("next.config.ts").exists() {
            section.push_str("\n### Framework\n\nNext.js\n");
        } else if workspace.join("vite.config.js").exists()
            || workspace.join("vite.config.ts").exists()
        {
            section.push_str("\n### Framework\n\nVite\n");
        }
    } else if workspace.join("pyproject.toml").exists() || workspace.join("setup.py").exists() {
        section.push_str("## Project Type: Python\n\n");
        section.push_str("### Common Commands\n\n");
        section.push_str("- Test: `pytest`\n");
        section.push_str("- Format: `black .`\n");
        section.push_str("- Lint: `ruff check .`\n");
    } else if workspace.join("go.mod").exists() {
        section.push_str("## Project Type: Go\n\n");
        section.push_str("### Common Commands\n\n");
        section.push_str("- Build: `go build`\n");
        section.push_str("- Test: `go test ./...`\n");
        section.push_str("- Format: `go fmt ./...`\n");
    } else {
        section.push_str("## Project Type\n\n");
        section.push_str("Unknown. Add the project's build, test, and lint commands below.\n");
    }
    section
}

fn read_cargo_package_name(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            let value = trimmed.strip_prefix("name")?.trim_start();
            let value = value.strip_prefix('=')?.trim();
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
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
    fn init_project_instructions_creates_agents_and_gitignore() {
        let root = temp_root("init-project");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "target").unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let path = init_project_instructions_at(&root).unwrap();

        assert_eq!(path, root.join("AGENTS.md"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Project Type: Rust"));
        assert!(content.contains("sample"));
        let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert!(gitignore.contains("target\n.dscode/\n"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn init_project_instructions_refuses_existing_agents_file() {
        let root = temp_root("init-existing");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("AGENTS.md"), "existing").unwrap();

        let error = init_project_instructions_at(&root).unwrap_err();

        assert!(error.to_string().contains("AGENTS.md already exists"));

        let _ = std::fs::remove_dir_all(root);
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
