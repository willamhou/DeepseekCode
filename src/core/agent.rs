use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::loop_runtime::AgentLoop;
use crate::error::AppResult;

pub struct Agent {
    config: AppConfig,
}

impl Agent {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub fn run(&mut self, context: TaskContext) -> AppResult<()> {
        let runtime = AgentLoop::new(self.config.clone());
        runtime.run(context)
    }

    pub fn run_with(
        &mut self,
        context: TaskContext,
        options: crate::core::loop_runtime::AgentLoopOptions,
    ) -> AppResult<()> {
        let runtime = crate::core::loop_runtime::AgentLoop::new(self.config.clone());
        runtime.run_with(context, options)
    }
}
