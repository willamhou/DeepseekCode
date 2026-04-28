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

pub struct AgentLoop {
    config: AppConfig,
}

impl AgentLoop {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub fn run(&self, context: TaskContext) -> AppResult<()> {
        self.run_with(context, AgentLoopOptions::default())
    }

    pub fn run_with(&self, context: TaskContext, options: AgentLoopOptions) -> AppResult<()> {
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

        let mut observations = options.initial_observations.clone();
        for step in 0..options.steps {
            let request = ModelRequest {
                system_prompt: build_system_prompt(skill),
                task: context.task.clone(),
                profile_name: profile.name.clone(),
                primary_file: primary_file.clone(),
                suggested_test_command: suggested_test_command.clone(),
                available_tools: registry
                    .names_for_policy(&policy)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                observations: compact_observations(&observations),
            };

            let response = client.respond(request)?;
            println!();
            println!("Step {}: {}", step + 1, response.message);

            match response.action {
                ModelAction::CallTool { tool_name, input } => {
                    match registry.execute_with_policy(&tool_name, input, &policy) {
                        Ok(output) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let summary = summarize_for_kind(&output.summary, kind);
                            println!("Tool `{tool_name}` output [{}]:", kind.label());
                            println!("{summary}");
                            observations.push(Observation::ok(tool_name, summary));
                        }
                        Err(error) => {
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let label = match crate::error::classify(error.as_ref()) {
                                crate::error::AppErrorKind::PolicyDenied => "DENIED",
                                _ => "FAILED",
                            };
                            let summary = summarize_for_kind(&error.to_string(), kind);
                            println!("Tool `{tool_name}` {label} [{}]:", kind.label());
                            println!("{summary}");
                            observations.push(Observation::failed(tool_name, summary));
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

        Ok(())
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

