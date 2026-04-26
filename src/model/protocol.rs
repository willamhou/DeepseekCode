use crate::tools::types::ToolInput;

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub system_prompt: String,
    pub task: String,
    pub profile_name: String,
    pub primary_file: Option<String>,
    pub suggested_test_command: Option<String>,
    pub available_tools: Vec<String>,
    pub observations: Vec<Observation>,
}

#[derive(Debug, Clone)]
pub struct Observation {
    pub tool_name: String,
    pub summary: String,
    pub status: ObservationStatus,
    pub kind: ObservationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationStatus {
    Ok,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObservationKind {
    FileExcerpt,
    Listing,
    SearchResults,
    Patch,
    Diff,
    ShellOutput,
    Other,
}

impl ObservationKind {
    pub fn from_tool_name(name: &str) -> Self {
        match name {
            "read_file" => Self::FileExcerpt,
            "list_files" => Self::Listing,
            "search_text" => Self::SearchResults,
            "apply_patch" => Self::Patch,
            "git_diff" => Self::Diff,
            "run_shell" => Self::ShellOutput,
            _ => Self::Other,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::FileExcerpt => "file_excerpt",
            Self::Listing => "listing",
            Self::SearchResults => "search_results",
            Self::Patch => "patch",
            Self::Diff => "diff",
            Self::ShellOutput => "shell_output",
            Self::Other => "other",
        }
    }
}

impl Observation {
    pub fn ok(tool_name: impl Into<String>, summary: impl Into<String>) -> Self {
        let tool_name = tool_name.into();
        let kind = ObservationKind::from_tool_name(&tool_name);
        Self {
            tool_name,
            summary: summary.into(),
            status: ObservationStatus::Ok,
            kind,
        }
    }

    pub fn failed(tool_name: impl Into<String>, summary: impl Into<String>) -> Self {
        let tool_name = tool_name.into();
        let kind = ObservationKind::from_tool_name(&tool_name);
        Self {
            tool_name,
            summary: summary.into(),
            status: ObservationStatus::Failed,
            kind,
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self.status, ObservationStatus::Failed)
    }
}

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub message: String,
    pub action: ModelAction,
}

#[derive(Debug, Clone)]
pub enum ModelAction {
    CallTool { tool_name: String, input: ToolInput },
    Finish,
}
