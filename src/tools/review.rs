use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::config::types::AppConfig;
use crate::core::runtime::{json_array, json_object};
use crate::error::{app_error, AppResult};
use crate::tools::dispatch_subagent::DispatchSubagentTool;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, json_value_to_string,
    parse_root_object, JsonValue,
};

const DEFAULT_MAX_CHARS: usize = 200_000;
const HARD_MAX_CHARS: usize = 1_000_000;
const DEFAULT_SEMANTIC_REVIEW_STEPS: &str = "6";
const MAX_SEMANTIC_REVIEW_DEPTH: usize = 2;

pub struct ReviewTool {
    config: Option<AppConfig>,
    parent_depth: usize,
}

pub struct PrReviewCommentPlanTool;

impl ReviewTool {
    pub fn new(config: AppConfig, parent_depth: usize) -> Self {
        Self {
            config: Some(config),
            parent_depth,
        }
    }
}

impl Default for ReviewTool {
    fn default() -> Self {
        Self {
            config: None,
            parent_depth: 0,
        }
    }
}

impl Tool for ReviewTool {
    fn name(&self) -> &str {
        "review"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let target = input
            .get("target")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| app_error("review requires `target`"))?;
        let cwd = input.get("cwd").map(str::trim).unwrap_or(".");
        let max_chars = parse_max_chars(&input);
        let source = resolve_review_source(&input, target, cwd, max_chars)?;
        let issues = review_issues(&source);
        let suggestions = review_suggestions(&source, &issues);
        let deterministic_output = review_output(&source, &issues, &suggestions);
        let output = if parse_bool(input.get("semantic")) {
            review_output_with_semantic(
                deterministic_output,
                self.run_semantic_review(&input, &source, &issues, &suggestions)?,
            )
        } else {
            deterministic_output
        };
        Ok(ToolOutput {
            summary: json_value_to_string(&output),
        })
    }
}

impl Tool for PrReviewCommentPlanTool {
    fn name(&self) -> &str {
        "pr_review_comment_plan"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let review_output = review_output_arg(&input)?;
        let root = parse_root_object(review_output)?;
        let issues = comment_issues_from_review(&root);
        let suggestions = comment_suggestions_from_review(&root);
        let source = comment_source_from_review(&root);
        let max_issues = parse_comment_max_issues(&input);
        let comment_error = input
            .get("comment_error")
            .or_else(|| input.get("previous_comment_error"))
            .or_else(|| input.get("retry_reason"))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let pr_context = input
            .get("github_context")
            .or_else(|| input.get("pr_context"))
            .or_else(|| input.get("context"))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let pr_number = comment_pr_number(&input, pr_context);
        let repo = comment_repo(&input, pr_context);
        let body = render_pr_review_comment_body(
            &issues,
            &suggestions,
            source.as_ref(),
            max_issues,
            comment_error,
        );
        let evidence =
            pr_review_comment_evidence(&root, &issues, source.as_ref(), max_issues, comment_error);
        let github_input =
            pr_review_comment_github_input(pr_number.as_deref(), repo.as_deref(), &body, &evidence);

        let mut fields = vec![
            (
                "summary",
                JsonValue::String(format!(
                    "Prepared PR review comment plan with {} finding(s).",
                    issues.len()
                )),
            ),
            ("comment_body", JsonValue::String(body)),
            ("evidence", evidence),
            (
                "ready_to_comment",
                JsonValue::Bool(pr_number.as_deref().is_some_and(|value| !value.is_empty())),
            ),
        ];
        if let Some(number) = pr_number {
            fields.push(("number", JsonValue::String(number)));
        }
        if let Some(repo) = repo {
            fields.push(("repo", JsonValue::String(repo)));
        }
        if let Some(input) = github_input {
            fields.push(("github_comment_input", input));
        }

        Ok(ToolOutput {
            summary: json_value_to_string(&comment_json_object(fields)),
        })
    }
}

