use std::collections::BTreeMap;

use crate::error::AppResult;

#[derive(Debug, Clone, Default)]
pub struct ToolInput {
    pub args: BTreeMap<String, String>,
}

impl ToolInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.insert(key.into(), value.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.args.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub summary: String,
}

pub trait Tool {
    fn name(&self) -> &'static str;
    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput>;
}
