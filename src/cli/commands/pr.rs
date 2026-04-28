use crate::cli::app::PrAction;
use crate::config::load::load_or_default;
use crate::core::agent::Agent;
use crate::core::context::TaskContext;
use crate::core::loop_runtime::AgentLoopOptions;
use crate::error::AppResult;
use crate::integrations::github::{
    ensure_gh_auth, fetch_first_failed_job, fetch_pr, parse_pr_ref, post_pr_comment,
    require_on_branch, CiFailure, PrContext,
};
use crate::model::protocol::Observation;

pub fn run(action: PrAction) -> AppResult<()> {
    warn_if_offline_planner();
    match action {
        PrAction::Review { reference, post, out } => {
            run_review(&reference, post, out.as_deref())
        }
        PrAction::Fix { reference, job } => run_fix(&reference, job.as_deref()),
        PrAction::Patch { reference, commit } => run_patch(&reference, commit),
    }
}

fn warn_if_offline_planner() {
    let config = match load_or_default() {
        Ok(config) => config,
        Err(_) => return,
    };
    let api_key_present = std::env::var(&config.model.api_key_env)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !api_key_present {
        eprintln!(
            "[offline] {} is not set; the offline planner will produce a shallow report. Export it for a real LLM-driven review.",
            config.model.api_key_env
        );
    }
}

fn run_review(reference: &str, post: bool, out: Option<&str>) -> AppResult<()> {
    ensure_gh_auth()?;
    let pr_ref = parse_pr_ref(reference)?;
    let pr = fetch_pr(&pr_ref)?;

    let task = build_review_task_text(&pr);
    let context = TaskContext::new(task, Some("pr-review".to_string()));

    let observations = vec![
        Observation::ok("git_diff", pr.diff.clone()),
        Observation::ok("list_files", pr.changed_files.join("\n")),
    ];

    let config = load_or_default()?;
    let mut agent = Agent::new(config);
    agent.run_with(
        context,
        AgentLoopOptions {
            steps: 4,
            initial_observations: observations,
        },
    )?;

    let body = format!(
        "DeepseekCode review of PR #{} ({})\n\nSee terminal trace above for the full review.",
        pr.number, pr.title
    );
    deliver_review(&pr, &body, post, out)?;
    Ok(())
}

fn build_review_task_text(pr: &PrContext) -> String {
    format!(
        "Review pull request #{} '{}' on {}/{}. Highlight correctness risks, security concerns, and style violations. Output a markdown report.",
        pr.number, pr.title, pr.repo, pr.branch
    )
}

