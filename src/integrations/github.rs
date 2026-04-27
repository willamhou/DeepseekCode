use crate::error::{app_error, AppResult};
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, parse_root_object, JsonValue,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrRef {
    Number(u64),
    Qualified { repo: String, number: u64 },
}

pub fn parse_pr_ref(input: &str) -> AppResult<PrRef> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(app_error("PR reference cannot be empty"));
    }

    if let Some(stripped) = trimmed.strip_prefix("https://github.com/") {
        let mut parts = stripped.split('/');
        let owner = parts.next().unwrap_or("");
        let repo = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        let number = parts.next().unwrap_or("");
        if kind != "pull" || owner.is_empty() || repo.is_empty() {
            return Err(app_error(format!("malformed GitHub PR URL: {input}")));
        }
        let number: u64 = number
            .parse()
            .map_err(|_| app_error(format!("PR URL has non-numeric ID: {input}")))?;
        return Ok(PrRef::Qualified {
            repo: format!("{owner}/{repo}"),
            number,
        });
    }

    if let Some((repo, number)) = trimmed.split_once('#') {
        if !repo.contains('/') {
            return Err(app_error(format!(
                "qualified PR reference must be `owner/repo#N`: {input}"
            )));
        }
        let number: u64 = number
            .parse()
            .map_err(|_| app_error(format!("qualified PR reference has non-numeric N: {input}")))?;
        return Ok(PrRef::Qualified {
            repo: repo.to_string(),
            number,
        });
    }

    let number: u64 = trimmed
        .parse()
        .map_err(|_| app_error(format!("PR reference is not a number, owner/repo#N, or URL: {input}")))?;
    Ok(PrRef::Number(number))
}

#[derive(Debug, Clone)]
pub struct PrContext {
    pub number: u64,
    pub repo: String,
    pub title: String,
    pub branch: String,
    pub base_branch: String,
    pub diff: String,
    pub changed_files: Vec<String>,
}

