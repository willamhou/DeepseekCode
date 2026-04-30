use crate::config::types::AppConfig;
use crate::error::AppResult;
use crate::repl::transcript::Transcript;

pub const DEFAULT_BUDGET: usize = 20;

pub enum ControlFlow {
    Continue,
    Quit,
}

#[derive(Debug)]
pub struct Repl {
    pub config: AppConfig,
    pub transcript: Transcript,
    pub budget: usize,
    pub skill: Option<String>,
    pub tokens_prompt: u64,
    pub tokens_completion: u64,
}

impl Repl {
    pub fn new(config: AppConfig, skill: Option<String>) -> Self {
        Self {
            config,
            transcript: Transcript::default(),
            budget: DEFAULT_BUDGET,
            skill,
            tokens_prompt: 0,
            tokens_completion: 0,
        }
    }

    pub fn run(&mut self) -> AppResult<()> {
        use std::io::{self, IsTerminal};
        if !io::stdin().is_terminal() {
            return Err(crate::error::policy_denied(
                "dscode chat requires a TTY; use `dscode run \"task\"` for one-shot tasks",
            ));
        }
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        self.run_with_reader(&mut handle, &mut io::stderr())
    }

    pub fn run_with_reader<R: std::io::BufRead, W: std::io::Write>(
        &mut self,
        reader: &mut R,
        prompt_sink: &mut W,
    ) -> AppResult<()> {
        let mut buffer = String::new();
        loop {
            let _ = write!(prompt_sink, "> ");
            let _ = prompt_sink.flush();
            buffer.clear();
            let bytes = reader.read_line(&mut buffer)?;
            if bytes == 0 {
                return Ok(());
            }
            let line = buffer.trim_end_matches('\n').trim_end_matches('\r');
            match self.handle_line(line)? {
                ControlFlow::Continue => continue,
                ControlFlow::Quit => return Ok(()),
            }
        }
    }

    pub fn handle_line(&mut self, line: &str) -> AppResult<ControlFlow> {
        if line.trim().is_empty() {
            return Ok(ControlFlow::Continue);
        }
        match crate::repl::slash::try_handle_slash(self, line)? {
            crate::repl::slash::SlashOutcome::Quit => return Ok(ControlFlow::Quit),
            crate::repl::slash::SlashOutcome::Continue => return Ok(ControlFlow::Continue),
            crate::repl::slash::SlashOutcome::NotASlash => {}
        }

        self.transcript.push_user(line);
        let prompt = self.transcript.render_for_prompt();
        let context =
            crate::core::context::TaskContext::new(prompt, self.skill.clone());
        let runtime = crate::core::loop_runtime::AgentLoop::new(self.config.clone());
        let result = runtime.run_with(
            context,
            crate::core::loop_runtime::AgentLoopOptions {
                steps: self.budget,
                initial_observations: Vec::new(),
            },
        )?;

        self.tokens_prompt += result.usage.prompt;
        self.tokens_completion += result.usage.completion;
        for event in result.tool_events {
            self.transcript.push_tool(
                event.tool_name,
                event.input,
                event.output,
                event.status,
            );
        }
        if !result.final_message.is_empty() {
            self.transcript.push_assistant(result.final_message);
        }
        Ok(ControlFlow::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::AppConfig;

    #[test]
    fn new_starts_with_default_budget_and_empty_transcript() {
        let r = Repl::new(AppConfig::default(), None);
        assert_eq!(r.budget, DEFAULT_BUDGET);
        assert!(r.transcript.turns.is_empty());
        assert_eq!(r.tokens_prompt, 0);
        assert_eq!(r.tokens_completion, 0);
        assert!(r.skill.is_none());
    }

    #[test]
    fn new_keeps_skill_when_provided() {
        let r = Repl::new(AppConfig::default(), Some("pr-review".to_string()));
        assert_eq!(r.skill.as_deref(), Some("pr-review"));
    }

    #[test]
    fn handle_line_returns_continue_for_empty_input() {
        let mut r = Repl::new(AppConfig::default(), None);
        let cf = r.handle_line("").unwrap();
        assert!(matches!(cf, ControlFlow::Continue));
        assert!(r.transcript.turns.is_empty());
    }

    #[test]
    fn handle_line_returns_continue_for_whitespace() {
        let mut r = Repl::new(AppConfig::default(), None);
        assert!(matches!(
            r.handle_line("   \t  ").unwrap(),
            ControlFlow::Continue,
        ));
        assert!(r.transcript.turns.is_empty());
    }

    #[test]
    fn handle_line_routes_help_slash_to_continue() {
        let mut r = Repl::new(AppConfig::default(), None);
        let cf = r.handle_line("/help").unwrap();
        assert!(matches!(cf, ControlFlow::Continue));
    }

    #[test]
    fn handle_line_routes_quit_slash_to_quit_control_flow() {
        let mut r = Repl::new(AppConfig::default(), None);
        let cf = r.handle_line("/quit").unwrap();
        assert!(matches!(cf, ControlFlow::Quit));
    }

    #[test]
    fn run_with_reader_processes_slash_commands_and_quits() {
        use std::io::Cursor;
        let mut input = Cursor::new(b"/help\n/quit\n".to_vec());
        let mut output = Vec::new();
        let mut r = Repl::new(AppConfig::default(), None);
        r.run_with_reader(&mut input, &mut output).unwrap();
        let prompt = String::from_utf8(output).unwrap();
        assert!(prompt.contains("> "));
    }
}
