use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::memory::MemoryState;
use crate::core::session::{SessionSnapshot, SessionStore};
use crate::error::AppResult;
use crate::language::detect::detect_profile;
use crate::language::infer::default_test_command;
use crate::skills::registry::SkillRegistry;
use crate::skills::resolver::resolve_skill;
use crate::tools::registry::default_registry;
use crate::tools::types::ToolInput;
use crate::ui::render::print_banner;

pub struct AgentLoop {
    config: AppConfig,
}

impl AgentLoop {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub fn run(&self, context: TaskContext) -> AppResult<()> {
        print_banner("DeepseekCode");

        let profile = detect_profile(".")?;
        let registry = default_registry();
        let skills = SkillRegistry::load_dir("skills")?;
        let skill = resolve_skill(&skills, context.skill.as_deref());
        let memory = MemoryState::new(profile.name.clone());

        println!("Task: {}", context.task);
        println!("Profile: {}", profile.name);
        println!("Available tools: {}", registry.names().join(", "));

        if let Some(skill) = skill {
            println!("Skill: {}", skill.name);
        }

        println!("Memory summary: {}", memory.summary());
        println!();
        println!("Repository snapshot:");
        println!(
            "{}",
            registry.execute(
                "list_files",
                ToolInput::new()
                    .with_arg("root", ".")
                    .with_arg("max_depth", "2")
                    .with_arg("limit", "12")
            )?
            .summary
        );

        if let Some(primary_file) = primary_file(&profile) {
            println!();
            println!("Primary file excerpt: {primary_file}");
            println!(
                "{}",
                registry.execute(
                    "read_file",
                    ToolInput::new()
                        .with_arg("path", primary_file)
                        .with_arg("max_lines", "20")
                )?
                .summary
            );
        }

        if let Some(test_command) = default_test_command(&profile) {
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
