use crate::cli::app::RunArgs;
use crate::config::load::load_or_default;
use crate::core::context::TaskContext;
use crate::core::loop_runtime::AgentLoop;
use crate::error::AppResult;

pub fn run(args: RunArgs) -> AppResult<()> {
    let config = load_or_default()?;
    let context = TaskContext::new(args.task, args.skill);
    AgentLoop::new(config).run(context)
}