impl ReviewTool {
    fn run_semantic_review(
        &self,
        input: &ToolInput,
        source: &ReviewSource,
        issues: &[ReviewIssue],
        suggestions: &[ReviewSuggestion],
    ) -> AppResult<JsonValue> {
        if self.parent_depth >= MAX_SEMANTIC_REVIEW_DEPTH {
            return Err(app_error(
                "review semantic mode is disabled at this subagent depth",
            ));
        }
        let Some(config) = self.config.as_ref() else {
            return Err(app_error(
                "review semantic mode requires a configured agent review tool",
            ));
        };
        let steps = input
            .get("steps")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_SEMANTIC_REVIEW_STEPS);
        let task = render_semantic_review_task(source, issues, suggestions);
        let mut child_input = ToolInput::new()
            .with_arg("task", task)
            .with_arg("steps", steps.to_string());
        if let Some(agent) = input
            .get("agent")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            child_input = child_input.with_arg("agent", agent.to_string());
        }
        if let Some(skill) = input
            .get("skill")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            child_input = child_input.with_arg("skill", skill.to_string());
        }
        let output = DispatchSubagentTool {
            config: config.clone(),
            parent_depth: self.parent_depth,
        }
        .execute(child_input)?;
        Ok(json_object([
            ("requested", JsonValue::Bool(true)),
            ("status", JsonValue::String("completed".to_string())),
            ("steps", JsonValue::String(steps.to_string())),
            ("summary", JsonValue::String(output.summary)),
        ]))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewSource {
    kind: String,
    target: String,
    path: Option<String>,
    content: String,
    truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewIssue {
    severity: String,
    title: String,
    description: String,
    path: Option<String>,
    line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewSuggestion {
    path: Option<String>,
    line: Option<u64>,
    suggestion: String,
}

fn resolve_review_source(
    input: &ToolInput,
    target: &str,
    cwd: &str,
    max_chars: usize,
) -> AppResult<ReviewSource> {
    let kind = input
        .get("kind")
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if let Some(context) = review_context_arg(input) {
        return resolve_context_source(target, context, max_chars);
    }
    let staged = parse_bool(input.get("staged")) || target.eq_ignore_ascii_case("staged");
    if kind.as_deref() == Some("diff")
        || target.eq_ignore_ascii_case("diff")
        || target.eq_ignore_ascii_case("staged")
    {
        return resolve_diff_source(input, target, cwd, staged, max_chars);
    }
    if target.starts_with("http://") || target.starts_with("https://") || target.contains("/pull/")
    {
        return Err(app_error(
            "review remote PR targets are not fetched by this tool; use github_pr_context first",
        ));
    }
    resolve_file_source(target, cwd, max_chars)
}

fn review_context_arg(input: &ToolInput) -> Option<&str> {
    input
        .get("github_context")
        .or_else(|| input.get("pr_context"))
        .or_else(|| input.get("context"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn resolve_context_source(
    target: &str,
    context: &str,
    max_chars: usize,
) -> AppResult<ReviewSource> {
    let (content, truncated) = clip_with_flag(context, max_chars);
    let has_pr_context =
        content.contains("meta.kind=pr") || target.contains("/pull/") || target.contains("pr");
    let has_diff = content.contains("\ndiff:\n")
        || content.starts_with("diff:\n")
        || content.contains("diff --git ");
    if !has_pr_context {
        return Err(app_error(
            "review context mode currently expects github_pr_context output",
        ));
    }
    Ok(ReviewSource {
        kind: if has_diff {
            "github_pr_diff".to_string()
        } else {
            "github_pr_context".to_string()
        },
        target: target.to_string(),
        path: None,
        content,
        truncated,
    })
}

fn resolve_file_source(target: &str, cwd: &str, max_chars: usize) -> AppResult<ReviewSource> {
    validate_relative_path(target)?;
    let root = std::fs::canonicalize(cwd)?;
    let path = root.join(target);
    let canonical = std::fs::canonicalize(&path)?;
    if !canonical.starts_with(&root) {
        return Err(app_error("review target must stay inside the workspace"));
    }
    let content = std::fs::read_to_string(&canonical)?;
    let (content, truncated) = clip_with_flag(&content, max_chars);
    Ok(ReviewSource {
        kind: "file".to_string(),
        target: target.to_string(),
        path: Some(target.to_string()),
        content,
        truncated,
    })
}

fn resolve_diff_source(
    input: &ToolInput,
    target: &str,
    cwd: &str,
    staged: bool,
    max_chars: usize,
) -> AppResult<ReviewSource> {
    let mut args = vec!["diff", "--no-color"];
    if staged {
        args.push("--cached");
    }
    let base = input
        .get("base")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(base) = base {
        validate_git_ref(base)?;
        args.push(base);
    }
    let output = Command::new("git").current_dir(cwd).args(&args).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        let exit_code = output.status.code().unwrap_or(-1);
        return Err(app_error(format!(
            "review git diff failed with exit code {exit_code}: {}",
            first_non_empty_line(&stderr)
                .or_else(|| first_non_empty_line(&stdout))
                .unwrap_or("no output")
        )));
    }
    if stdout.trim().is_empty() {
        return Err(app_error("review diff target has no changes"));
    }
    let (content, truncated) = clip_with_flag(&stdout, max_chars);
    Ok(ReviewSource {
        kind: if staged { "staged_diff" } else { "diff" }.to_string(),
        target: target.to_string(),
        path: None,
        content,
        truncated,
    })
}

fn review_issues(source: &ReviewSource) -> Vec<ReviewIssue> {
    let mut issues = Vec::new();
    let mut diff_path: Option<String> = None;
    let mut diff_line: Option<u64> = None;

    for (index, line) in source.content.lines().enumerate() {
        if source.kind.contains("diff") {
            if let Some(path) = line.strip_prefix("+++ b/") {
                diff_path = Some(path.to_string());
                diff_line = None;
                continue;
            }
            if let Some(start) = parse_diff_new_line_start(line) {
                diff_line = Some(start);
                continue;
            }
            if line.starts_with('+') && !line.starts_with("+++") {
                let content = &line[1..];
                push_line_issues(
                    &mut issues,
                    content,
                    diff_path.clone(),
                    diff_line,
                    source.kind.as_str(),
                );
                if let Some(value) = diff_line.as_mut() {
                    *value += 1;
                }
                continue;
            }
            if line.starts_with(' ') {
                if let Some(value) = diff_line.as_mut() {
                    *value += 1;
                }
            }
            continue;
        }

        push_line_issues(
            &mut issues,
            line,
            source.path.clone(),
            Some(index as u64 + 1),
            source.kind.as_str(),
        );
    }
    issues.extend(review_behavioral_issues(source));
    issues
}

fn review_behavioral_issues(source: &ReviewSource) -> Vec<ReviewIssue> {
    let mut issues = if source.kind.starts_with("github_pr") {
        review_github_pr_context_issues(source)
    } else {
        Vec::new()
    };
    if source.kind.contains("diff") {
        issues.extend(review_diff_behavioral_issues(source));
    } else {
        issues.extend(review_file_behavioral_issues(source));
    }
    issues
}

fn review_github_pr_context_issues(source: &ReviewSource) -> Vec<ReviewIssue> {
    let mut issues = Vec::new();
    if source.kind == "github_pr_context" {
        issues.push(issue(
            "info",
            "GitHub PR context missing diff",
            "Remote PR review context did not include a patch diff; rerun github_pr_context with include_diff=true before relying on code review findings.",
            None,
            None,
        ));
    }
    let Some(json_block) = github_pr_context_json(&source.content) else {
        return issues;
    };
    let Ok(root) = parse_root_object(json_block) else {
        issues.push(issue(
            "info",
            "GitHub PR context JSON was not parsed",
            "PR metadata could not be parsed, so review decision and status-check signals were not inspected.",
            None,
            None,
        ));
        return issues;
    };
    if let Some(decision) = root.get("reviewDecision").and_then(json_as_string) {
        match decision {
            "CHANGES_REQUESTED" => issues.push(issue(
                "warning",
                "GitHub PR has requested changes",
                "The PR review decision is CHANGES_REQUESTED; inspect reviewer feedback before treating the diff as ready.",
                None,
                None,
            )),
            "REVIEW_REQUIRED" => issues.push(issue(
                "info",
                "GitHub PR still requires review",
                "The PR review decision is REVIEW_REQUIRED; verify required reviewers have approved before merge.",
                None,
                None,
            )),
            _ => {}
        }
    }
    let failing_checks = failing_status_check_names(root.get("statusCheckRollup"));
    if !failing_checks.is_empty() {
        issues.push(issue(
            "warning",
            "GitHub PR status checks failing",
            &format!(
                "{} status check(s) are failing or cancelled: {}",
                failing_checks.len(),
                failing_checks.join(", ")
            ),
            None,
            None,
        ));
    }
    issues
}

fn review_file_behavioral_issues(source: &ReviewSource) -> Vec<ReviewIssue> {
    let Some(path) = source.path.as_deref() else {
        return Vec::new();
    };
    if is_test_path(path) || !source.content.lines().any(is_public_api_line) {
        return Vec::new();
    }
    if source.content.contains("#[cfg(test)]")
        || source.content.contains("mod tests")
        || source.content.contains("describe(")
        || source.content.contains("it(")
    {
        return Vec::new();
    }
    vec![issue(
        "info",
        "public API lacks local test signal",
        "Public API declarations were found, but no local test marker was detected in this file; verify coverage exists elsewhere.",
        Some(path.to_string()),
        first_matching_line(&source.content, is_public_api_line),
    )]
}

fn review_diff_behavioral_issues(source: &ReviewSource) -> Vec<ReviewIssue> {
    let mut issues = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_line: Option<u64> = None;
    let mut source_paths_changed = Vec::new();
    let mut test_changed = false;
    let mut manifest_changes = Vec::new();
    let mut public_api_changes = Vec::new();

    for line in source.content.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_path = Some(path.to_string());
            current_line = None;
            if is_test_path(path) {
                test_changed = true;
            }
            if is_manifest_path(path) && !manifest_changes.iter().any(|item| item == path) {
                manifest_changes.push(path.to_string());
            }
            continue;
        }
        if let Some(start) = parse_diff_new_line_start(line) {
            current_line = Some(start);
            continue;
        }
        let Some(path) = current_path.as_deref() else {
            continue;
        };
        if line.starts_with('+') && !line.starts_with("+++") {
            let added = &line[1..];
            if is_source_path(path) && !source_paths_changed.iter().any(|item| item == path) {
                source_paths_changed.push(path.to_string());
            }
            if is_public_api_line(added) {
                public_api_changes.push((path.to_string(), current_line));
            }
            if let Some(value) = current_line.as_mut() {
                *value += 1;
            }
            continue;
        }
        if line.starts_with('-') && !line.starts_with("---") {
            if is_source_path(path) && !source_paths_changed.iter().any(|item| item == path) {
                source_paths_changed.push(path.to_string());
            }
            continue;
        }
        if line.starts_with(' ') {
            if let Some(value) = current_line.as_mut() {
                *value += 1;
            }
        }
    }

    if !source_paths_changed.is_empty() && !test_changed {
        issues.push(issue(
            "warning",
            "source change without test change",
            "Source files changed, but no test file changes were detected in this diff; add or update focused tests or document why existing coverage is sufficient.",
            source_paths_changed.first().cloned(),
            None,
        ));
    }
    for (path, line) in public_api_changes.into_iter().take(5) {
        issues.push(issue(
            "info",
            "public API change",
            "Public declarations changed; verify compatibility, documentation, and caller/test coverage.",
            Some(path),
            line,
        ));
    }
    for path in manifest_changes.into_iter().take(5) {
        issues.push(issue(
            "warning",
            "dependency or configuration change",
            "Manifest/config changes can affect installs, releases, or runtime behavior; verify lockfiles, release notes, and compatibility.",
            Some(path),
            None,
        ));
    }
    issues
}

fn github_pr_context_json(content: &str) -> Option<&str> {
    let after_json = if let Some(rest) = content.strip_prefix("json:\n") {
        rest
    } else {
        let start = content.find("\njson:\n")? + "\njson:\n".len();
        &content[start..]
    };
    let json = if let Some(end) = after_json.find("\ndiff:\n") {
        &after_json[..end]
    } else {
        after_json
    };
    let json = json.trim();
    (!json.is_empty()).then_some(json)
}

fn failing_status_check_names(raw: Option<&JsonValue>) -> Vec<String> {
    let Some(items) = raw.and_then(json_as_array) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for item in items {
        let Some(object) = json_as_object(item) else {
            continue;
        };
        let failing = json_string_field(object, "conclusion")
            .or_else(|| json_string_field(object, "state"))
            .or_else(|| json_string_field(object, "status"))
            .is_some_and(is_failing_status_value);
        if !failing {
            continue;
        }
        let name = json_string_field(object, "name")
            .or_else(|| json_string_field(object, "workflowName"))
            .or_else(|| json_string_field(object, "context"))
            .unwrap_or("unnamed check")
            .to_string();
        if !names.iter().any(|item| item == &name) {
            names.push(name);
        }
    }
    names
}

fn json_string_field<'a>(
    object: &'a std::collections::BTreeMap<String, JsonValue>,
    key: &str,
) -> Option<&'a str> {
    object.get(key).and_then(json_as_string)
}

