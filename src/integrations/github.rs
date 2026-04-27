use crate::error::{app_error, AppResult};

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
}
