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
}
