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
use crate::tools::registry::{default_registry, ExecutionPolicy};
use crate::ui::render::print_banner;

pub struct AgentLoopOptions {
    pub steps: usize,
    pub initial_observations: Vec<Observation>,
}

impl Default for AgentLoopOptions {
    fn default() -> Self {
        Self {
            steps: 4,
            initial_observations: Vec::new(),
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
        let AgentLoopOptions {
            steps,
            initial_observations,
        } = options;
        print_banner("DeepseekCode");

        let profile = detect_profile(".")?;
        let registry = default_registry();
        let skills = SkillRegistry::load_dir("skills")?;
        let skill = resolve_skill(&skills, context.skill.as_deref());
        let policy = ExecutionPolicy::new(&self.config.approval, skill);
        let memory = MemoryState::new(profile.name.clone());
        let primary_file = primary_file(&profile).map(str::to_string);
        let suggested_test_command = default_test_command(&profile).map(str::to_string);
        let client = DeepSeekClient {
            config: self.config.model.clone(),
        };

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
        for step in 0..steps {
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
            };

            renderer.paint_step_divider(step + 1);
            let (response, step_usage) = client.respond(request, &mut renderer)?;
            if let Some(usage) = step_usage {
                total_usage.prompt += usage.prompt;
                total_usage.completion += usage.completion;
            }
            last_message = response.message.clone();

            match response.action {
                ModelAction::CallTool { tool_name, input } => {
                    let event_input = input.args.clone();
                    match registry.execute_with_policy(&tool_name, input, &policy) {
                        Ok(output) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let summary = summarize_for_kind(&output.summary, kind);
                            renderer.paint_tool_result(
                                crate::ui::stream::ToolResultKind::Ok,
                                &tool_name,
                                kind.label(),
                                &summary,
                            );
                            let event_output = summary.clone();
                            let event_name = tool_name.clone();
                            observations.push(Observation::ok(tool_name, summary));
                            tool_events.push(ToolEvent {
                                tool_name: event_name,
                                input: event_input,
                                output: event_output,
                                status: crate::model::protocol::ObservationStatus::Ok,
                            });
                        }
                        Err(error) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let summary = summarize_for_kind(&error.to_string(), kind);
                            let result_kind = match crate::error::classify(error.as_ref()) {
                                crate::error::AppErrorKind::PolicyDenied => {
                                    crate::ui::stream::ToolResultKind::Denied
                                }
                                _ => crate::ui::stream::ToolResultKind::Failed,
                            };
                            renderer.paint_tool_result(
                                result_kind,
                                &tool_name,
                                kind.label(),
                                &summary,
                            );
                            let event_output = summary.clone();
                            let event_name = tool_name.clone();
                            observations.push(Observation::failed(tool_name, summary));
                            tool_events.push(ToolEvent {
                                tool_name: event_name,
                                input: event_input,
                                output: event_output,
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
    prompt
}