fn is_failing_status_value(value: &str) -> bool {
    matches!(
        value,
        "FAILURE" | "FAILED" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
    )
}

fn push_line_issues(
    issues: &mut Vec<ReviewIssue>,
    line: &str,
    path: Option<String>,
    line_number: Option<u64>,
    source_kind: &str,
) {
    let trimmed = line.trim();
    if trimmed.contains("<<<<<<<") || trimmed.contains("=======") || trimmed.contains(">>>>>>>") {
        issues.push(issue(
            "error",
            "merge conflict marker",
            "Conflict markers should be resolved before review or commit.",
            path.clone(),
            line_number,
        ));
    }
    if trimmed.contains(".unwrap()") || trimmed.contains(".expect(") {
        issues.push(issue(
            "warning",
            "panic-prone error handling",
            "unwrap/expect can panic at runtime; prefer explicit error handling unless this invariant is proven.",
            path.clone(),
            line_number,
        ));
    }
    if trimmed.contains("panic!(")
        || trimmed.contains("todo!(")
        || trimmed.contains("unimplemented!(")
    {
        issues.push(issue(
            "warning",
            "unfinished or panic path",
            "panic!, todo!, or unimplemented! can turn normal execution into a runtime failure.",
            path.clone(),
            line_number,
        ));
    }
    if trimmed.contains("dbg!(") || trimmed.contains("println!") || trimmed.contains("eprintln!") {
        issues.push(issue(
            "info",
            "debug output",
            "Debug printing in production code can create noisy output or leak operational details.",
            path.clone(),
            line_number,
        ));
    }
    if trimmed.contains("unsafe") && source_kind != "diff_header" {
        issues.push(issue(
            "warning",
            "unsafe code requires justification",
            "Unsafe blocks should have a narrow scope and a clear safety invariant.",
            path.clone(),
            line_number,
        ));
    }
}

