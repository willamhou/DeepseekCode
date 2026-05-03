use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::memory::MemoryState;
use crate::core::observations::{compact_observations, summarize_for_kind};
use crate::core::session::{SessionSnapshot, SessionStore};
use crate::error::AppResult;
use crate::language::detect::detect_profile;
use crate::language::infer::default_test_command;
use crate::model::client::ModelClient;
use crate::model::deepseek::DeepSeekClient;
use crate::model::protocol::{ModelAction, ModelRequest, Observation, ObservationKind};
use crate::skills::registry::SkillRegistry;
use crate::skills::resolver::resolve_skill;
use crate::skills::schema::SkillSpec;
use crate::tools::registry::ExecutionPolicy;
use crate::ui::render::print_banner;

pub struct AgentLoopOptions {
    pub steps: usize,
    pub initial_observations: Vec<Observation>,
    pub todos: std::rc::Rc<std::cell::RefCell<crate::core::todos::TodoList>>,
}

impl Default for AgentLoopOptions {
    fn default() -> Self {
        Self {
            steps: 4,
            initial_observations: Vec::new(),
            todos: std::rc::Rc::new(std::cell::RefCell::new(
                crate::core::todos::TodoList::default(),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolEvent {
    pub tool_name: String,
    pub input: std::collections::BTreeMap<String, String>,
    pub output: String,
    pub status: crate::model::protocol::ObservationStatus,
}

#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub final_message: String,
    pub tool_events: Vec<ToolEvent>,
    pub usage: crate::model::protocol::TokenUsage,
}

pub struct AgentLoop {
    config: AppConfig,
}

impl AgentLoop {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub fn run(&self, context: TaskContext) -> AppResult<()> {
        self.run_with(context, AgentLoopOptions::default())
            .map(|_| ())
    }

    pub fn run_with(
        &self,
        context: TaskContext,
        options: AgentLoopOptions,
    ) -> AppResult<RunResult> {
        let client = DeepSeekClient {
            config: self.config.model.clone(),
        };
        self.run_with_client(context, options, &client)
    }

    pub fn run_with_client<C: ModelClient>(
        &self,
        context: TaskContext,
        options: AgentLoopOptions,
        client: &C,
    ) -> AppResult<RunResult> {
        let AgentLoopOptions {
            steps,
            initial_observations,
            todos,
        } = options;
        print_banner("DeepseekCode");

        let profile = detect_profile(".")?;
        let registry = crate::tools::registry::default_registry_with_todos(todos.clone());
        let skills = SkillRegistry::load_dir("skills")?;
        let skill = resolve_skill(&skills, context.skill.as_deref());
        let policy = ExecutionPolicy::new(&self.config.approval, skill);
        let memory = MemoryState::new(profile.name.clone());
        let primary_file = primary_file(&profile).map(str::to_string);
        let suggested_test_command = default_test_command(&profile).map(str::to_string);

        println!("Task: {}", context.task);
        println!("Profile: {}", profile.name);
        if !profile.hints.is_empty() {
            println!("Profile hints:");
            for hint in &profile.hints {
                println!("- {hint}");
            }
        }
        println!(
            "Available tools: {}",
            registry.names_for_policy(&policy).join(", ")
        );

        if let Some(skill) = skill {
            println!("Skill: {}", skill.name);
            println!("Skill description: {}", skill.description);
            if !skill.suggested_steps.is_empty() {
                println!("Suggested steps:");
                for step in &skill.suggested_steps {
                    println!("- {}", step);
                }
            }
        }

        println!("Memory summary: {}", memory.summary());

        let mut observations = initial_observations;
        let mut last_message = String::new();
        let mut tool_events: Vec<ToolEvent> = Vec::new();
        let mut total_usage = crate::model::protocol::TokenUsage::default();
        let mut renderer = crate::ui::stream::TtyRenderer::from_stdout();
        // Phase 10c-1: accumulate prior assistant messages so each step sees what it
        // already said. Without this, dscode run loops on "I'll start by …" because
        // the LLM never sees its own progress (REPL has Repl.transcript; one-shot did not).
        let mut recent_steps_log: Vec<String> = Vec::new();
        const RECENT_STEPS_KEEP: usize = 3;
        for step in 0..steps {
            let recent_window = recent_steps_log
                .iter()
                .rev()
                .take(RECENT_STEPS_KEEP)
                .rev()
                .cloned()
                .collect::<Vec<_>>();
            let request = ModelRequest {
                system_prompt: build_system_prompt(skill),
                task: context.task.clone(),
                profile_name: profile.name.clone(),
                profile_hints: profile.hints.clone(),
                primary_file: primary_file.clone(),
                suggested_test_command: suggested_test_command.clone(),
                available_tools: registry
                    .names_for_policy(&policy)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                observations: compact_observations(&observations),
                todos: todos.borrow().snapshot(),
                recent_steps: recent_window,
            };

            renderer.paint_step_divider(step + 1);
            let (response, step_usage) = client.respond(request, &mut renderer)?;
            if let Some(usage) = step_usage {
                total_usage.prompt += usage.prompt;
                total_usage.completion += usage.completion;
            }
            last_message = response.message.clone();
            if !response.message.trim().is_empty() {
                recent_steps_log.push(response.message.clone());
            }

            match response.action {
                ModelAction::CallTool { tool_name, input } => {
                    let event_input = input.args.clone();
                    match registry.execute_with_policy(&tool_name, input, &policy) {
                        Ok(output) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let observation_summary = summarize_for_kind(&output.summary, kind);
                            // CR-1: user sees full body (output.summary), observation/transcript get trim.
                            renderer.paint_tool_result(
                                crate::ui::stream::ToolResultKind::Ok,
                                &tool_name,
                                kind.label(),
                                &output.summary,
                            );
                            let event_name = tool_name.clone();
                            observations
                                .push(Observation::ok(tool_name, observation_summary.clone()));
                            tool_events.push(ToolEvent {
                                tool_name: event_name,
                                input: event_input,
                                output: observation_summary,
                                status: crate::model::protocol::ObservationStatus::Ok,
                            });
                        }
                        Err(error) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let raw = error.to_string();
                            let observation_summary = summarize_for_kind(&raw, kind);
                            let result_kind = match crate::error::classify(error.as_ref()) {
                                crate::error::AppErrorKind::PolicyDenied => {
                                    crate::ui::stream::ToolResultKind::Denied
                                }
                                _ => crate::ui::stream::ToolResultKind::Failed,
                            };
                            // CR-1: user sees full error text, observation/transcript get trim.
                            renderer.paint_tool_result(result_kind, &tool_name, kind.label(), &raw);
                            let event_name = tool_name.clone();
                            observations
                                .push(Observation::failed(tool_name, observation_summary.clone()));
                            tool_events.push(ToolEvent {
                                tool_name: event_name,
                                input: event_input,
                                output: observation_summary,
                                status: crate::model::protocol::ObservationStatus::Failed,
                            });
                        }
                    }
                }
                ModelAction::Finish => {
                    break;
                }
            }
        }

        if let Some(test_command) = suggested_test_command.as_deref() {
            println!();
            println!("Suggested validation command: {test_command}");
        }

        let store = SessionStore::new(self.config.workspace.session_dir());
        let snapshot = SessionSnapshot::new(context.task, profile.name);
        store.save(&snapshot)?;

        Ok(RunResult {
            final_message: last_message,
            tool_events,
            usage: total_usage,
        })
    }
}

fn primary_file(profile: &crate::language::profile::LanguageProfile) -> Option<&str> {
    profile.file_priority.iter().find_map(|path| {
        let candidate = path.trim_end_matches('/');
        if std::path::Path::new(candidate).is_file() {
            Some(candidate)
        } else {
            None
        }
    })
}

const TODO_NUDGE: &str = "\n\nYou have access to a todo_write tool. Use it proactively when the request:\n- involves three or more distinct steps,\n- spans multiple files or non-trivial refactoring,\n- requires running tests or shell commands as part of completion.\n\nEach todo has fields: content (imperative, e.g. \"Run tests\"), activeForm (present continuous, e.g. \"Running tests\"), status (\"pending\" | \"in_progress\" | \"completed\").\n\nMark exactly one todo as in_progress at a time. Update the list (mark completed, add discovered tasks) before moving to the next step. Skip todo_write only for trivial single-step requests.";

fn build_system_prompt(skill_name: Option<&SkillSpec>) -> String {
    let mut prompt = String::from(
        "You are the offline planning layer for DeepseekCode. Prefer repository inspection before edits.",
    );
    if let Some(skill) = skill_name {
        prompt.push_str(&format!(" Active skill: {}.", skill.name));
        if !skill.description.is_empty() {
            prompt.push_str(&format!(" Skill description: {}.", skill.description));
        }
        if !skill.system_append.is_empty() {
            prompt.push(' ');
            prompt.push_str(skill.system_append.trim());
        }
    }
    prompt.push_str(TODO_NUDGE);
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_system_prompt_includes_todo_nudge() {
        let prompt = super::build_system_prompt(None);
        assert!(prompt.contains("todo_write"));
        assert!(prompt.contains("in_progress"));
        assert!(prompt.contains("Skip todo_write only for trivial"));
    }

    #[test]
    fn build_system_prompt_places_nudge_after_skill_append() {
        use crate::skills::schema::{SkillPolicy, SkillSpec};
        // SkillPolicy has no Default impl in this codebase; construct explicitly.
        let skill = SkillSpec {
            name: "demo".to_string(),
            description: "demo skill".to_string(),
            allowed_tools: Vec::new(),
            system_append: "ZZZ_SKILL_HINT".to_string(),
            suggested_steps: Vec::new(),
            policy: SkillPolicy {
                require_write_confirmation: false,
                require_shell_confirmation: false,
                shell_allowlist: Vec::new(),
            },
        };
        let prompt = super::build_system_prompt(Some(&skill));
        let skill_pos = prompt.find("ZZZ_SKILL_HINT").expect("skill hint present");
        let nudge_pos = prompt.find("todo_write").expect("nudge present");
        assert!(nudge_pos > skill_pos, "nudge must come after skill_append");
    }

    #[test]
    fn agent_loop_options_default_provides_empty_todo_list() {
        let opts = AgentLoopOptions::default();
        assert_eq!(opts.steps, 4);
        assert!(opts.todos.borrow().is_empty());
    }
}

#[cfg(test)]
mod cr1_regression_test {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::core::context::TaskContext;
    use crate::core::todos::{TodoList, TodoStatus};
    use crate::model::client::ModelClient;
    use crate::model::protocol::{ModelAction, ModelRequest, ModelResponse, TokenUsage};
    use crate::tools::types::ToolInput;
    use crate::ui::stream::StreamEvents;

    struct ScriptedClient {
        calls: RefCell<u32>,
    }

    impl ModelClient for ScriptedClient {
        fn respond(
            &self,
            _input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            let n = *self.calls.borrow();
            *self.calls.borrow_mut() = n + 1;
            let action = if n == 0 {
                let mut input = ToolInput::new();
                let items = r#"[{"content":"A","activeForm":"Aing","status":"pending"},{"content":"B","activeForm":"Bing","status":"in_progress"},{"content":"C","activeForm":"Cing","status":"completed"}]"#;
                input.args.insert("items".to_string(), items.to_string());
                ModelAction::CallTool {
                    tool_name: "todo_write".to_string(),
                    input,
                }
            } else {
                ModelAction::Finish
            };
            Ok((
                ModelResponse {
                    message: "scripted".to_string(),
                    action,
                },
                None,
            ))
        }
    }

    struct ScriptedReplyClient {
        replies: RefCell<Vec<String>>,
        captured_recent_steps: RefCell<Vec<Vec<String>>>,
    }

    impl ModelClient for ScriptedReplyClient {
        fn respond(
            &self,
            input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            self.captured_recent_steps
                .borrow_mut()
                .push(input.recent_steps.clone());
            let n = self.captured_recent_steps.borrow().len() - 1;
            let action = if n < 2 {
                let mut tin = ToolInput::new();
                tin.args.insert("root".to_string(), ".".to_string());
                tin.args.insert("max_depth".to_string(), "1".to_string());
                tin.args.insert("limit".to_string(), "5".to_string());
                ModelAction::CallTool {
                    tool_name: "list_files".to_string(),
                    input: tin,
                }
            } else {
                ModelAction::Finish
            };
            let message = self
                .replies
                .borrow()
                .get(n)
                .cloned()
                .unwrap_or_else(|| "done".to_string());
            Ok((ModelResponse { message, action }, None))
        }
    }

    #[test]
    fn run_with_client_replays_recent_assistant_steps_into_each_request() {
        // Phase 10c-1 regression: dscode run multi-step loops without seeing prior
        // assistant messages, causing "I'll start by..." infinite loops. Verify the
        // ModelRequest.recent_steps field carries prior messages forward.
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let todos = Rc::new(RefCell::new(TodoList::default()));
        let client = ScriptedReplyClient {
            replies: RefCell::new(vec![
                "step ONE: looking at files".to_string(),
                "step TWO: read first one".to_string(),
                "step THREE: finishing".to_string(),
            ]),
            captured_recent_steps: RefCell::new(Vec::new()),
        };
        let _ = agent.run_with_client(
            context,
            AgentLoopOptions {
                steps: 3,
                initial_observations: Vec::new(),
                todos,
            },
            &client,
        );

        let captured = client.captured_recent_steps.borrow();
        assert_eq!(captured.len(), 3, "should have called respond 3 times");
        // First call: no prior steps yet.
        assert!(captured[0].is_empty(), "step 1 should see empty recent_steps");
        // Second call: should see step 1's message.
        assert_eq!(captured[1].len(), 1);
        assert!(captured[1][0].contains("step ONE"));
        // Third call: should see steps 1 + 2.
        assert_eq!(captured[2].len(), 2);
        assert!(captured[2][0].contains("step ONE"));
        assert!(captured[2][1].contains("step TWO"));
    }

    #[test]
    fn run_with_client_decouples_user_display_from_observation_summary() {
        // CR-1 regression: ToolEvent.output is the trim version (one line),
        // proving the user-display path (output.summary) is decoupled from
        // the observation/transcript path (summarize_for_kind).
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let todos = Rc::new(RefCell::new(TodoList::default()));
        let options = AgentLoopOptions {
            steps: 2,
            initial_observations: Vec::new(),
            todos: todos.clone(),
        };
        let client = ScriptedClient {
            calls: RefCell::new(0),
        };

        let result = agent.run_with_client(context, options, &client).unwrap();

        // The TodoList was actually mutated (proving the registry got the same Rc):
        let inner = todos.borrow();
        assert_eq!(inner.items.len(), 3);
        assert_eq!(inner.items[1].status, TodoStatus::InProgress);
        drop(inner);

        // The ToolEvent.output must be the trim version (one line, summary-only):
        assert_eq!(result.tool_events.len(), 1);
        let observed = &result.tool_events[0].output;
        assert_eq!(
            observed.lines().count(),
            1,
            "observation must be one line: {observed}"
        );
        assert!(observed.starts_with("3 todos"), "observed: {observed}");
    }
}