fn deliver_review(pr: &PrContext, body: &str, post: bool, out: Option<&str>) -> AppResult<()> {
    if let Some(path) = out {
        std::fs::write(path, body)?;
        println!("review written to {path}");
    }
    if post {
        post_pr_comment(&pr.repo, pr.number, body)?;
        println!("review posted as comment on {}#{}", pr.repo, pr.number);
    }
    if !post && out.is_none() {
        println!("{body}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_pr() -> PrContext {
        PrContext {
            number: 12,
            repo: "owner/repo".to_string(),
            title: "Add feature X".to_string(),
            branch: "feat/x".to_string(),
            base_branch: "main".to_string(),
            diff: String::new(),
            changed_files: Vec::new(),
        }
    }

    #[test]
    fn review_task_text_mentions_number_and_title() {
        let text = build_review_task_text(&fixture_pr());
        assert!(text.contains("#12"));
        assert!(text.contains("Add feature X"));
        assert!(text.contains("owner/repo"));
    }
}

fn run_fix(reference: &str, job_filter: Option<&str>) -> AppResult<()> {
    ensure_gh_auth()?;
    let pr_ref = parse_pr_ref(reference)?;
    let pr = fetch_pr(&pr_ref)?;
    require_on_branch(&pr.branch)?;

    let failure = match fetch_first_failed_job(&pr, job_filter)? {
        Some(failure) => failure,
        None => {
            println!("no failed CI jobs on PR #{}", pr.number);
            return Ok(());
        }
    };

    let task = build_fix_task_text(&pr, &failure);
    let context = TaskContext::new(task, None);
    let observations = vec![Observation::ok("run_shell", failure.log_tail.clone())];

    let config = load_or_default()?;
    let mut agent = Agent::new(config);
    agent.run_with(
        context,
        AgentLoopOptions {
            steps: 12,
            initial_observations: observations,
        },
    )?;

    println!(
        "fix attempt complete for job `{}` (run #{}); review `git diff HEAD` and rerun if needed",
        failure.job_name, failure.run_id
    );
    Ok(())
}

fn build_fix_task_text(pr: &PrContext, failure: &CiFailure) -> String {
    let step_clause = failure
        .failed_step
        .as_ref()
        .map(|step| format!(" at step `{step}`"))
        .unwrap_or_default();
    format!(
        "CI job `{job}` (run #{run_id}) on PR #{number} failed{step_clause}. Reproduce locally, fix the root cause, and rerun the failing test. Failed log tail follows.",
        job = failure.job_name,
        run_id = failure.run_id,
        number = pr.number,
    )
}

#[cfg(test)]
mod fix_tests {
    use super::*;

    fn fixture_pr() -> PrContext {
        PrContext {
            number: 12,
            repo: "owner/repo".to_string(),
            title: "Some PR".to_string(),
            branch: "feat/x".to_string(),
            base_branch: "main".to_string(),
            diff: String::new(),
            changed_files: Vec::new(),
        }
    }

    fn fixture_failure() -> CiFailure {
        CiFailure {
            run_id: 555,
            job_name: "test-rust".to_string(),
            job_id: 7,
            log_tail: "FAILED at line 42".to_string(),
            failed_step: Some("cargo test".to_string()),
        }
    }

    #[test]
    fn fix_task_text_includes_run_id_and_step() {
        let text = build_fix_task_text(&fixture_pr(), &fixture_failure());
        assert!(text.contains("run #555"));
        assert!(text.contains("test-rust"));
        assert!(text.contains("cargo test"));
        assert!(text.contains("PR #12"));
    }
}

use crate::integrations::github::worktree_is_clean;

fn run_patch(reference: &str, commit: bool) -> AppResult<()> {
    ensure_gh_auth()?;
    let pr_ref = parse_pr_ref(reference)?;
    let pr = fetch_pr(&pr_ref)?;
    require_on_branch(&pr.branch)?;
    if commit && !worktree_is_clean()? {
        return Err(crate::error::policy_denied(
            "working tree has uncommitted changes; commit or stash before --commit",
        ));
    }

    let task = build_patch_task_text(&pr);
    let context = TaskContext::new(task, None);
    let observations = vec![Observation::ok("git_diff", pr.diff.clone())];

    let config = load_or_default()?;
    let mut agent = Agent::new(config);
    agent.run_with(
        context,
        AgentLoopOptions {
            steps: 4,
            initial_observations: observations,
        },
    )?;

    if commit {
        run_git(&["add", "-A"])?;
        let message = format!("dscode: fix PR #{}", pr.number);
        run_git(&["commit", "-m", &message])?;
        println!("committed staged changes (no push)");
    } else {
        println!("changes left in worktree; run `git diff` to inspect, then commit manually");
    }
    Ok(())
}

fn build_patch_task_text(pr: &PrContext) -> String {
    format!(
        "Address review feedback or apply the requested change in PR #{} '{}'. PR diff is the current head; propose minimal additional changes.",
        pr.number, pr.title
    )
}

fn run_git(args: &[&str]) -> AppResult<()> {
    let output = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|error| crate::error::app_error(format!("could not invoke git: {error}")))?;
    if !output.status.success() {
        return Err(crate::error::tool_failure(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod patch_tests {
    use super::*;

    #[test]
    fn patch_task_text_mentions_pr_number_and_title() {
        let pr = PrContext {
            number: 9,
            repo: "o/r".to_string(),
            title: "Tighten retry loop".to_string(),
            branch: "feat/retry".to_string(),
            base_branch: "main".to_string(),
            diff: String::new(),
            changed_files: Vec::new(),
        };
        let text = build_patch_task_text(&pr);
        assert!(text.contains("#9"));
        assert!(text.contains("Tighten retry loop"));
    }
}