pub fn parse_pr_view_json(body: &str) -> AppResult<PrContext> {
    let root = parse_root_object(body)?;

    let number = root
        .get("number")
        .and_then(|value| match value {
            JsonValue::Number(text) => text.parse().ok(),
            _ => None,
        })
        .ok_or_else(|| app_error("pr view: missing or non-numeric `number`"))?;
    let title = root
        .get("title")
        .and_then(json_as_string)
        .ok_or_else(|| app_error("pr view: missing string `title`"))?
        .to_string();
    let branch = root
        .get("headRefName")
        .and_then(json_as_string)
        .ok_or_else(|| app_error("pr view: missing string `headRefName`"))?
        .to_string();
    let base_branch = root
        .get("baseRefName")
        .and_then(json_as_string)
        .ok_or_else(|| app_error("pr view: missing string `baseRefName`"))?
        .to_string();
    let repo = root
        .get("headRepository")
        .and_then(json_as_object)
        .and_then(|map| map.get("nameWithOwner"))
        .and_then(json_as_string)
        .ok_or_else(|| app_error("pr view: missing string `headRepository.nameWithOwner`"))?
        .to_string();
    let changed_files = root
        .get("files")
        .and_then(json_as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    json_as_object(item)
                        .and_then(|map| map.get("path"))
                        .and_then(json_as_string)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(PrContext {
        number,
        repo,
        title,
        branch,
        base_branch,
        diff: String::new(),
        changed_files,
    })
}

#[derive(Debug, Clone)]
pub struct CiFailure {
    pub run_id: u64,
    pub job_name: String,
    pub job_id: u64,
    pub log_tail: String,
    pub failed_step: Option<String>,
}

pub fn parse_first_failed_check(
    body: &str,
    job_filter: Option<&str>,
) -> AppResult<Option<(u64, String)>> {
    use crate::util::json::parse_json_value;

    let root = parse_json_value(body.trim())?;
    let JsonValue::Array(items) = root else {
        return Err(app_error("pr checks: expected JSON array"));
    };

    for item in &items {
        let JsonValue::Object(check) = item else {
            continue;
        };
        let state = check.get("state").and_then(json_as_string).unwrap_or("");
        if !state.eq_ignore_ascii_case("FAILURE") {
            continue;
        }
        let name = check
            .get("name")
            .and_then(json_as_string)
            .unwrap_or("")
            .to_string();
        if let Some(filter) = job_filter {
            if !name.eq_ignore_ascii_case(filter) {
                continue;
            }
        }
        let link = check
            .get("link")
            .and_then(json_as_string)
            .unwrap_or_default();
        if let Some(run_id) = extract_run_id_from_link(link) {
            return Ok(Some((run_id, name)));
        }
    }
    Ok(None)
}

fn extract_run_id_from_link(link: &str) -> Option<u64> {
    let marker = "/runs/";
    let start = link.find(marker)? + marker.len();
    let rest = &link[start..];
    let end = rest.find('/').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub fn parse_failed_job_from_run(
    body: &str,
    job_name: &str,
) -> AppResult<(u64, Option<String>)> {
    let root = parse_root_object(body)?;
    let jobs = root
        .get("jobs")
        .and_then(json_as_array)
        .ok_or_else(|| app_error("run view: missing `jobs` array"))?;

    for job in jobs {
        let Some(map) = json_as_object(job) else {
            continue;
        };
        let name = map.get("name").and_then(json_as_string).unwrap_or("");
        if !name.eq_ignore_ascii_case(job_name) {
            continue;
        }
        let database_id = map
            .get("databaseId")
            .and_then(|value| match value {
                JsonValue::Number(text) => text.parse().ok(),
                _ => None,
            })
            .ok_or_else(|| app_error(format!("run view: job `{job_name}` missing databaseId")))?;
        let failed_step = map
            .get("steps")
            .and_then(json_as_array)
            .and_then(|steps| {
                steps.iter().find_map(|step| {
                    let map = json_as_object(step)?;
                    let conclusion = map.get("conclusion").and_then(json_as_string)?;
                    if conclusion.eq_ignore_ascii_case("failure") {
                        Some(
                            map.get("name")
                                .and_then(json_as_string)
                                .unwrap_or("")
                                .to_string(),
                        )
                    } else {
                        None
                    }
                })
            });
        return Ok((database_id, failed_step));
    }

    Err(app_error(format!(
        "run view: job `{job_name}` not found in jobs list"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_number() {
        assert_eq!(parse_pr_ref("123").unwrap(), PrRef::Number(123));
    }

    #[test]
    fn parses_qualified_owner_repo_form() {
        let parsed = parse_pr_ref("willamhou/DeepseekCode#42").unwrap();
        assert_eq!(
            parsed,
            PrRef::Qualified {
                repo: "willamhou/DeepseekCode".to_string(),
                number: 42,
            }
        );
    }

    #[test]
    fn parses_github_pull_request_url() {
        let parsed =
            parse_pr_ref("https://github.com/willamhou/DeepseekCode/pull/7").unwrap();
        assert_eq!(
            parsed,
            PrRef::Qualified {
                repo: "willamhou/DeepseekCode".to_string(),
                number: 7,
            }
        );
    }

    #[test]
    fn rejects_blank_input() {
        assert!(parse_pr_ref("   ").is_err());
    }

    #[test]
    fn rejects_qualified_form_without_slash() {
        assert!(parse_pr_ref("repo#3").is_err());
    }

    #[test]
    fn rejects_non_numeric_id() {
        assert!(parse_pr_ref("owner/repo#abc").is_err());
    }

    #[test]
    fn parse_pr_view_extracts_metadata() {
        let body = r#"{
            "number": 12,
            "title": "Add CRLF round-trip",
            "headRefName": "feat/crlf",
            "baseRefName": "main",
            "headRepository": {"nameWithOwner": "willamhou/DeepseekCode"},
            "files": [
                {"path": "src/tools/apply_patch.rs"},
                {"path": "docs/roadmap.md"}
            ]
        }"#;
        let parsed = parse_pr_view_json(body).unwrap();
        assert_eq!(parsed.number, 12);
        assert_eq!(parsed.title, "Add CRLF round-trip");
        assert_eq!(parsed.branch, "feat/crlf");
        assert_eq!(parsed.base_branch, "main");
        assert_eq!(parsed.repo, "willamhou/DeepseekCode");
        assert_eq!(
            parsed.changed_files,
            vec![
                "src/tools/apply_patch.rs".to_string(),
                "docs/roadmap.md".to_string(),
            ]
        );
    }

    #[test]
    fn parse_pr_view_rejects_missing_required_fields() {
        let body = r#"{"number": 1}"#;
        assert!(parse_pr_view_json(body).is_err());
    }

    #[test]
    fn parse_pr_checks_finds_first_failed_run() {
        let body = r#"[
            {"name": "lint", "state": "SUCCESS", "link": "https://github.com/o/r/actions/runs/100/jobs/1"},
            {"name": "test", "state": "FAILURE", "link": "https://github.com/o/r/actions/runs/200/jobs/2"},
            {"name": "deploy", "state": "FAILURE", "link": "https://github.com/o/r/actions/runs/300/jobs/3"}
        ]"#;
        let (run_id, name) = parse_first_failed_check(body, None).unwrap().unwrap();
        assert_eq!(run_id, 200);
        assert_eq!(name, "test");
    }

    #[test]
    fn parse_pr_checks_filters_by_job_name() {
        let body = r#"[
            {"name": "lint", "state": "FAILURE", "link": "https://github.com/o/r/actions/runs/100/jobs/1"},
            {"name": "test", "state": "FAILURE", "link": "https://github.com/o/r/actions/runs/200/jobs/2"}
        ]"#;
        let (run_id, name) = parse_first_failed_check(body, Some("test"))
            .unwrap()
            .unwrap();
        assert_eq!(run_id, 200);
        assert_eq!(name, "test");
    }

    #[test]
    fn parse_pr_checks_returns_none_when_all_pass() {
        let body = r#"[
            {"name": "lint", "state": "SUCCESS", "link": "https://github.com/o/r/actions/runs/100/jobs/1"}
        ]"#;
        assert!(parse_first_failed_check(body, None).unwrap().is_none());
    }

    #[test]
    fn parse_run_jobs_picks_failed_job_id() {
        let body = r#"{
            "jobs": [
                {"databaseId": 11, "name": "lint", "conclusion": "success", "steps": []},
                {"databaseId": 22, "name": "test", "conclusion": "failure", "steps": [
                    {"name": "Set up Rust", "conclusion": "success"},
                    {"name": "cargo test", "conclusion": "failure"}
                ]}
            ]
        }"#;
        let (job_id, failed_step) = parse_failed_job_from_run(body, "test").unwrap();
        assert_eq!(job_id, 22);
        assert_eq!(failed_step.as_deref(), Some("cargo test"));
    }

    #[test]
    fn parse_run_jobs_errors_when_job_name_missing() {
        let body = r#"{"jobs": []}"#;
        assert!(parse_failed_job_from_run(body, "test").is_err());
    }
}
