use crate::error::app_error;
use crate::error::AppResult;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

        let mut process = Command::new("sh");
        process.args(["-lc", command]).current_dir(cwd);
        if let Some(path) = augmented_path_for_toolchains() {
            process.env("PATH", path);
        }
        let pycache_prefix = configure_python_cache_prefix(&mut process, command);

        let output_result = process.output();
        if let Some(prefix) = pycache_prefix {
            let _ = fs::remove_dir_all(prefix);
        }
        let output = output_result?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        let command_kind = classify_command_kind(command);
        let failed_tests = collect_failed_tests(command, &stdout, &stderr);
        let stderr_summary = first_non_empty_line(&stderr)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(clip_metadata_value);

        let mut summary = String::new();
        summary.push_str(&format!("meta.command_kind={command_kind}\n"));
        summary.push_str(&format!("meta.exit_code={exit_code}\n"));
        summary.push_str(&format!(
            "meta.result={}\n",
            if output.status.success() {
                "ok"
            } else {
                "failed"
            }
        ));
        if exit_code != 0 {
            summary.push_str(&format!(
                "meta.failure_kind={}\n",
                classify_failure_kind(command_kind, &failed_tests)
            ));
        }
        if !failed_tests.is_empty() {
            summary.push_str(&format!("meta.failed_tests={}\n", failed_tests.join(", ")));
        }
        if let Some(stderr_summary) = stderr_summary.as_deref() {
            summary.push_str(&format!("meta.stderr_summary={stderr_summary}\n"));
        }
        summary.push_str(&format!("exit_code: {exit_code}\n"));
        if !stdout.trim().is_empty() {
            summary.push_str("stdout:\n");
            summary.push_str(stdout.trim());
            summary.push('\n');
        }
        if !stderr.trim().is_empty() {
            summary.push_str("stderr:\n");
            summary.push_str(stderr.trim());
        }

        Ok(ToolOutput { summary })
    }
}

fn classify_command_kind(command: &str) -> &'static str {
    let command = command.trim();
    if command.starts_with("cargo test")
        || command.starts_with("go test")
        || command.starts_with("pytest")
        || command.starts_with("python -m pytest")
        || command.starts_with("node --test")
        || command.starts_with("gradle test")
        || command.starts_with("mvn test")
        || command.starts_with("pnpm test")
        || command.starts_with("npm test")
    {
        "test"
    } else if command.starts_with("cargo clippy")
        || command.starts_with("cargo fmt")
        || command.starts_with("ruff check")
        || command.starts_with("mypy")
        || command.starts_with("pnpm lint")
        || command.starts_with("npm run lint")
        || command.starts_with("go vet")
    {
        "lint"
    } else if command.starts_with("cargo build")
        || command.starts_with("cargo check")
        || command.starts_with("go build")
        || command.starts_with("pnpm build")
        || command.starts_with("npm run build")
        || command.starts_with("mvn package")
        || command.starts_with("gradle build")
    {
        "build"
    } else if command.starts_with("curl ")
        || command.starts_with("wget ")
        || command.starts_with("gh search ")
        || command.starts_with("gh repo view ")
        || command.starts_with("gh api ")
    {
        "research"
    } else {
        "other"
    }
}

fn classify_failure_kind(command_kind: &str, failed_tests: &[String]) -> &'static str {
    match command_kind {
        "test" if !failed_tests.is_empty() => "test_failure",
        "test" => "test_failure",
        "lint" => "lint_failure",
        "build" => "build_failure",
        _ => "command_failure",
    }
}

fn collect_failed_tests(command: &str, stdout: &str, stderr: &str) -> Vec<String> {
    let mut failures = Vec::new();
    let combined = format!("{stdout}\n{stderr}");
    let is_pytest =
        command.trim().starts_with("pytest") || command.trim().starts_with("python -m pytest");
    let is_node_test = command.trim().starts_with("node --test")
        || command.trim().starts_with("npm test")
        || command.trim().starts_with("pnpm test");
    let mut pending_node_failure: Option<String> = None;

    for line in combined.lines() {
        let trimmed = line.trim();
        if is_node_test {
            if let Some(name) = trimmed
                .strip_prefix("not ok ")
                .and_then(|rest| rest.split_once(" - ").map(|(_, name)| name.trim()))
            {
                if let Some(previous) = pending_node_failure.replace(name.to_string()) {
                    push_unique(&mut failures, previous);
                }
                continue;
            }
            if let Some(location) = trimmed.strip_prefix("location:") {
                if let Some(name) = pending_node_failure.take() {
                    if let Some(path) = extract_test_location_path(location) {
                        push_unique(&mut failures, format!("{path}::{name}"));
                    } else {
                        push_unique(&mut failures, name);
                    }
                }
                continue;
            }
            if let Some(location) = trimmed.strip_prefix("test at ") {
                if let Some(path) = extract_test_location_path(location) {
                    push_unique(&mut failures, path);
                }
                continue;
            }
        }
        if let Some(name) = trimmed
            .strip_prefix("test ")
            .and_then(|rest| rest.strip_suffix(" ... FAILED"))
        {
            push_unique(&mut failures, name.trim().to_string());
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix("FAILED ")
            .and_then(|rest| rest.split_whitespace().next())
        {
            if is_pytest || name.contains("::") || name.ends_with(".py") {
                push_unique(&mut failures, name.trim().to_string());
                continue;
            }
        }
        if let Some(name) = trimmed
            .strip_prefix("---- ")
            .and_then(|rest| rest.strip_suffix(" stdout ----"))
        {
            push_unique(&mut failures, name.trim().to_string());
        }
    }

    if let Some(name) = pending_node_failure {
        push_unique(&mut failures, name);
    }

    failures
}

fn extract_test_location_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('\'').trim_matches('"');
    let trimmed = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    let candidate = trimmed
        .rsplit_once(':')
        .and_then(|(left, _)| left.rsplit_once(':').map(|(path, _)| path))
        .unwrap_or(trimmed)
        .trim();
    if candidate.is_empty() {
        None
    } else {
        Some(candidate.to_string())
    }
}

