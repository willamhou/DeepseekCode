use crate::error::AppResult;
use crate::error::app_error;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use std::process::Command;

pub struct RunShellTool;

impl Tool for RunShellTool {
    fn name(&self) -> &'static str {
        "run_shell"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let command = input
            .get("command")
            .ok_or_else(|| app_error("run_shell requires a command"))?;
        let cwd = input.get("cwd").unwrap_or(".");

        if !is_safe_shell_command(command) {
            return Err(app_error(format!("command not allowed: {command}")));
        }

        let output = Command::new("sh")
            .args(["-lc", command])
            .current_dir(cwd)
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut summary = String::new();
        summary.push_str(&format!("exit_code: {}\n", output.status.code().unwrap_or(-1)));
        if !stdout.trim().is_empty() {
            summary.push_str("stdout:\n");
            summary.push_str(stdout.trim());
            summary.push('\n');
        }
        if !stderr.trim().is_empty() {
            summary.push_str("stderr:\n");
            summary.push_str(stderr.trim());
        }

        Ok(ToolOutput {
            summary,
        })
    }
}

pub fn is_safe_shell_command(command: &str) -> bool {
    let command = command.trim();
    let allowlist = [
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo clippy",
        "cargo fmt",
        "go test",
        "go build",
        "go vet",
        "pytest",
        "python -m pytest",
        "ruff check",
        "mypy",
        "pnpm test",
        "pnpm lint",
        "pnpm build",
        "npm test",
        "npm run lint",
        "npm run build",
        "mvn test",
        "mvn package",
        "gradle test",
        "gradle build",
        "git status",
        "git diff",
        "ls",
        "pwd",
        "mkdir -p ",
        "cat ",
        "echo ",
        "head ",
        "tail ",
        // Read-only research / fetch (Phase 10c precursor — no body, follow redirects).
        "curl -sSL ",
        "curl -sS ",
        "curl -L ",
        "curl -I ",
        "wget -qO- ",
        "gh search ",
        "gh repo view ",
        "gh api ",
    ];

    allowlist.iter().any(|prefix| command.starts_with(prefix))
}
