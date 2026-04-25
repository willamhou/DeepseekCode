use crate::error::AppResult;
use crate::error::app_error;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ApplyPatchTool;

impl Tool for ApplyPatchTool {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        if let Some(patch) = input.get("patch") {
            let cwd = input.get("cwd").unwrap_or(".");
            apply_unified_patch(cwd, patch)
        } else {
            apply_text_replacement(&input)
        }
    }
}

fn apply_text_replacement(input: &ToolInput) -> AppResult<ToolOutput> {
    let path = input
        .get("path")
        .ok_or_else(|| app_error("apply_patch requires a path"))?;
    let find = input
        .get("find")
        .ok_or_else(|| app_error("apply_patch requires a find string"))?;
    let replace = input
        .get("replace")
        .ok_or_else(|| app_error("apply_patch requires a replace string"))?;
    let replace_all = input.get("replace_all").unwrap_or("false") == "true";
    let path = Path::new(path);

    if path.is_dir() {
        return Err(app_error("apply_patch path points to a directory"));
    }

    let original = fs::read_to_string(path)?;
    let updated = apply_replacement(&original, find, replace, replace_all)?;
    fs::write(path, updated)?;

    Ok(ToolOutput {
        summary: format!(
            "Updated {} using {} replacement mode.",
            path.display(),
            if replace_all { "global" } else { "single" }
        ),
    })
}

fn apply_replacement(
    original: &str,
    find: &str,
    replace: &str,
    replace_all: bool,
) -> AppResult<String> {
    if find.is_empty() {
        return Err(app_error("find string cannot be empty"));
    }

    if !original.contains(find) {
        return Err(app_error("find string not found in target file"));
    }

    let updated = if replace_all {
        original.replace(find, replace)
    } else {
        original.replacen(find, replace, 1)
    };

    Ok(updated)
}

fn apply_unified_patch(cwd: &str, patch: &str) -> AppResult<ToolOutput> {
    if patch.trim().is_empty() {
        return Err(app_error("patch content cannot be empty"));
    }

    let temp_path = unique_patch_path();
    let patch_body = normalize_patch_paths(cwd, patch);
    let patch_body = ensure_trailing_newline(&patch_body);
    fs::write(&temp_path, patch_body)?;

    let dry_run = Command::new("patch")
        .args(["--dry-run", "--batch", "--forward", "-p0", "-i"])
        .arg(&temp_path)
        .current_dir(cwd)
        .output()?;

    if !dry_run.status.success() {
        let stderr = String::from_utf8_lossy(&dry_run.stderr);
        let stdout = String::from_utf8_lossy(&dry_run.stdout);
        let _ = fs::remove_file(&temp_path);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(app_error(format!("patch dry-run failed: {detail}")));
    }

    let apply = Command::new("patch")
        .args(["--batch", "--forward", "-p0", "-i"])
        .arg(&temp_path)
        .current_dir(cwd)
        .output()?;

    let _ = fs::remove_file(&temp_path);

    if !apply.status.success() {
        let stderr = String::from_utf8_lossy(&apply.stderr);
        let stdout = String::from_utf8_lossy(&apply.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(app_error(format!("patch apply failed: {detail}")));
    }

    let stdout = String::from_utf8_lossy(&apply.stdout);
    let stderr = String::from_utf8_lossy(&apply.stderr);
    let mut summary = format!("Applied unified patch in {}.", cwd);
    if !stdout.trim().is_empty() {
        summary.push_str("\nstdout:\n");
        summary.push_str(stdout.trim());
    }
    if !stderr.trim().is_empty() {
        summary.push_str("\nstderr:\n");
        summary.push_str(stderr.trim());
    }

    Ok(ToolOutput { summary })
}

fn ensure_trailing_newline(value: &str) -> String {
    if value.ends_with('\n') {
        value.to_string()
    } else {
        format!("{value}\n")
    }
}

fn normalize_patch_paths(cwd: &str, patch: &str) -> String {
    let Ok(cwd) = fs::canonicalize(cwd) else {
        return patch.to_string();
    };

    patch
        .lines()
        .map(|line| normalize_patch_header_line(&cwd, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_patch_header_line(cwd: &Path, line: &str) -> String {
    let Some((prefix, raw_path)) = line
        .strip_prefix("--- ")
        .map(|path| ("--- ", path))
        .or_else(|| line.strip_prefix("+++ ").map(|path| ("+++ ", path)))
    else {
        return line.to_string();
    };

    let path_token = raw_path.split_whitespace().next().unwrap_or(raw_path);
    let path = Path::new(path_token);
    if !path.is_absolute() {
        return line.to_string();
    }

    let Ok(relative) = path.strip_prefix(cwd) else {
        return line.to_string();
    };

    let relative = relative.display().to_string();
    format!("{prefix}{relative}")
}

fn unique_patch_path() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("dscode_patch_{nanos}.diff"))
}

#[cfg(test)]
mod tests {
    use super::{apply_replacement, apply_unified_patch};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn replaces_first_occurrence_only() {
        let updated = apply_replacement("a b a", "a", "x", false).unwrap();
        assert_eq!(updated, "x b a");
    }

    #[test]
    fn replaces_all_occurrences() {
        let updated = apply_replacement("a b a", "a", "x", true).unwrap();
        assert_eq!(updated, "x b x");
    }

    #[test]
    fn errors_when_find_is_missing() {
        let error = apply_replacement("hello", "missing", "x", false).unwrap_err();
        assert!(error.to_string().contains("not found"));
    }

    #[test]
    fn applies_unified_diff_patch() {
        let dir = unique_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("demo.txt");
        fs::write(&file, "alpha\nbeta\n").unwrap();

        let patch = format!(
            "--- {0}\n+++ {0}\n@@ -1,2 +1,2 @@\n-alpha\n+omega\n beta\n",
            file.display()
        );

        let summary = apply_unified_patch(dir.to_str().unwrap(), &patch).unwrap();
        let content = fs::read_to_string(&file).unwrap();

        assert!(summary.summary.contains("Applied unified patch"));
        assert_eq!(content, "omega\nbeta\n");

        let _ = fs::remove_dir_all(dir);
    }

    fn unique_test_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("dscode_patch_test_{nanos}"))
    }
}
