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
}

impl Display for AppError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for AppError {}

pub fn app_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(AppError {
        message: message.into(),
        kind: AppErrorKind::Other,
    })
}

pub fn policy_denied(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(AppError {
        message: message.into(),
        kind: AppErrorKind::PolicyDenied,
    })
}

pub fn tool_failure(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(AppError {
        message: message.into(),
        kind: AppErrorKind::ToolFailure,
    })
}

pub fn classify(error: &(dyn Error + 'static)) -> AppErrorKind {
    error
        .downcast_ref::<AppError>()
        .map(|app| app.kind)
        .unwrap_or(AppErrorKind::Other)
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
}
