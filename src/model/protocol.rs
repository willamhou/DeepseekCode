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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationStatus {
    Ok,
    Failed,
}

impl Observation {
    pub fn ok(tool_name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            summary: summary.into(),
            status: ObservationStatus::Ok,
        }
    }

    pub fn failed(tool_name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            summary: summary.into(),
            status: ObservationStatus::Failed,
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