fn push_unique(items: &mut Vec<String>, value: String) {
    if !items.iter().any(|existing| existing == &value) {
        items.push(value);
    }
}

fn first_non_empty_line(text: &str) -> Option<&str> {
    text.lines().find(|line| !line.trim().is_empty())
}

fn clip_metadata_value(value: &str) -> String {
    const LIMIT: usize = 120;
    if value.chars().count() <= LIMIT {
        return value.to_string();
    }
    let head: String = value.chars().take(LIMIT).collect();
    format!("{head}…")
}

fn augmented_path_for_toolchains() -> Option<OsString> {
    let home = std::env::var_os("HOME")?;
    let cargo_bin = PathBuf::from(home).join(".cargo").join("bin");
    if !cargo_bin.is_dir() {
        return None;
    }
    prepend_path_entry(std::env::var_os("PATH"), &cargo_bin)
}

fn prepend_path_entry(existing: Option<OsString>, entry: &Path) -> Option<OsString> {
    let existing = existing.unwrap_or_default();
    let mut paths = std::env::split_paths(&existing).collect::<Vec<_>>();
    if paths.iter().any(|path| path == entry) {
        return Some(existing);
    }
    paths.insert(0, entry.to_path_buf());
    std::env::join_paths(paths).ok()
}

fn configure_python_cache_prefix(process: &mut Command, command: &str) -> Option<PathBuf> {
    if !uses_isolated_python_cache(command) {
        return None;
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let prefix =
        std::env::temp_dir().join(format!("deepseek-pycache-{}-{nanos}", std::process::id()));
    if fs::create_dir_all(&prefix).is_err() {
        return None;
    }
    process.env("PYTHONPYCACHEPREFIX", &prefix);
    Some(prefix)
}

fn uses_isolated_python_cache(command: &str) -> bool {
    let command = command.trim();
    command.starts_with("pytest") || command.starts_with("python -m pytest")
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
        "node --test",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_command_kind_recognizes_test_and_build_commands() {
        assert_eq!(classify_command_kind("cargo test"), "test");
        assert_eq!(classify_command_kind("pytest -q"), "test");
        assert_eq!(classify_command_kind("node --test"), "test");
        assert_eq!(classify_command_kind("cargo check"), "build");
        assert_eq!(classify_command_kind("cargo clippy"), "lint");
        assert_eq!(
            classify_command_kind("gh search code dispatch_subagent"),
            "research"
        );
    }

    #[test]
    fn collect_failed_tests_parses_cargo_and_pytest_failures() {
        let cargo = collect_failed_tests(
            "cargo test",
            "test parser::round_trip ... ok\ntest parser::rejects_bad_input ... FAILED",
            "",
        );
        assert_eq!(cargo, vec!["parser::rejects_bad_input".to_string()]);

        let pytest = collect_failed_tests(
            "pytest -q",
            "FAILED tests/test_cli.py::test_help_flag - AssertionError",
            "",
        );
        assert_eq!(
            pytest,
            vec!["tests/test_cli.py::test_help_flag".to_string()]
        );

        let node = collect_failed_tests(
            "node --test",
            "TAP version 13\n# Subtest: routeBenchmarkCommand routes bench\nnot ok 1 - routeBenchmarkCommand routes bench\n  ---\n  location: 'test/route-benchmark.test.js:6:1'\n  ...",
            "",
        );
        assert_eq!(
            node,
            vec!["test/route-benchmark.test.js::routeBenchmarkCommand routes bench".to_string()]
        );

        let node_default = collect_failed_tests(
            "npm test",
            "✖ test/math.test.js (58.576176ms)\nℹ tests 1\nℹ fail 1\n\n✖ failing tests:\n\ntest at test/math.test.js:1:1\n✖ test/math.test.js (58.576176ms)\n  'test failed'",
            "",
        );
        assert_eq!(node_default, vec!["test/math.test.js".to_string()]);
    }

    #[test]
    fn uses_isolated_python_cache_only_for_pytest_commands() {
        assert!(uses_isolated_python_cache("pytest"));
        assert!(uses_isolated_python_cache("pytest -q"));
        assert!(uses_isolated_python_cache("python -m pytest tests"));
        assert!(!uses_isolated_python_cache("python script.py"));
        assert!(!uses_isolated_python_cache("cargo test"));
    }

    #[test]
    fn prepend_path_entry_puts_new_path_first_without_duplicates() {
        let existing = Some(OsString::from("/usr/bin:/bin"));
        let joined = prepend_path_entry(existing, Path::new("/tmp/toolchain")).unwrap();
        let paths = std::env::split_paths(&joined).collect::<Vec<_>>();
        assert_eq!(paths[0], PathBuf::from("/tmp/toolchain"));

        let joined = prepend_path_entry(Some(joined), Path::new("/tmp/toolchain")).unwrap();
        let paths = std::env::split_paths(&joined).collect::<Vec<_>>();
        assert_eq!(
            paths
                .iter()
                .filter(|path| *path == &PathBuf::from("/tmp/toolchain"))
                .count(),
            1
        );
    }
}
