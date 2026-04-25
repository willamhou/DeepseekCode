use crate::tools::apply_patch::ApplyPatchTool;
use crate::tools::git_diff::GitDiffTool;
use crate::tools::list_files::ListFilesTool;
use crate::tools::read_file::ReadFileTool;
use crate::tools::run_shell::RunShellTool;
use crate::tools::search_text::SearchTextTool;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::error::{app_error, AppResult};

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    pub fn execute(&self, name: &str, input: ToolInput) -> AppResult<ToolOutput> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .ok_or_else(|| app_error(format!("unknown tool: {name}")))?;
        tool.execute(input)
    }
}

pub fn default_registry() -> ToolRegistry {
    ToolRegistry {
        tools: vec![
            Box::new(ListFilesTool),
            Box::new(ReadFileTool),
            Box::new(SearchTextTool),
            Box::new(ApplyPatchTool),
            Box::new(RunShellTool),
            Box::new(GitDiffTool),
        ],
    }
}