fn review_suggestions(source: &ReviewSource, issues: &[ReviewIssue]) -> Vec<ReviewSuggestion> {
    let mut suggestions = Vec::new();
    if issues.is_empty() {
        suggestions.push(ReviewSuggestion {
            path: source.path.clone(),
            line: None,
            suggestion: "No deterministic risk markers were found; still verify behavior with focused tests.".to_string(),
        });
    } else {
        suggestions.push(ReviewSuggestion {
            path: None,
            line: None,
            suggestion: "Review each reported marker and add or update tests for changed behavior."
                .to_string(),
        });
    }
    if source.truncated {
        suggestions.push(ReviewSuggestion {
            path: source.path.clone(),
            line: None,
            suggestion:
                "Source was truncated; rerun review with a larger max_chars or a narrower target."
                    .to_string(),
        });
    }
    suggestions
}

fn review_output(
    source: &ReviewSource,
    issues: &[ReviewIssue],
    suggestions: &[ReviewSuggestion],
) -> JsonValue {
    let issue_count = issues.len();
    json_object([
        (
            "summary",
            JsonValue::String(format!(
                "Reviewed {} target `{}` with {} deterministic issue(s).",
                source.kind, source.target, issue_count
            )),
        ),
        (
            "issues",
            json_array(issues.iter().map(issue_to_json).collect()),
        ),
        (
            "suggestions",
            json_array(suggestions.iter().map(suggestion_to_json).collect()),
        ),
        (
            "overall_assessment",
            JsonValue::String(if issue_count == 0 {
                "No deterministic risk markers found. This is not a substitute for semantic review."
                    .to_string()
            } else {
                "Deterministic review found markers that should be resolved or justified."
                    .to_string()
            }),
        ),
        (
            "source",
            json_object([
                ("kind", JsonValue::String(source.kind.clone())),
                ("target", JsonValue::String(source.target.clone())),
                (
                    "path",
                    source
                        .path
                        .clone()
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::Null),
                ),
                ("truncated", JsonValue::Bool(source.truncated)),
            ]),
        ),
    ])
}

