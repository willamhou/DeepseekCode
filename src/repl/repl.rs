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
    pub todos: std::rc::Rc<std::cell::RefCell<crate::core::todos::TodoList>>,
    pub last_rollback_snapshot_id: Option<String>,
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
            todos: std::rc::Rc::new(std::cell::RefCell::new(
                crate::core::todos::TodoList::default(),
            )),
            last_rollback_snapshot_id: None,
        }
    }

    pub fn run(&mut self) -> AppResult<()> {
        use std::io::{self, IsTerminal};
        if !io::stdin().is_terminal() {
            let bin = invoked_binary_name();
            return Err(crate::error::policy_denied(
                format!(
                    "{bin} interactive mode requires a TTY; use `{bin} run \"task\"` for one-shot tasks"
                ),
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
            crate::repl::slash::SlashOutcome::Submit(prompt) => {
                return self.dispatch_prompt(prompt);
            }
            crate::repl::slash::SlashOutcome::NotASlash => {}
        }

        self.dispatch_prompt(line.to_string())
    }

    fn dispatch_prompt(&mut self, prompt: String) -> AppResult<ControlFlow> {
        let snapshot_id = self.create_turn_snapshot(&prompt);
        if snapshot_id.is_some() {
            self.last_rollback_snapshot_id = snapshot_id.clone();
        }
        self.transcript.push_user(&prompt);
        let prompt = self.transcript.render_for_prompt();
        let context = crate::core::context::TaskContext::new(prompt, self.skill.clone());
        let runtime = crate::core::loop_runtime::AgentLoop::new(self.config.clone());
        let result = runtime.run_with(
            context,
            crate::core::loop_runtime::AgentLoopOptions {
                steps: self.budget,
                initial_observations: Vec::new(),
                todos: self.todos.clone(),
                ..crate::core::loop_runtime::AgentLoopOptions::default()
            },
        )?;

        self.tokens_prompt += result.usage.prompt;
        self.tokens_completion += result.usage.completion;
        let had_tool_events = !result.tool_events.is_empty();
        for event in result.tool_events {
            self.transcript
                .push_tool(event.tool_name, event.input, event.output, event.status);
        }
        if !result.final_message.is_empty() {
            self.transcript.push_assistant(result.final_message);
        }
        if had_tool_events {
            if let Some(snapshot_id) = snapshot_id {
                println!("rollback snapshot: {snapshot_id} (/revert_turn last --apply)");
            }
        }
        Ok(ControlFlow::Continue)
    }

    fn create_turn_snapshot(&self, prompt: &str) -> Option<String> {
        let cwd = std::env::current_dir().ok()?;
        let store = crate::core::rollback::RollbackStore::new(
            std::path::PathBuf::from(&self.config.workspace.config_dir).join("rollback"),
        );
        store
            .create_snapshot(&cwd, repl_turn_snapshot_label(prompt))
            .ok()
            .map(|snapshot| snapshot.id)
    }
}

fn repl_turn_snapshot_label(prompt: &str) -> String {
    let mut summary = prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect::<String>();
    if summary.is_empty() {
        summary = "empty prompt".to_string();
    }
    format!("REPL turn before: {summary}")
}

fn invoked_binary_name() -> String {
    std::env::args()
        .next()
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "deepseek".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::AppConfig;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-repl-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn new_starts_with_default_budget_and_empty_transcript() {
        let r = Repl::new(AppConfig::default(), None);
        assert_eq!(r.budget, DEFAULT_BUDGET);
        assert!(r.transcript.turns.is_empty());
        assert_eq!(r.tokens_prompt, 0);
        assert_eq!(r.tokens_completion, 0);
        assert!(r.skill.is_none());
        assert!(r.last_rollback_snapshot_id.is_none());
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

    #[test]
    fn invoked_binary_name_falls_back_to_deepseek_when_missing() {
        let name = invoked_binary_name();
        assert!(!name.trim().is_empty());
    }

    #[test]
    fn repl_turn_snapshot_label_compacts_prompt() {
        let label = repl_turn_snapshot_label("  edit   the file\n\nand run tests  ");
        assert_eq!(label, "REPL turn before: edit the file and run tests");
        let long = repl_turn_snapshot_label(&"x ".repeat(200));
        assert!(long.len() <= "REPL turn before: ".len() + 80);
    }

    #[test]
    fn create_turn_snapshot_captures_repl_worktree_state() {
        let repo = temp_root("turn-snapshot");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init"]);
        fs::write(repo.join("src.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "src.txt"]);
        run_git(
            &repo,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "initial",
            ],
        );
        fs::write(repo.join("src.txt"), "changed before repl turn\n").unwrap();

        let _cwd = crate::util::cwd::CwdGuard::enter(&repo).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = repo.join(".dscode").display().to_string();
        let repl = Repl::new(config, None);
        let snapshot_id = repl
            .create_turn_snapshot("edit src and run tests")
            .expect("REPL turn snapshot");

        let store = crate::core::rollback::RollbackStore::new(repo.join(".dscode/rollback"));
        let snapshot = store.load_snapshot(&snapshot_id).unwrap();
        assert_eq!(snapshot.label, "REPL turn before: edit src and run tests");
        assert!(snapshot.patch_bytes > 0);
        assert_eq!(snapshot.runtime_thread_id, None);
        assert_eq!(snapshot.runtime_turn_id, None);
    }
}
