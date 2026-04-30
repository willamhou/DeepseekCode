use std::error::Error;
use std::fmt::{Display, Formatter};

pub type AppResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppErrorKind {
    Other,
    PolicyDenied,
    ToolFailure,
}

#[derive(Debug)]
pub struct AppError {
    pub message: String,
    pub kind: AppErrorKind,
    pub hint: Option<String>,
    pub source: Option<Box<dyn Error + Send + Sync>>,
}

impl Display for AppError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "\n  hint: {hint}")?;
        }
        Ok(())
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|inner| inner as &(dyn Error + 'static))
    }
}

impl AppError {
    fn into_box(self) -> Box<dyn Error> {
        Box::new(self)
    }
}

pub fn app_error(message: impl Into<String>) -> Box<dyn Error> {
    let message = message.into();
    let hint = derive_hint(&message);
    AppError {
        message,
        kind: AppErrorKind::Other,
        hint,
        source: None,
    }
    .into_box()
}

pub fn policy_denied(message: impl Into<String>) -> Box<dyn Error> {
    let message = message.into();
    let hint = derive_hint(&message);
    AppError {
        message,
        kind: AppErrorKind::PolicyDenied,
        hint,
        source: None,
    }
    .into_box()
}

pub fn tool_failure(message: impl Into<String>) -> Box<dyn Error> {
    let message = message.into();
    let hint = derive_hint(&message);
    AppError {
        message,
        kind: AppErrorKind::ToolFailure,
        hint,
        source: None,
    }
    .into_box()
}

pub fn classify(error: &(dyn Error + 'static)) -> AppErrorKind {
    error
        .downcast_ref::<AppError>()
        .map(|app| app.kind)
        .unwrap_or(AppErrorKind::Other)
}

fn derive_hint(message: &str) -> Option<String> {
    let lower = message.to_ascii_lowercase();

    if lower.contains("gh cli not found") {
        return Some(
            "install gh from https://cli.github.com/ then run `gh auth login`".to_string(),
        );
    }
    if lower.contains("gh not authenticated") {
        return Some("run `gh auth login` to authenticate".to_string());
    }
    if lower.contains("expected branch") && lower.contains("currently on") {
        return Some("checkout the PR's head branch first (e.g. `gh pr checkout <N>`)".to_string());
    }
    if lower.contains("uncommitted changes") {
        return Some("run `git status` then commit or `git stash` before retrying".to_string());
    }
    if lower.contains("write declined") || lower.contains("approval required") {
        return Some(
            "set DSCODE_AUTO_APPROVE_WRITES=1 (and / or DSCODE_AUTO_APPROVE_SHELL=1) to skip prompts in non-interactive runs"
                .to_string(),
        );
    }
    if lower.contains("blocked by policy allowlist") || lower.contains("command not allowed") {
        return Some(
            "expand the active skill's `policy.shell_allowlist` or relax the policy in .dscode/config.toml"
                .to_string(),
        );
    }
    if lower.contains("non-interactive") && lower.contains("auto-denying") {
        return Some(
            "this is a non-TTY run; export DSCODE_AUTO_APPROVE_WRITES=1 / DSCODE_AUTO_APPROVE_SHELL=1 to bypass"
                .to_string(),
        );
    }
    if lower.contains("hunk") && lower.contains("did not match") {
        return Some(
            "the file changed since the patch was built; re-read the target and retry".to_string(),
        );
    }
    if lower.contains("escapes cwd") {
        return Some(
            "patch targets must be relative to the working directory; rebuild without `..` or absolute paths"
                .to_string(),
        );
    }
    if lower.contains("missing — remote calls will fall back") || lower.contains("api_key_env") {
        return Some(
            "export the configured API key (default DEEPSEEK_API_KEY) for live LLM-driven planning"
                .to_string(),
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_recognizes_policy_denied_kind() {
        let error = policy_denied("nope");
        assert_eq!(classify(error.as_ref()), AppErrorKind::PolicyDenied);
    }

    #[test]
    fn classify_recognizes_tool_failure_kind() {
        let error = tool_failure("crash");
        assert_eq!(classify(error.as_ref()), AppErrorKind::ToolFailure);
    }

    #[test]
    fn classify_falls_back_to_other_for_default_app_error() {
        let error = app_error("plain");
        assert_eq!(classify(error.as_ref()), AppErrorKind::Other);
    }

    #[test]
    fn classify_falls_back_to_other_for_non_app_errors() {
        let io_error: Box<dyn Error> = Box::new(std::io::Error::other("boom"));
        assert_eq!(classify(io_error.as_ref()), AppErrorKind::Other);
    }

    #[test]
    fn hint_attached_for_gh_auth_failure() {
        let error = policy_denied("gh not authenticated; run `gh auth login` (gh said: ...)");
        assert!(error.to_string().contains("hint: run `gh auth login`"));
    }

    #[test]
    fn hint_attached_for_branch_mismatch() {
        let error = policy_denied("expected branch `feat/x`, but currently on `main`; ...");
        assert!(error.to_string().contains("hint: checkout the PR"));
    }

    #[test]
    fn hint_attached_for_uncommitted_changes() {
        let error =
            policy_denied("working tree has uncommitted changes; commit or stash before --commit");
        assert!(error.to_string().contains("hint: run `git status`"));
    }

    #[test]
    fn no_hint_for_unknown_error_text() {
        let error = app_error("something went wrong with x42");
        assert!(!error.to_string().contains("hint:"));
    }

    #[test]
    fn source_chain_preserved_when_set() {
        let inner = std::io::Error::other("original cause");
        let app = AppError {
            message: "wrapper".to_string(),
            kind: AppErrorKind::ToolFailure,
            hint: None,
            source: Some(Box::new(inner)),
        };
        let boxed: Box<dyn Error> = Box::new(app);
        assert!(boxed.source().is_some());
        assert!(boxed
            .source()
            .unwrap()
            .to_string()
            .contains("original cause"));
    }
}