fn review_output_with_semantic(mut deterministic: JsonValue, semantic: JsonValue) -> JsonValue {
    let JsonValue::Object(ref mut object) = deterministic else {
        return deterministic;
    };
    object.insert("semantic_review".to_string(), semantic);
    object.insert(
        "overall_assessment".to_string(),
        JsonValue::String(
            "Semantic child review completed; inspect deterministic issues and semantic findings together."
                .to_string(),
        ),
    );
    deterministic
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommentIssue {
    severity: String,
    title: String,
    description: String,
    path: Option<String>,
    line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommentSource {
    kind: String,
    target: String,
    truncated: bool,
}

fn review_output_arg(input: &ToolInput) -> AppResult<&str> {
    input
        .get("review_output")
        .or_else(|| input.get("review_json"))
        .or_else(|| input.get("review"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("pr_review_comment_plan requires `review_output`"))
}

fn parse_comment_max_issues(input: &ToolInput) -> usize {
    input
        .get("max_issues")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(8)
        .clamp(1, 20)
}

fn comment_issues_from_review(root: &BTreeMap<String, JsonValue>) -> Vec<CommentIssue> {
    let Some(items) = root.get("issues").and_then(json_as_array) else {
        return Vec::new();
    };
    let mut issues = Vec::new();
    for item in items {
        let Some(object) = json_as_object(item) else {
            continue;
        };
        let title = json_string_field(object, "title")
            .unwrap_or("untitled finding")
            .to_string();
        let description = json_string_field(object, "description")
            .unwrap_or("")
            .to_string();
        let severity = json_string_field(object, "severity")
            .unwrap_or("info")
            .to_ascii_lowercase();
        let path = json_string_field(object, "path").map(str::to_string);
        let line = object.get("line").and_then(json_as_u64);
        issues.push(CommentIssue {
            severity,
            title,
            description,
            path,
            line,
        });
    }
    issues.sort_by_key(|issue| severity_rank(&issue.severity));
    issues
}

fn comment_suggestions_from_review(root: &BTreeMap<String, JsonValue>) -> Vec<String> {
    let Some(items) = root.get("suggestions").and_then(json_as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(json_as_object)
        .filter_map(|object| json_string_field(object, "suggestion"))
        .map(|value| compact_text(value, 240))
        .collect()
}

fn comment_source_from_review(root: &BTreeMap<String, JsonValue>) -> Option<CommentSource> {
    let object = root.get("source").and_then(json_as_object)?;
    let kind = json_string_field(object, "kind").unwrap_or("unknown");
    let target = json_string_field(object, "target").unwrap_or("unknown");
    let truncated = matches!(object.get("truncated"), Some(JsonValue::Bool(true)));
    Some(CommentSource {
        kind: kind.to_string(),
        target: target.to_string(),
        truncated,
    })
}

fn render_pr_review_comment_body(
    issues: &[CommentIssue],
    suggestions: &[String],
    source: Option<&CommentSource>,
    max_issues: usize,
    comment_error: Option<&str>,
) -> String {
    let mut body = String::new();
    body.push_str("## Automated PR Review\n\n");
    if let Some(error) = comment_error {
        body.push_str("Previous comment attempt failed or was denied: ");
        body.push_str(&compact_text(error, 240));
        body.push_str("\n\n");
    }
    if let Some(source) = source {
        body.push_str(&format!(
            "Reviewed `{}` target `{}`.",
            source.kind, source.target
        ));
        if source.truncated {
            body.push_str(" The reviewed source was truncated.");
        }
        body.push_str("\n\n");
    }
    if issues.is_empty() {
        body.push_str(
            "No deterministic issues were found. This is not a substitute for semantic review.\n",
        );
    } else {
        body.push_str(&format!(
            "Found {} deterministic issue(s). Findings are ordered by severity.\n\n",
            issues.len()
        ));
        for issue in issues.iter().take(max_issues) {
            body.push_str("- ");
            body.push_str(&issue.severity);
            body.push_str(": ");
            if let Some(location) = comment_issue_location(issue) {
                body.push_str(&location);
                body.push_str(" - ");
            }
            body.push_str(&compact_text(&issue.title, 120));
            if !issue.description.trim().is_empty() {
                body.push_str(" - ");
                body.push_str(&compact_text(&issue.description, 220));
            }
            body.push('\n');
        }
        if issues.len() > max_issues {
            body.push_str(&format!(
                "- {} additional finding(s) omitted from this comment plan.\n",
                issues.len() - max_issues
            ));
        }
    }
    if !suggestions.is_empty() {
        body.push_str("\nNext steps:\n");
        for suggestion in suggestions.iter().take(3) {
            body.push_str("- ");
            body.push_str(suggestion);
            body.push('\n');
        }
    }
    body
}

fn pr_review_comment_evidence(
    root: &BTreeMap<String, JsonValue>,
    issues: &[CommentIssue],
    source: Option<&CommentSource>,
    max_issues: usize,
    comment_error: Option<&str>,
) -> JsonValue {
    let mut error_count = 0usize;
    let mut warning_count = 0usize;
    let mut info_count = 0usize;
    for issue in issues {
        match issue.severity.as_str() {
            "error" => error_count += 1,
            "warning" => warning_count += 1,
            _ => info_count += 1,
        }
    }
    let mut fields = vec![
        ("tool", JsonValue::String("review".to_string())),
        (
            "review_summary",
            JsonValue::String(
                root.get("summary")
                    .and_then(json_as_string)
                    .unwrap_or("review output")
                    .to_string(),
            ),
        ),
        ("issue_count", JsonValue::Number(issues.len().to_string())),
        (
            "rendered_issue_count",
            JsonValue::Number(issues.len().min(max_issues).to_string()),
        ),
        (
            "severity_counts",
            json_object([
                ("error", JsonValue::Number(error_count.to_string())),
                ("warning", JsonValue::Number(warning_count.to_string())),
                ("info", JsonValue::Number(info_count.to_string())),
            ]),
        ),
    ];
    if let Some(source) = source {
        fields.push(("source_kind", JsonValue::String(source.kind.clone())));
        fields.push(("source_target", JsonValue::String(source.target.clone())));
        fields.push(("source_truncated", JsonValue::Bool(source.truncated)));
    }
    if let Some(error) = comment_error {
        fields.push((
            "previous_comment_error",
            JsonValue::String(compact_text(error, 500)),
        ));
    }
    comment_json_object(fields)
}

fn pr_review_comment_github_input(
    number: Option<&str>,
    repo: Option<&str>,
    body: &str,
    evidence: &JsonValue,
) -> Option<JsonValue> {
    let number = number?;
    let mut fields = vec![
        ("target", JsonValue::String("pr".to_string())),
        ("number", JsonValue::String(number.to_string())),
        ("body", JsonValue::String(body.to_string())),
        (
            "evidence",
            JsonValue::String(json_value_to_string(evidence)),
        ),
        ("dry_run", JsonValue::String("true".to_string())),
    ];
    if let Some(repo) = repo {
        fields.push(("repo", JsonValue::String(repo.to_string())));
    }
    Some(comment_json_object(fields))
}

fn comment_pr_number(input: &ToolInput, pr_context: Option<&str>) -> Option<String> {
    input
        .get("number")
        .or_else(|| input.get("pr"))
        .or_else(|| input.get("ref"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| pr_context.and_then(pr_context_number))
}

fn comment_repo(input: &ToolInput, pr_context: Option<&str>) -> Option<String> {
    input
        .get("repo")
        .or_else(|| input.get("repository"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| pr_context.and_then(pr_context_repo))
}

fn pr_context_number(context: &str) -> Option<String> {
    for line in context.lines() {
        if let Some(value) = line.strip_prefix("meta.number=") {
            let number = value.trim();
            if number.chars().all(|ch| ch.is_ascii_digit()) {
                return Some(number.to_string());
            }
        }
        if let Some(rest) = line.trim_start().strip_prefix("PR #") {
            let number: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if !number.is_empty() {
                return Some(number);
            }
        }
    }
    let json = github_pr_context_json(context)?;
    let root = parse_root_object(json).ok()?;
    root.get("number")
        .and_then(json_as_u64)
        .map(|value| value.to_string())
}

fn pr_context_repo(context: &str) -> Option<String> {
    let json = github_pr_context_json(context)?;
    let root = parse_root_object(json).ok()?;
    let url = root.get("url").and_then(json_as_string)?;
    github_repo_from_pr_url(url)
}

fn github_repo_from_pr_url(url: &str) -> Option<String> {
    let tail = url.strip_prefix("https://github.com/")?;
    let mut parts = tail.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let kind = parts.next()?;
    if kind != "pull" || owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn comment_issue_location(issue: &CommentIssue) -> Option<String> {
    let path = issue.path.as_deref()?.trim();
    if path.is_empty() {
        return None;
    }
    Some(match issue.line {
        Some(line) => format!("`{path}:{line}`"),
        None => format!("`{path}`"),
    })
}

fn severity_rank(severity: &str) -> usize {
    match severity {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    }
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= max_chars {
        return single_line;
    }
    let mut out = String::new();
    for ch in single_line.chars().take(max_chars.saturating_sub(3)) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn comment_json_object(fields: Vec<(&str, JsonValue)>) -> JsonValue {
    JsonValue::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn render_semantic_review_task(
    source: &ReviewSource,
    issues: &[ReviewIssue],
    suggestions: &[ReviewSuggestion],
) -> String {
    let deterministic = review_output(source, issues, suggestions);
    format!(
        "Semantic code review task\n\
\n\
Review the {kind} target `{target}` for real behavioral bugs, regressions, missing tests, \
compatibility risk, and maintainability issues. Findings must lead the answer, ordered by severity, \
with concrete file/line references when available. Do not modify files, run write tools, or post comments.\n\
\n\
Deterministic local review JSON:\n\
{deterministic}\n\
\n\
Source under review:\n\
```{fence}\n\
{content}\n\
```\n",
        kind = source.kind,
        target = source.target,
        deterministic = json_value_to_string(&deterministic),
        fence = if source.kind.contains("diff") {
            "diff"
        } else {
            "text"
        },
        content = source.content
    )
}

fn issue(
    severity: &str,
    title: &str,
    description: &str,
    path: Option<String>,
    line: Option<u64>,
) -> ReviewIssue {
    ReviewIssue {
        severity: severity.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        path,
        line,
    }
}

fn issue_to_json(issue: &ReviewIssue) -> JsonValue {
    json_object([
        ("severity", JsonValue::String(issue.severity.clone())),
        ("title", JsonValue::String(issue.title.clone())),
        ("description", JsonValue::String(issue.description.clone())),
        (
            "path",
            issue
                .path
                .clone()
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "line",
            issue
                .line
                .map(|line| JsonValue::Number(line.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
    ])
}

fn suggestion_to_json(suggestion: &ReviewSuggestion) -> JsonValue {
    json_object([
        (
            "path",
            suggestion
                .path
                .clone()
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "line",
            suggestion
                .line
                .map(|line| JsonValue::Number(line.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "suggestion",
            JsonValue::String(suggestion.suggestion.clone()),
        ),
    ])
}

fn parse_diff_new_line_start(line: &str) -> Option<u64> {
    let line = line.strip_prefix("@@ ")?;
    let plus = line.find('+')?;
    let after = &line[plus + 1..];
    let number = after
        .split(|ch| ch == ',' || ch == ' ')
        .next()
        .unwrap_or("");
    number.parse::<u64>().ok()
}

fn parse_max_chars(input: &ToolInput) -> usize {
    input
        .get("max_chars")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_CHARS)
        .clamp(1, HARD_MAX_CHARS)
}

fn parse_bool(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("true" | "1" | "yes" | "on")
    )
}

fn clip_with_flag(content: &str, max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    for (index, ch) in content.chars().enumerate() {
        if index >= max_chars {
            out.push_str("\n...[truncated]\n");
            return (out, true);
        }
        out.push(ch);
    }
    (out, false)
}

fn validate_relative_path(path: &str) -> AppResult<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(app_error("review file target must be a safe relative path"));
    }
    Ok(())
}

fn validate_git_ref(value: &str) -> AppResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '-' | '.' | ':'))
    {
        return Err(app_error("review base contains unsafe characters"));
    }
    Ok(())
}

fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

fn first_matching_line<F>(content: &str, predicate: F) -> Option<u64>
where
    F: Fn(&str) -> bool,
{
    content
        .lines()
        .position(predicate)
        .map(|index| index as u64 + 1)
}

fn is_source_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    matches!(
        Path::new(&lower)
            .extension()
            .and_then(|value| value.to_str()),
        Some(
            "rs" | "py"
                | "go"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "java"
                | "kt"
                | "swift"
                | "c"
                | "cc"
                | "cpp"
                | "h"
                | "hpp"
        )
    ) && !is_test_path(path)
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("__tests__")
        || lower.contains("_test.")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.ends_with("test.rs")
}

fn is_manifest_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "pyproject.toml"
            | "poetry.lock"
            | "requirements.txt"
            | "go.mod"
            | "go.sum"
            | "dockerfile"
            | "docker-compose.yml"
            | "docker-compose.yaml"
    ) || lower.starts_with(".github/workflows/")
        || lower.ends_with(".yml")
        || lower.ends_with(".yaml")
        || lower.ends_with(".toml")
}

fn is_public_api_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("pub fn ")
        || trimmed.starts_with("pub async fn ")
        || trimmed.starts_with("pub struct ")
        || trimmed.starts_with("pub enum ")
        || trimmed.starts_with("pub trait ")
        || trimmed.starts_with("export function ")
        || trimmed.starts_with("export async function ")
        || trimmed.starts_with("export class ")
        || trimmed.starts_with("export interface ")
        || trimmed.starts_with("export type ")
        || trimmed.starts_with("def ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-review-tool-{name}-{}-{suffix}",
            std::process::id()
        ))
    }

    #[test]
    fn review_file_reports_deterministic_markers() {
        let root = temp_root("file");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "fn demo(value: Option<u8>) { println!(\"{:?}\", value.unwrap()); }\n",
        )
        .unwrap();

        let output = ReviewTool::default()
            .execute(
                ToolInput::new()
                    .with_arg("target", "src/lib.rs")
                    .with_arg("cwd", root.display().to_string()),
            )
            .unwrap();

        assert!(output
            .summary
            .contains("\"title\":\"panic-prone error handling\""));
        assert!(output.summary.contains("\"title\":\"debug output\""));
        assert!(output.summary.contains("\"path\":\"src/lib.rs\""));
        assert!(output.summary.contains("\"line\":1"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn review_rejects_unsafe_file_path() {
        let error = ReviewTool::default()
            .execute(ToolInput::new().with_arg("target", "../secret.txt"))
            .unwrap_err();

        assert!(error.to_string().contains("safe relative path"));
    }

    #[test]
    fn review_diff_reports_added_line_markers() {
        if !git_available() {
            return;
        }
        let root = temp_root("diff");
        std::fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);
        std::fs::write(
            root.join("file.rs"),
            "fn demo(v: Option<u8>) { v.unwrap(); }\n",
        )
        .unwrap();

        let output = ReviewTool::default()
            .execute(
                ToolInput::new()
                    .with_arg("target", "diff")
                    .with_arg("cwd", root.display().to_string()),
            )
            .unwrap();

        assert!(output.summary.contains("\"kind\":\"diff\""));
        assert!(output.summary.contains("\"path\":\"file.rs\""));
        assert!(output.summary.contains("\"panic-prone error handling\""));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn review_diff_reports_behavioral_risk_signals() {
        if !git_available() {
            return;
        }
        let root = temp_root("behavioral-diff");
        std::fs::create_dir_all(root.join("src")).unwrap();
        init_git_repo(&root);
        std::fs::write(root.join("src/lib.rs"), "fn existing() -> u8 {\n    0\n}\n").unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        run_git(&root, &["add", "src/lib.rs", "Cargo.toml"]);
        run_git(&root, &["commit", "-m", "baseline"]);
        std::fs::write(
            root.join("src/lib.rs"),
            "fn existing() -> u8 {\n    0\n}\n\npub fn new_api() -> u8 {\n    1\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();

        let output = ReviewTool::default()
            .execute(
                ToolInput::new()
                    .with_arg("target", "diff")
                    .with_arg("cwd", root.display().to_string()),
            )
            .unwrap();

        assert!(output
            .summary
            .contains("\"title\":\"source change without test change\""));
        assert!(output.summary.contains("\"title\":\"public API change\""));
        assert!(output
            .summary
            .contains("\"title\":\"dependency or configuration change\""));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn review_accepts_github_pr_context_with_diff() {
        let pr_context = "meta.kind=pr\n\
meta.number=42\n\
PR #42: Add API\n\
json:\n\
{\"number\":42,\"title\":\"Add API\",\"reviewDecision\":\"APPROVED\",\"statusCheckRollup\":[{\"name\":\"ci\",\"conclusion\":\"SUCCESS\"}]}\n\
diff:\n\
diff --git a/src/lib.rs b/src/lib.rs\n\
index 0000000..1111111 100644\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -1,2 +1,6 @@\n\
 fn existing() {}\n\
+pub fn new_api() {\n\
+    println!(\"debug\");\n\
+}\n";

        let output = ReviewTool::default()
            .execute(
                ToolInput::new()
                    .with_arg("target", "github_pr_context")
                    .with_arg("github_context", pr_context),
            )
            .unwrap();

        assert!(output.summary.contains(r#""kind":"github_pr_diff""#));
        assert!(output.summary.contains(r#""target":"github_pr_context""#));
        assert!(output.summary.contains(r#""path":"src/lib.rs""#));
        assert!(output.summary.contains(r#""title":"debug output""#));
        assert!(output.summary.contains(r#""title":"public API change""#));
        assert!(output
            .summary
            .contains(r#""title":"source change without test change""#));
        assert!(!output.summary.contains("GitHub PR status checks failing"));
    }

    #[test]
    fn review_github_pr_context_reports_remote_pr_blockers() {
        let pr_context = "meta.kind=pr\n\
meta.number=42\n\
meta.state=OPEN\n\
PR #42: Add API\n\
json:\n\
{\"number\":42,\"title\":\"Add API\",\"reviewDecision\":\"CHANGES_REQUESTED\",\"statusCheckRollup\":[{\"name\":\"unit-tests\",\"conclusion\":\"FAILURE\"},{\"workflowName\":\"lint\",\"state\":\"SUCCESS\"},{\"context\":\"deploy\",\"status\":\"CANCELLED\"}]}\n";

        let output = ReviewTool::default()
            .execute(
                ToolInput::new()
                    .with_arg("target", "github_pr_context")
                    .with_arg("github_context", pr_context),
            )
            .unwrap();

        assert!(output.summary.contains(r#""kind":"github_pr_context""#));
        assert!(output
            .summary
            .contains(r#""title":"GitHub PR context missing diff""#));
        assert!(output
            .summary
            .contains(r#""title":"GitHub PR has requested changes""#));
        assert!(output
            .summary
            .contains(r#""title":"GitHub PR status checks failing""#));
        assert!(output.summary.contains("unit-tests"));
        assert!(output.summary.contains("deploy"));
    }

    #[test]
    fn pr_review_comment_plan_renders_comment_and_github_input() {
        let review_output = r#"{"summary":"Reviewed github_pr_diff target `github_pr_context` with 2 deterministic issue(s).","issues":[{"severity":"info","title":"public API change","description":"Public declarations changed; verify compatibility.","path":"src/lib.rs","line":7},{"severity":"warning","title":"GitHub PR status checks failing","description":"1 status check(s) are failing or cancelled: unit-tests","path":null,"line":null}],"suggestions":[{"path":null,"line":null,"suggestion":"Review each reported marker and add or update tests for changed behavior."}],"overall_assessment":"Deterministic review found markers that should be resolved or justified.","source":{"kind":"github_pr_diff","target":"github_pr_context","path":null,"truncated":false}}"#;
        let pr_context = "meta.kind=pr\n\
meta.number=42\n\
PR #42: Add API\n\
json:\n\
{\"number\":42,\"url\":\"https://github.com/acme/widgets/pull/42\"}\n";

        let output = PrReviewCommentPlanTool
            .execute(
                ToolInput::new()
                    .with_arg("review_output", review_output)
                    .with_arg("pr_context", pr_context),
            )
            .unwrap();

        assert!(output.summary.contains(r#""ready_to_comment":true"#));
        assert!(output.summary.contains(r#""number":"42""#));
        assert!(output.summary.contains(r#""repo":"acme/widgets""#));
        assert!(output.summary.contains("## Automated PR Review"));
        assert!(output
            .summary
            .contains("warning: GitHub PR status checks failing"));
        assert!(output.summary.contains(r#""github_comment_input""#));
        assert!(output.summary.contains(r#""dry_run":"true""#));
    }

    #[test]
    fn pr_review_comment_plan_includes_previous_comment_error() {
        let review_output = r#"{"summary":"Reviewed github_pr_diff target `github_pr_context` with 0 deterministic issue(s).","issues":[],"suggestions":[],"source":{"kind":"github_pr_diff","target":"github_pr_context","path":null,"truncated":false}}"#;

        let output = PrReviewCommentPlanTool
            .execute(
                ToolInput::new()
                    .with_arg("review_output", review_output)
                    .with_arg("number", "42")
                    .with_arg("comment_error", "policy denied by reviewer"),
            )
            .unwrap();

        assert!(output
            .summary
            .contains("Previous comment attempt failed or was denied"));
        assert!(output.summary.contains("policy denied by reviewer"));
        assert!(output.summary.contains(r#""previous_comment_error""#));
    }

    #[test]
    fn review_rejects_remote_pr_url_without_context() {
        let error = ReviewTool::default()
            .execute(
                ToolInput::new().with_arg("target", "https://github.com/example/project/pull/42"),
            )
            .unwrap_err();

        assert!(error.to_string().contains("github_pr_context first"));
    }

    #[test]
    fn review_file_reports_public_api_without_local_tests() {
        let root = temp_root("public-api-file");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn visible() -> u8 {\n    1\n}\n",
        )
        .unwrap();

        let output = ReviewTool::default()
            .execute(
                ToolInput::new()
                    .with_arg("target", "src/lib.rs")
                    .with_arg("cwd", root.display().to_string()),
            )
            .unwrap();

        assert!(output
            .summary
            .contains("\"title\":\"public API lacks local test signal\""));
        assert!(output.summary.contains("\"line\":1"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn review_semantic_requires_configured_agent_tool() {
        let root = temp_root("semantic-missing-config");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn visible() -> u8 { 1 }\n").unwrap();

        let error = ReviewTool::default()
            .execute(
                ToolInput::new()
                    .with_arg("target", "src/lib.rs")
                    .with_arg("cwd", root.display().to_string())
                    .with_arg("semantic", "true"),
            )
            .unwrap_err();

        assert!(error.to_string().contains("configured agent review tool"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn semantic_review_task_includes_baseline_json_and_read_only_instruction() {
        let source = ReviewSource {
            kind: "diff".to_string(),
            target: "diff".to_string(),
            path: None,
            content: "diff --git a/src/lib.rs b/src/lib.rs\n+pub fn visible() -> u8 { 1 }\n"
                .to_string(),
            truncated: false,
        };
        let issues = vec![issue(
            "info",
            "public API change",
            "Public declarations changed.",
            Some("src/lib.rs".to_string()),
            Some(1),
        )];
        let suggestions = vec![ReviewSuggestion {
            path: None,
            line: None,
            suggestion: "Add tests.".to_string(),
        }];
        let task = render_semantic_review_task(&source, &issues, &suggestions);

        assert!(task.contains("Semantic code review task"));
        assert!(task.contains("Do not modify files"));
        assert!(task.contains("\"title\":\"public API change\""));
        assert!(task.contains("```diff"));
        assert!(task.contains("+pub fn visible()"));
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn init_git_repo(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join("file.rs"), "fn demo() {}\n").unwrap();
        run_git(root, &["add", "file.rs"]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
