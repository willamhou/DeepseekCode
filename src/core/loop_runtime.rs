use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::memory::MemoryState;
use crate::core::observations::{compact_observations, summarize_for_kind};
use crate::core::session::{SessionSnapshot, SessionStore};
use crate::error::{app_error, AppResult};
use crate::language::detect::detect_profile;
use crate::language::infer::default_test_command;
use crate::model::client::ModelClient;
use crate::model::deepseek::DeepSeekClient;
use crate::model::protocol::{
    ModelAction, ModelRequest, ModelResponse, Observation, ObservationKind, TokenUsage,
};
use crate::skills::registry::SkillRegistry;
use crate::skills::resolver::{resolve_skill, SkillResolution};
use crate::skills::schema::SkillSpec;
use crate::tools::registry::ExecutionPolicy;
use crate::ui::render::print_banner;
use crate::ui::stream::StreamEvents;
use crate::util::cancel::CancellationCheck;

pub struct AgentLoopOptions {
    pub steps: usize,
    pub initial_observations: Vec<Observation>,
    pub todos: std::rc::Rc<std::cell::RefCell<crate::core::todos::TodoList>>,
    pub subagent_depth: usize,
    pub emit_progress: bool,
    pub persist_session: bool,
    pub stream_events: Option<Box<dyn crate::ui::stream::StreamEvents>>,
    pub run_events: Option<SharedAgentRunEvents>,
    pub approval_resolver: Option<SharedAgentApprovalResolver>,
    pub cancel_check: Option<SharedAgentCancelCheck>,
}

impl Default for AgentLoopOptions {
    fn default() -> Self {
        Self {
            steps: 4,
            initial_observations: Vec::new(),
            todos: std::rc::Rc::new(std::cell::RefCell::new(
                crate::core::todos::TodoList::default(),
            )),
            subagent_depth: 0,
            emit_progress: true,
            persist_session: true,
            stream_events: None,
            run_events: None,
            approval_resolver: None,
            cancel_check: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolEvent {
    pub tool_name: String,
    pub input: BTreeMap<String, String>,
    pub output: String,
    pub status: crate::model::protocol::ObservationStatus,
}

#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub final_message: String,
    pub tool_events: Vec<ToolEvent>,
    pub usage: crate::model::protocol::TokenUsage,
}

pub type SharedAgentRunEvents = Rc<RefCell<dyn AgentRunEvents>>;

pub trait AgentRunEvents {
    fn on_tool_call(&mut self, tool_name: &str, input: &BTreeMap<String, String>);

    fn on_permission_request(
        &mut self,
        tool_name: &str,
        input: &BTreeMap<String, String>,
        kind: &str,
        target: &str,
    );

    fn on_tool_result(&mut self, event: &ToolEvent);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentApprovalRequest {
    pub tool_name: String,
    pub input: BTreeMap<String, String>,
    pub kind: String,
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentApprovalDecision {
    Approved,
    Denied,
}

pub type SharedAgentApprovalResolver = Rc<RefCell<dyn AgentApprovalResolver>>;

pub trait AgentApprovalResolver {
    fn resolve(&mut self, request: &AgentApprovalRequest) -> AppResult<AgentApprovalDecision>;
}

pub type SharedAgentCancelCheck = Rc<RefCell<dyn AgentCancelCheck>>;

pub trait AgentCancelCheck {
    fn is_cancelled(&mut self) -> AppResult<bool>;
}

struct AgentCancelAdapter<'a> {
    inner: &'a mut dyn AgentCancelCheck,
}

impl CancellationCheck for AgentCancelAdapter<'_> {
    fn is_cancelled(&mut self) -> AppResult<bool> {
        self.inner.is_cancelled()
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
            subagent_depth,
            emit_progress,
            persist_session,
            mut stream_events,
            run_events,
            approval_resolver,
            cancel_check,
        } = options;
        if emit_progress {
            print_banner("DeepSeekCode");
        }

        let profile = detect_profile(".")?;
        let cwd = std::env::current_dir()?;
        let workspace_instructions =
            crate::core::instructions::load_workspace_instructions(&cwd, &self.config.workspace)?;
        let hooks = crate::core::hooks::HookRunner::new(&self.config.hooks);
        let registry = crate::tools::registry::default_registry_with_context(
            self.config.clone(),
            subagent_depth,
            todos.clone(),
        );
        let user_skills_dir =
            crate::skills::tilde::expand_tilde(&self.config.workspace.user_skills_dir);
        let repo_skills_dir = crate::skills::paths::resolve_repo_skills_dir();
        let (skills, _stats) =
            SkillRegistry::load_dirs(&[repo_skills_dir.as_path(), user_skills_dir.as_path()])?;
        let resolved_skill = resolve_skill(&skills, context.skill.as_deref(), &context.task);
        let skill = resolved_skill.map(|resolved| resolved.spec);
        let policy = ExecutionPolicy::new(&self.config.approval, skill);
        let memory = MemoryState::new(profile.name.clone());
        let primary_file = primary_file(&profile).map(str::to_string);
        let suggested_test_command = default_test_command(&profile).map(str::to_string);
        if let Some(skill) = skill {
            if todos.borrow().is_empty() && !skill.initial_todos.is_empty() {
                let seeded = skill
                    .initial_todos
                    .iter()
                    .map(crate::skills::schema::TodoSeed::to_todo)
                    .collect::<Vec<_>>();
                let seeded_count = seeded.len();
                todos.borrow_mut().replace(seeded);
                if emit_progress {
                    println!("Seeded todos from skill: {seeded_count}");
                }
            }
        }

        if emit_progress {
            println!("Task: {}", context.task);
            println!("Profile: {}", profile.name);
            if !profile.hints.is_empty() {
                println!("Profile hints:");
                for hint in &profile.hints {
                    println!("- {hint}");
                }
            }
        }
        let available_tools = registry
            .names_for_policy(&policy)
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let research_bootstrap =
            should_apply_research_bootstrap(&context.task, Path::new("."), &available_tools);
        let planning_mode = !research_bootstrap
            && should_use_explicit_planning(&context.task, skill, &available_tools);

        if emit_progress {
            println!("Available tools: {}", available_tools.join(", "));
            if planning_mode {
                println!("Planning mode: explicit");
            }
        }

        if let Some(skill) = skill {
            if emit_progress {
                println!("Skill: {}", skill.name);
                if let Some(resolved) = resolved_skill {
                    match resolved.resolution {
                        SkillResolution::Explicit => println!("Skill source: explicit"),
                        SkillResolution::Auto => println!("Skill source: auto (trigger match)"),
                    }
                }
                println!("Skill description: {}", skill.description);
                if !skill.suggested_steps.is_empty() {
                    println!("Suggested steps:");
                    for step in &skill.suggested_steps {
                        println!("- {}", step);
                    }
                }
                if !skill.references.is_empty() {
                    println!("References:");
                    for reference in &skill.references {
                        println!("- {}", reference);
                    }
                }
            }
        }

        if emit_progress {
            println!("Memory summary: {}", memory.summary());
            if !workspace_instructions.is_empty() {
                println!("Workspace instructions:");
                for file in &workspace_instructions {
                    let suffix = if file.truncated { " (truncated)" } else { "" };
                    println!("- {}{}", file.path.display(), suffix);
                }
            }
        }

        let mut observations = initial_observations;
        if let Some(hook_context) = hooks.session_start(&context.task, "startup")? {
            observations.push(Observation::ok(
                "hook",
                format!("session_start: {hook_context}"),
            ));
        }
        if let Some(hook_context) = hooks.user_prompt_submit(&context.task)? {
            observations.push(Observation::ok(
                "hook",
                format!("user_prompt_submit: {hook_context}"),
            ));
        }
        let mut last_message = String::new();
        let mut tool_events: Vec<ToolEvent> = Vec::new();
        let mut total_usage = crate::model::protocol::TokenUsage::default();
        let mut renderer = emit_progress.then(crate::ui::stream::TtyRenderer::from_stdout);
        let mut noop_events = crate::ui::stream::NoopStreamEvents;
        // Phase 10c-1: accumulate prior assistant messages and compact reasoning
        // summaries so each step sees what it already considered. Without this,
        // dscode run loops on "I'll start by …" because the LLM never sees its own
        // progress (REPL has Repl.transcript; one-shot did not).
        let mut recent_steps_log: Vec<String> = Vec::new();
        const RECENT_STEPS_KEEP: usize = 3;
        // Phase 10c-2: repeat-call detection. Track fingerprints of the last
        // REPEAT_WINDOW tool calls. 2nd identical call appends a stuck-warning to the
        // observation summary; 3rd short-circuits with tool_failure forcing the LLM
        // to change strategy. Dogfood-driven: v4-pro reproducibly looped 30 steps on
        // identical list_files invocations against an empty workspace.
        let mut recent_call_fingerprints: Vec<String> = Vec::new();
        const REPEAT_WINDOW: usize = 3;
        for step in 0..steps {
            check_cancelled(cancel_check.as_ref())?;
            let recent_window = recent_steps_log
                .iter()
                .rev()
                .take(RECENT_STEPS_KEEP)
                .rev()
                .cloned()
                .collect::<Vec<_>>();
            let todo_snapshot = todos.borrow().snapshot();
            let request = ModelRequest {
                system_prompt: build_system_prompt_with_workspace_instructions(
                    skill,
                    research_bootstrap,
                    planning_mode,
                    !todo_snapshot.is_empty(),
                    available_tools
                        .iter()
                        .any(|tool| tool == "dispatch_subagent" || tool == "dispatch_subagents"),
                    &workspace_instructions,
                ),
                task: context.task.clone(),
                image_inputs: context.image_inputs.clone(),
                profile_name: profile.name.clone(),
                profile_hints: profile.hints.clone(),
                primary_file: primary_file.clone(),
                suggested_test_command: suggested_test_command.clone(),
                available_tools: available_tools.clone(),
                observations: compact_observations(&observations),
                todos: todo_snapshot,
                planning_mode,
                recent_steps: recent_window,
            };

            if let Some(renderer) = renderer.as_mut() {
                renderer.paint_step_divider(step + 1);
            }
            let (response, step_usage, step_reasoning) =
                if let Some(events) = stream_events.as_deref_mut() {
                    let mut capture = ReasoningCaptureEvents::new(events);
                    let outcome = model_respond_with_cancel(
                        client,
                        request,
                        &mut capture,
                        cancel_check.as_ref(),
                    )?;
                    let reasoning = capture.into_reasoning();
                    (outcome.0, outcome.1, reasoning)
                } else if let Some(renderer) = renderer.as_mut() {
                    let mut capture = ReasoningCaptureEvents::new(renderer);
                    let outcome = model_respond_with_cancel(
                        client,
                        request,
                        &mut capture,
                        cancel_check.as_ref(),
                    )?;
                    let reasoning = capture.into_reasoning();
                    (outcome.0, outcome.1, reasoning)
                } else {
                    let mut capture = ReasoningCaptureEvents::new(&mut noop_events);
                    let outcome = model_respond_with_cancel(
                        client,
                        request,
                        &mut capture,
                        cancel_check.as_ref(),
                    )?;
                    let reasoning = capture.into_reasoning();
                    (outcome.0, outcome.1, reasoning)
                };
            if let Some(usage) = step_usage {
                total_usage.add_assign(&usage);
            }
            check_cancelled(cancel_check.as_ref())?;
            last_message = response.message.clone();
            if let Some(entry) = recent_step_replay_entry(&response.message, &step_reasoning) {
                recent_steps_log.push(entry);
            }

            match response.action {
                ModelAction::CallTool { tool_name, input } => {
                    check_cancelled(cancel_check.as_ref())?;
                    let event_input = input.args.clone();
                    emit_tool_call(run_events.as_ref(), &tool_name, &event_input);

                    // Phase 10c-2: compute fingerprint and check window BEFORE executing.
                    let fingerprint = format!(
                        "{}:{}",
                        tool_name,
                        event_input
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join("|")
                    );
                    let same_count_in_window = recent_call_fingerprints
                        .iter()
                        .rev()
                        .take(REPEAT_WINDOW)
                        .filter(|fp| **fp == fingerprint)
                        .count();
                    recent_call_fingerprints.push(fingerprint.clone());
                    // Trim to keep memory bounded over long runs (only the last
                    // REPEAT_WINDOW are ever read).
                    if recent_call_fingerprints.len() > REPEAT_WINDOW {
                        let drop_n = recent_call_fingerprints.len() - REPEAT_WINDOW;
                        recent_call_fingerprints.drain(0..drop_n);
                    }

                    if same_count_in_window >= 2 {
                        // Third identical call in window → short-circuit as tool_failure.
                        let stuck_msg = format!(
                            "repeated identical tool call detected: '{}' invoked {} times in last {} steps with same args. Break out of stuck loop — try a different approach (todo_write to plan, gh/curl for research, or a different path/argument).",
                            tool_name,
                            same_count_in_window + 1,
                            REPEAT_WINDOW
                        );
                        if let Some(renderer) = renderer.as_mut() {
                            renderer.paint_tool_result(
                                crate::ui::stream::ToolResultKind::Failed,
                                &tool_name,
                                "stuck",
                                &stuck_msg,
                            );
                        }
                        let event_name = tool_name.clone();
                        observations.push(Observation::failed(tool_name, stuck_msg.clone()));
                        push_tool_event(
                            &mut tool_events,
                            run_events.as_ref(),
                            ToolEvent {
                                tool_name: event_name,
                                input: event_input,
                                output: stuck_msg,
                                status: crate::model::protocol::ObservationStatus::Failed,
                            },
                        );
                        continue;
                    }

                    // Phase 10c-2: 2nd identical call — emit a separate stuck-warning
                    // Observation BEFORE running the tool. Avoids burying the warning in the
                    // tail of a long tool output that head_trim / Todos summarize would eat,
                    // and works for both Ok and Err result paths.
                    if same_count_in_window == 1 {
                        let warning = format!(
                            "⚠ stuck-warning: '{tool_name}' was called with the same args last step. If output is unchanged, try a DIFFERENT approach (todo_write to plan, gh/curl for research, different path/args, or move to the next step)."
                        );
                        observations.push(Observation::ok("stuck-warning", warning));
                    }

                    match hooks.pre_tool_use(&context.task, &tool_name, &input) {
                        Ok(Some(hook_context)) => {
                            observations.push(Observation::ok(
                                "hook",
                                format!("pre_tool_use: {hook_context}"),
                            ));
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let raw = error.to_string();
                            if let Some(renderer) = renderer.as_mut() {
                                renderer.paint_tool_result(
                                    crate::ui::stream::ToolResultKind::Denied,
                                    &tool_name,
                                    "hook",
                                    &raw,
                                );
                            }
                            let event_name = tool_name.clone();
                            observations.push(Observation::failed(
                                tool_name,
                                format!("pre_tool_use hook blocked tool: {raw}"),
                            ));
                            push_tool_event(
                                &mut tool_events,
                                run_events.as_ref(),
                                ToolEvent {
                                    tool_name: event_name,
                                    input: event_input,
                                    output: raw,
                                    status: crate::model::protocol::ObservationStatus::Failed,
                                },
                            );
                            continue;
                        }
                    }

                    let mut execution_policy = policy.clone();
                    if let Some(permission) =
                        registry.permission_request_for(&tool_name, &input, &policy)
                    {
                        emit_permission_request(
                            run_events.as_ref(),
                            &tool_name,
                            &event_input,
                            &permission.kind,
                            &permission.target,
                        );
                        match hooks.permission_request(
                            &context.task,
                            &tool_name,
                            &input,
                            &permission.kind,
                            &permission.target,
                        ) {
                            Ok(Some(hook_context)) => {
                                observations.push(Observation::ok(
                                    "hook",
                                    format!("permission_request: {hook_context}"),
                                ));
                            }
                            Ok(None) => {}
                            Err(error) => {
                                let raw = error.to_string();
                                if let Some(renderer) = renderer.as_mut() {
                                    renderer.paint_tool_result(
                                        crate::ui::stream::ToolResultKind::Denied,
                                        &tool_name,
                                        "hook",
                                        &raw,
                                    );
                                }
                                let event_name = tool_name.clone();
                                observations.push(Observation::failed(
                                    tool_name,
                                    format!("permission_request hook blocked tool: {raw}"),
                                ));
                                push_tool_event(
                                    &mut tool_events,
                                    run_events.as_ref(),
                                    ToolEvent {
                                        tool_name: event_name,
                                        input: event_input,
                                        output: raw,
                                        status: crate::model::protocol::ObservationStatus::Failed,
                                    },
                                );
                                continue;
                            }
                        }

                        if let Some(resolver) = approval_resolver.as_ref() {
                            let approval_request = AgentApprovalRequest {
                                tool_name: tool_name.clone(),
                                input: event_input.clone(),
                                kind: permission.kind.clone(),
                                target: permission.target.clone(),
                            };
                            match resolver.borrow_mut().resolve(&approval_request)? {
                                AgentApprovalDecision::Approved => {
                                    execution_policy =
                                        policy.with_auto_approved_permission(&permission.kind);
                                }
                                AgentApprovalDecision::Denied => {
                                    let raw = format!(
                                        "permission denied for {}: {}",
                                        permission.kind, permission.target
                                    );
                                    if let Some(renderer) = renderer.as_mut() {
                                        renderer.paint_tool_result(
                                            crate::ui::stream::ToolResultKind::Denied,
                                            &tool_name,
                                            &permission.kind,
                                            &raw,
                                        );
                                    }
                                    let event_name = tool_name.clone();
                                    observations.push(Observation::failed(tool_name, raw.clone()));
                                    push_tool_event(
                                        &mut tool_events,
                                        run_events.as_ref(),
                                        ToolEvent {
                                            tool_name: event_name,
                                            input: event_input,
                                            output: raw,
                                            status:
                                                crate::model::protocol::ObservationStatus::Failed,
                                        },
                                    );
                                    continue;
                                }
                            }
                        }
                    }

                    match execute_tool_with_cancel(
                        &registry,
                        &tool_name,
                        input,
                        &execution_policy,
                        cancel_check.as_ref(),
                    ) {
                        Ok(mut output) => {
                            check_cancelled(cancel_check.as_ref())?;
                            if tool_name == "dispatch_subagent" {
                                if let Some(delegated_task) = event_input.get("task") {
                                    if todos
                                        .borrow_mut()
                                        .complete_in_progress_matching_subagent_task(delegated_task)
                                    {
                                        output.summary.push_str(
                                            "\nparent todos auto-advanced after subagent completion",
                                        );
                                    }
                                }
                            }
                            let kind = ObservationKind::from_tool_name(&tool_name);
                            let observation_summary = summarize_for_kind(&output.summary, kind);
                            // CR-1: user sees full body (output.summary), observation/transcript get trim.
                            if let Some(renderer) = renderer.as_mut() {
                                renderer.paint_tool_result(
                                    crate::ui::stream::ToolResultKind::Ok,
                                    &tool_name,
                                    kind.label(),
                                    &output.summary,
                                );
                            }
                            let event_name = tool_name.clone();
                            observations
                                .push(Observation::ok(tool_name, observation_summary.clone()));
                            if let Some(recovery_hint) = derive_recovery_hint_after_success(
                                &event_name,
                                &output.summary,
                                &available_tools,
                                primary_file.as_deref(),
                                &observations,
                            ) {
                                observations.push(Observation::ok("recovery_hint", recovery_hint));
                            }
                            if let Some(replan_hint) =
                                derive_replan_hint(&event_name, &output.summary, &observations)
                            {
                                observations.push(Observation::ok("replan_hint", replan_hint));
                            }
                            push_tool_event(
                                &mut tool_events,
                                run_events.as_ref(),
                                ToolEvent {
                                    tool_name: event_name,
                                    input: event_input,
                                    output: output.summary,
                                    status: crate::model::protocol::ObservationStatus::Ok,
                                },
                            );
                            push_post_tool_hook_observation(
                                &hooks,
                                &context.task,
                                &tool_events,
                                &mut observations,
                            );
                        }
                        Err(error) => {
                            check_cancelled(cancel_check.as_ref())?;
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
                            if let Some(renderer) = renderer.as_mut() {
                                renderer.paint_tool_result(
                                    result_kind,
                                    &tool_name,
                                    kind.label(),
                                    &raw,
                                );
                            }
                            let event_name = tool_name.clone();
                            observations
                                .push(Observation::failed(tool_name, observation_summary.clone()));
                            if let Some(recovery_hint) = derive_recovery_hint_after_failure(
                                &event_name,
                                &available_tools,
                                primary_file.as_deref(),
                                &observations,
                            ) {
                                observations.push(Observation::ok("recovery_hint", recovery_hint));
                            }
                            if let Some(replan_hint) =
                                derive_replan_hint(&event_name, &observation_summary, &observations)
                            {
                                observations.push(Observation::ok("replan_hint", replan_hint));
                            }
                            push_tool_event(
                                &mut tool_events,
                                run_events.as_ref(),
                                ToolEvent {
                                    tool_name: event_name,
                                    input: event_input,
                                    output: raw,
                                    status: crate::model::protocol::ObservationStatus::Failed,
                                },
                            );
                            push_post_tool_hook_observation(
                                &hooks,
                                &context.task,
                                &tool_events,
                                &mut observations,
                            );
                        }
                    }
                }
                ModelAction::Finish => {
                    break;
                }
            }
        }

        if emit_progress {
            if let Some(test_command) = suggested_test_command.as_deref() {
                println!();
                println!("Suggested validation command: {test_command}");
            }
        }

        let _ = hooks.session_stop(&context.task, "finish", &last_message)?;

        if persist_session {
            let store = SessionStore::new(self.config.workspace.session_dir());
            let snapshot = SessionSnapshot::new(context.task, profile.name);
            store.save(&snapshot)?;
        }

        Ok(RunResult {
            final_message: last_message,
            tool_events,
            usage: total_usage,
        })
    }
}

struct ReasoningCaptureEvents<'a> {
    inner: &'a mut dyn StreamEvents,
    reasoning: String,
}

impl<'a> ReasoningCaptureEvents<'a> {
    fn new(inner: &'a mut dyn StreamEvents) -> Self {
        Self {
            inner,
            reasoning: String::new(),
        }
    }

    fn into_reasoning(self) -> String {
        self.reasoning
    }
}

impl StreamEvents for ReasoningCaptureEvents<'_> {
    fn on_reasoning_delta(&mut self, chunk: &str) {
        if !chunk.is_empty() {
            self.reasoning.push_str(chunk);
        }
        self.inner.on_reasoning_delta(chunk);
    }

    fn on_text_delta(&mut self, chunk: &str) {
        self.inner.on_text_delta(chunk);
    }

    fn on_assistant_done(&mut self, full_text: &str) {
        self.inner.on_assistant_done(full_text);
    }

    fn on_tool_call(&mut self, name: &str, input: &BTreeMap<String, String>) {
        self.inner.on_tool_call(name, input);
    }
}

fn recent_step_replay_entry(message: &str, reasoning: &str) -> Option<String> {
    let message = compact_replay_text(message, 120);
    let reasoning = compact_replay_text(reasoning, 160);
    match (message.is_empty(), reasoning.is_empty()) {
        (true, true) => None,
        (false, true) => Some(message),
        (true, false) => Some(format!("reasoning: {reasoning}")),
        (false, false) => Some(format!("reasoning: {reasoning} | assistant: {message}")),
    }
}

fn compact_replay_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let head = normalized.chars().take(max_chars).collect::<String>();
    format!("{head}...")
}

fn emit_tool_call(
    run_events: Option<&SharedAgentRunEvents>,
    tool_name: &str,
    input: &BTreeMap<String, String>,
) {
    if let Some(events) = run_events {
        events.borrow_mut().on_tool_call(tool_name, input);
    }
}

fn emit_permission_request(
    run_events: Option<&SharedAgentRunEvents>,
    tool_name: &str,
    input: &BTreeMap<String, String>,
    kind: &str,
    target: &str,
) {
    if let Some(events) = run_events {
        events
            .borrow_mut()
            .on_permission_request(tool_name, input, kind, target);
    }
}

fn push_tool_event(
    tool_events: &mut Vec<ToolEvent>,
    run_events: Option<&SharedAgentRunEvents>,
    event: ToolEvent,
) {
    if let Some(events) = run_events {
        events.borrow_mut().on_tool_result(&event);
    }
    tool_events.push(event);
}

fn model_respond_with_cancel<C: ModelClient>(
    client: &C,
    request: ModelRequest,
    events: &mut dyn StreamEvents,
    cancel_check: Option<&SharedAgentCancelCheck>,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    if let Some(check) = cancel_check {
        let mut guard = check.borrow_mut();
        let mut adapter = AgentCancelAdapter { inner: &mut *guard };
        client.respond_with_cancel(request, events, Some(&mut adapter))
    } else {
        client.respond_with_cancel(request, events, None)
    }
}

fn execute_tool_with_cancel(
    registry: &crate::tools::registry::ToolRegistry,
    tool_name: &str,
    input: crate::tools::types::ToolInput,
    policy: &ExecutionPolicy,
    cancel_check: Option<&SharedAgentCancelCheck>,
) -> AppResult<crate::tools::types::ToolOutput> {
    if let Some(check) = cancel_check {
        let mut guard = check.borrow_mut();
        let mut adapter = AgentCancelAdapter { inner: &mut *guard };
        registry.execute_with_policy_and_cancel(tool_name, input, policy, Some(&mut adapter))
    } else {
        registry.execute_with_policy_and_cancel(tool_name, input, policy, None)
    }
}

fn check_cancelled(cancel_check: Option<&SharedAgentCancelCheck>) -> AppResult<()> {
    if let Some(check) = cancel_check {
        let mut guard = check.borrow_mut();
        if AgentCancelCheck::is_cancelled(&mut *guard)? {
            return Err(app_error("agent run cancelled"));
        }
    }
    Ok(())
}

fn push_post_tool_hook_observation(
    hooks: &crate::core::hooks::HookRunner,
    task: &str,
    tool_events: &[ToolEvent],
    observations: &mut Vec<Observation>,
) {
    let Some(event) = tool_events.last() else {
        return;
    };
    match hooks.post_tool_use(
        task,
        &event.tool_name,
        &event.input,
        event.status,
        &event.output,
    ) {
        Ok(Some(hook_context)) => {
            observations.push(Observation::ok(
                "hook",
                format!("post_tool_use: {hook_context}"),
            ));
        }
        Ok(None) => {}
        Err(error) => {
            observations.push(Observation::failed(
                "hook",
                format!("post_tool_use hook failed: {error}"),
            ));
        }
    }
}

fn derive_recovery_hint_after_success(
    tool_name: &str,
    output: &str,
    available_tools: &[String],
    primary_file: Option<&str>,
    observations: &[Observation],
) -> Option<String> {
    if tool_name == "search_text" && output.starts_with("No matches for `") {
        return format_recovery_hint(
            "search_text",
            preferred_listing_or_search_tool(available_tools)?,
            "search_text returned no matches, inspect the repository layout or broaden the lookup before retrying the query",
            None,
            None,
        );
    }

    if tool_name == "run_shell" && shell_exit_code(output).is_some_and(|code| code != 0) {
        if let Some(plan) =
            shell_recovery_directive(output, available_tools, primary_file, observations)
        {
            return format_recovery_hint(
                "run_shell",
                plan.next,
                &plan.reason,
                plan.query.as_deref(),
                plan.path.as_deref(),
            );
        }
    }

    None
}

fn derive_recovery_hint_after_failure(
    tool_name: &str,
    available_tools: &[String],
    primary_file: Option<&str>,
    observations: &[Observation],
) -> Option<String> {
    match tool_name {
        "read_file" => format_recovery_hint(
            "read_file",
            preferred_search_or_listing_tool(available_tools)?,
            "read_file failed, locate the correct file path before retrying the read",
            None,
            None,
        ),
        "dispatch_subagent" | "dispatch_subagents" => format_recovery_hint(
            "dispatch_subagent",
            preferred_search_or_listing_tool(available_tools)?,
            "subagent dispatch failed, continue locally with a direct inspection step",
            None,
            None,
        ),
        "run_shell" => format_recovery_hint(
            "run_shell",
            preferred_shell_recovery_tool(available_tools, primary_file, observations)?,
            "run_shell failed before completing, inspect the relevant code or diff before retrying the command",
            None,
            None,
        ),
        _ => None,
    }
}

struct RecoveryDirective {
    next: &'static str,
    reason: String,
    query: Option<String>,
    path: Option<String>,
}

fn derive_replan_hint(
    tool_name: &str,
    output: &str,
    observations: &[Observation],
) -> Option<String> {
    if tool_name == "dispatch_subagent"
        && child_outcome(output).is_some_and(|outcome| outcome == "blocked")
    {
        return Some(
            "reason=subagent blocker; action=replan parent todo list around the blocker"
                .to_string(),
        );
    }

    if tool_name == "dispatch_subagents" && parallel_child_blocked(output) {
        return Some(
            "reason=subagent blocker; action=replan parent todo list around the blocker"
                .to_string(),
        );
    }

    if tool_name == "recovery_hint" {
        return None;
    }

    let recent_recovery_hints = observations
        .iter()
        .rev()
        .take(6)
        .filter(|observation| observation.tool_name == "recovery_hint")
        .count();
    if recent_recovery_hints >= 2 {
        return Some(
            "reason=multiple recovery hints in recent steps; action=replan the remaining todo list before continuing".to_string(),
        );
    }

    None
}

fn parallel_child_blocked(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.starts_with("meta.parallel_child_") && line.contains("_outcome=blocked"))
}

fn child_outcome(summary: &str) -> Option<&str> {
    summary
        .lines()
        .find_map(|line| line.strip_prefix("meta.child_outcome="))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            summary
                .lines()
                .find_map(|line| line.strip_prefix("child outcome: "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
}

fn preferred_search_or_listing_tool(available_tools: &[String]) -> Option<&'static str> {
    if available_tools.iter().any(|tool| tool == "search_text") {
        Some("search_text")
    } else if available_tools.iter().any(|tool| tool == "list_files") {
        Some("list_files")
    } else {
        None
    }
}

fn preferred_listing_or_search_tool(available_tools: &[String]) -> Option<&'static str> {
    if available_tools.iter().any(|tool| tool == "list_files") {
        Some("list_files")
    } else if available_tools.iter().any(|tool| tool == "search_text") {
        Some("search_text")
    } else {
        None
    }
}

fn preferred_shell_recovery_tool(
    available_tools: &[String],
    primary_file: Option<&str>,
    observations: &[Observation],
) -> Option<&'static str> {
    let has_apply_patch_success = observations
        .iter()
        .any(|observation| observation.tool_name == "apply_patch" && !observation.is_failure());
    if has_apply_patch_success && available_tools.iter().any(|tool| tool == "git_diff") {
        return Some("git_diff");
    }
    if primary_file.is_some() && available_tools.iter().any(|tool| tool == "read_file") {
        return Some("read_file");
    }
    preferred_search_or_listing_tool(available_tools)
}

fn format_recovery_hint(
    after: &str,
    next: &str,
    reason: &str,
    query: Option<&str>,
    path: Option<&str>,
) -> Option<String> {
    let mut parts = vec![format!("after={after}"), format!("next={next}")];
    if let Some(query) = query.filter(|value| !value.is_empty()) {
        parts.push(format!("query={query}"));
    }
    if let Some(path) = path.filter(|value| !value.is_empty()) {
        parts.push(format!("path={path}"));
    }
    parts.push(format!("reason={reason}"));
    Some(parts.join("; "))
}

fn shell_exit_code(output: &str) -> Option<i32> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("meta.exit_code="))
        .or_else(|| {
            output
                .lines()
                .find_map(|line| line.strip_prefix("exit_code: "))
        })
        .and_then(|raw| raw.trim().parse::<i32>().ok())
}

fn shell_meta_value<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("meta.{key}=");
    output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
}

fn shell_failure_reason(output: &str) -> String {
    let failure_kind = shell_meta_value(output, "failure_kind").unwrap_or("command_failure");
    let stderr_summary = shell_meta_value(output, "stderr_summary");
    let failed_tests = shell_meta_value(output, "failed_tests");

    match failure_kind {
        "test_failure" => {
            if let Some(failed_tests) = failed_tests.filter(|value| !value.is_empty()) {
                format!(
                    "run_shell reported failing tests ({failed_tests}), inspect the relevant code or diff before retrying the command"
                )
            } else if let Some(stderr_summary) = stderr_summary {
                format!(
                    "run_shell reported a test failure ({stderr_summary}), inspect the relevant code or diff before retrying the command"
                )
            } else {
                "run_shell reported a test failure, inspect the relevant code or diff before retrying the command"
                    .to_string()
            }
        }
        "lint_failure" => {
            if let Some(stderr_summary) = stderr_summary {
                format!(
                    "run_shell reported a lint failure ({stderr_summary}), inspect the relevant code or diff before retrying the command"
                )
            } else {
                "run_shell reported a lint failure, inspect the relevant code or diff before retrying the command"
                    .to_string()
            }
        }
        "build_failure" => {
            if let Some(stderr_summary) = stderr_summary {
                format!(
                    "run_shell reported a build failure ({stderr_summary}), inspect the relevant code or diff before retrying the command"
                )
            } else {
                "run_shell reported a build failure, inspect the relevant code or diff before retrying the command"
                    .to_string()
            }
        }
        _ => {
            if let Some(stderr_summary) = stderr_summary {
                format!(
                    "run_shell exited non-zero ({stderr_summary}), inspect the relevant code or diff before retrying the command"
                )
            } else {
                "run_shell exited non-zero, inspect the relevant code or diff before retrying the command"
                    .to_string()
            }
        }
    }
}

fn shell_recovery_directive(
    output: &str,
    available_tools: &[String],
    primary_file: Option<&str>,
    observations: &[Observation],
) -> Option<RecoveryDirective> {
    let failure_kind = shell_meta_value(output, "failure_kind").unwrap_or("command_failure");
    let failed_tests = shell_meta_value(output, "failed_tests");
    let stderr_summary = shell_meta_value(output, "stderr_summary");
    let reason = shell_failure_reason(output);
    let has_apply_patch_success = observations
        .iter()
        .any(|observation| observation.tool_name == "apply_patch" && !observation.is_failure());

    match failure_kind {
        "test_failure" => {
            if let Some(path) = failed_test_path(failed_tests)
                .filter(|path| is_javascript_test_path(path))
                .filter(|_| available_tools.iter().any(|tool| tool == "read_file"))
            {
                return Some(RecoveryDirective {
                    next: "read_file",
                    reason,
                    query: None,
                    path: Some(path),
                });
            }
            if has_apply_patch_success && available_tools.iter().any(|tool| tool == "git_diff") {
                return Some(RecoveryDirective {
                    next: "git_diff",
                    reason,
                    query: None,
                    path: None,
                });
            }
            if let Some(path) = failed_test_path(failed_tests)
                .filter(|_| available_tools.iter().any(|tool| tool == "read_file"))
            {
                return Some(RecoveryDirective {
                    next: "read_file",
                    reason,
                    query: None,
                    path: Some(path),
                });
            }
            if let Some(primary_file) =
                primary_file.filter(|_| available_tools.iter().any(|tool| tool == "read_file"))
            {
                return Some(RecoveryDirective {
                    next: "read_file",
                    reason,
                    query: None,
                    path: Some(primary_file.to_string()),
                });
            }
        }
        "lint_failure" | "build_failure" => {
            if let Some(query) = stderr_summary
                .and_then(derive_search_query_like)
                .filter(|_| available_tools.iter().any(|tool| tool == "search_text"))
            {
                return Some(RecoveryDirective {
                    next: "search_text",
                    reason,
                    query: Some(query),
                    path: None,
                });
            }
            if let Some(primary_file) =
                primary_file.filter(|_| available_tools.iter().any(|tool| tool == "read_file"))
            {
                return Some(RecoveryDirective {
                    next: "read_file",
                    reason,
                    query: None,
                    path: Some(primary_file.to_string()),
                });
            }
        }
        _ => {}
    }

    Some(RecoveryDirective {
        next: preferred_shell_recovery_tool(available_tools, primary_file, observations)?,
        reason,
        query: None,
        path: None,
    })
}

fn derive_search_query_like(text: &str) -> Option<String> {
    first_quoted_segment(text)
        .or_else(|| identifier_like_token_like(text))
        .or_else(|| {
            text.split_whitespace()
                .map(|word| {
                    word.trim_matches(|ch: char| {
                        !ch.is_ascii_alphanumeric() && ch != '_' && ch != ':' && ch != '-'
                    })
                })
                .find(|word| word.len() >= 3 && word.chars().any(|ch| ch.is_ascii_alphanumeric()))
                .map(str::to_string)
        })
}

fn first_quoted_segment(text: &str) -> Option<String> {
    for marker in ['`', '"', '\''] {
        let mut parts = text.split(marker);
        let _ = parts.next();
        if let Some(inner) = parts.next().map(str::trim).filter(|part| !part.is_empty()) {
            return Some(inner.to_string());
        }
    }
    None
}

fn identifier_like_token_like(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|ch: char| {
                !ch.is_ascii_alphanumeric() && ch != '_' && ch != ':' && ch != '-'
            })
        })
        .find(|word| {
            !word.is_empty()
                && (word.contains('_')
                    || word.contains("::")
                    || word.chars().any(|ch| ch.is_ascii_uppercase()))
        })
        .map(str::to_string)
}

fn failed_test_path(failed_tests: Option<&str>) -> Option<String> {
    let first = failed_tests?
        .split(',')
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())?;
    let candidate = first.split("::").next().unwrap_or(first).trim();
    if candidate.contains('/') || candidate.ends_with(".py") || candidate.ends_with(".rs") {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn is_javascript_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    (lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx"))
        && (lower.contains("/test") || lower.contains(".test.") || lower.contains(".spec."))
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
const SUBAGENT_NUDGE: &str = "\n\n[sub-agent delegation]\nYou may call `dispatch_subagent` for one independent subtask, or `dispatch_subagents` when the user explicitly asks for parallel work or when multiple independent workstreams can run concurrently.\n- Only dispatch self-contained workstreams with concrete tasks and disjoint write scopes.\n- Prefer dispatch after a todo plan exists, or when the split is already obvious.\n- Nested dispatch is bounded; use it only when the child has its own clearly separable subtask.\n- Do NOT dispatch trivial reads, tiny edits, or work you can finish directly in one step.\n- Treat child results as summarized observations, read back child-edited files before relying on patches, then continue the parent plan.";
const EXPLICIT_PLANNING_BOOTSTRAP_NUDGE: &str = "\n\n[explicit-planning mode]\nThis task is large enough that you MUST create and follow a concrete plan.\n- If no todo plan exists yet, your NEXT turn MUST call todo_write with 3-7 concrete steps before repository inspection, edits, or test runs.\n- Keep exactly one todo in_progress at a time.\n- After a plan exists, execute the current in_progress step instead of starting over.\n- Do NOT rewrite the whole plan unless new evidence changes the approach.\n- Your assistant message should say which plan step you are executing now.";
const EXPLICIT_PLAN_EXECUTION_NUDGE: &str = "\n\n[plan execution]\nA todo plan already exists.\n- Continue from the current in_progress step.\n- Update todo_write only when a step changes status or new work is discovered.\n- Do NOT recreate the plan from scratch while execution is already in progress.";

/// Phase 10c-3: research-bootstrap nudge. Prepended to system prompt when the
/// workspace is empty AND the task text contains research keywords. Without
/// this, dogfood with v4-pro showed agents oscillating between mkdir +
/// todo_write for 30 steps without ever issuing a gh/curl call. Strong-style
/// directive that matches the empirically-observed failure mode.
const RESEARCH_BOOTSTRAP_NUDGE: &str = "\n\n[research-bootstrap mode]\nThe workspace is INTENTIONALLY EMPTY. You are doing research, not editing files.\n- Step 1 MUST be a REAL research call through `run_shell`, using `gh search ...` or `curl -sSL ...`.\n- DO NOT start with todo_write, mkdir, list_files, or any setup-only shell command.\n- DO NOT call mkdir, list_files, or run_shell with setup commands — the workspace is empty by design.\n- DO NOT repeat the same setup tool call. Each step should make NEW progress (a new gh query, a new curl URL, or a todo_write update after concrete research results exist).\n- After the first research result lands, use todo_write to track follow-up steps if the task is multi-step.\n- After research is complete, use apply_patch to write findings to a markdown file.";

fn should_apply_research_bootstrap(
    task: &str,
    workspace_root: &Path,
    available_tools: &[String],
) -> bool {
    if !task_looks_like_research(task) {
        return false;
    }
    if !workspace_is_bootstrap_empty(workspace_root) {
        return false;
    }
    available_tools.iter().any(|tool| tool == "run_shell")
}

fn task_looks_like_research(task: &str) -> bool {
    let lower = task.to_lowercase();
    let keywords = [
        "research",
        "investigate",
        "调研",
        "explore",
        "find on github",
        "gh search",
        "gh repo",
        "curl",
        "search github",
        "look up",
    ];
    keywords.iter().any(|kw| lower.contains(kw))
}

fn workspace_is_bootstrap_empty(workspace_root: &Path) -> bool {
    std::fs::read_dir(workspace_root)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).all(|entry| {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                name_str.starts_with('.') || name_str == ".dscode"
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
fn build_system_prompt(skill_name: Option<&SkillSpec>) -> String {
    build_system_prompt_with_flags(skill_name, false, false, false, false)
}

/// Phase 10c-3: tool-call concurrency constraint. DeepSeek v4 (both flash + pro)
/// happily emits parallel tool calls when the task mentions multiple subtopics
/// ("research these 4 topics"). dscode's parser rejects them (C3 fail-loud) so
/// the agent gets a fatal error instead of useful work. State this constraint
/// explicitly so the model issues sequential calls.
const ONE_TOOL_PER_TURN_NUDGE: &str = "\n\nALWAYS emit exactly ONE tool call per turn. NEVER emit parallel tool calls — the runtime rejects them with a hard error. Process multiple subtopics SEQUENTIALLY across turns.";

fn should_use_explicit_planning(
    task: &str,
    skill: Option<&SkillSpec>,
    available_tools: &[String],
) -> bool {
    if !available_tools.iter().any(|tool| tool == "todo_write") {
        return false;
    }

    let lower = task.to_lowercase();
    if crate::model::deepseek::task_has_direct_edit_request(task) {
        return false;
    }

    if skill.map(|s| s.suggested_steps.len() >= 3).unwrap_or(false) {
        return true;
    }

    let complexity_markers = [
        " and ",
        " then ",
        " across ",
        " multiple ",
        " end-to-end",
        " investigate",
        " research",
        "improve",
        "enhance",
        "stabilize",
        "hardening",
        "optimize",
        " better",
        " more like ",
        "close the gap",
        "gap closure",
        "production-ready",
        "production ready",
        "product-ready",
        "product ready",
        "productize",
        "productionize",
        "ship-ready",
        "ship ready",
        "daily coding",
        "daily use",
        " implement",
        " refactor",
        " debug",
        " review",
        " write ",
        " update ",
        " fix ",
        " verify ",
        " test ",
        " build ",
    ];

    complexity_markers
        .iter()
        .any(|marker| lower.contains(marker))
        || task.split_whitespace().count() >= 10
}

fn build_system_prompt_with_flags(
    skill_name: Option<&SkillSpec>,
    research_bootstrap: bool,
    planning_mode: bool,
    has_plan: bool,
    subagent_available: bool,
) -> String {
    let mut prompt = String::from(
        "You are the offline planning layer for DeepSeekCode. Prefer repository inspection before edits.",
    );
    prompt.push_str(ONE_TOOL_PER_TURN_NUDGE);
    // Note: ONE_TOOL_PER_TURN_NUDGE starts with explicit "\n\n" so order with
    // skill.system_append (added below) is well-defined regardless of trailing
    // punctuation in the base prompt.
    if let Some(skill) = skill_name {
        prompt.push_str(&format!(" Active skill: {}.", skill.name));
        if !skill.description.is_empty() {
            prompt.push_str(&format!(" Skill description: {}.", skill.description));
        }
        if !skill.references.is_empty() {
            prompt.push_str(" Skill references:");
            for reference in &skill.references {
                prompt.push_str(&format!(" [{reference}]"));
            }
            prompt.push('.');
        }
        if !skill.system_append.is_empty() {
            prompt.push(' ');
            prompt.push_str(skill.system_append.trim());
        }
    }
    if research_bootstrap {
        prompt.push_str(RESEARCH_BOOTSTRAP_NUDGE);
    }
    if planning_mode {
        if has_plan {
            prompt.push_str(EXPLICIT_PLAN_EXECUTION_NUDGE);
        } else {
            prompt.push_str(EXPLICIT_PLANNING_BOOTSTRAP_NUDGE);
        }
    }
    prompt.push_str(TODO_NUDGE);
    if subagent_available {
        prompt.push_str(SUBAGENT_NUDGE);
    }
    prompt
}

fn build_system_prompt_with_workspace_instructions(
    skill_name: Option<&SkillSpec>,
    research_bootstrap: bool,
    planning_mode: bool,
    has_plan: bool,
    subagent_available: bool,
    workspace_instructions: &[crate::core::instructions::InstructionFile],
) -> String {
    let mut prompt = build_system_prompt_with_flags(
        skill_name,
        research_bootstrap,
        planning_mode,
        has_plan,
        subagent_available,
    );
    if let Some(instructions) =
        crate::core::instructions::render_workspace_instructions(workspace_instructions)
    {
        prompt.push_str("\n\n");
        prompt.push_str(&instructions);
    }
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
    fn build_system_prompt_includes_workspace_instructions() {
        let instructions = [crate::core::instructions::InstructionFile {
            path: std::path::PathBuf::from("AGENTS.md"),
            content: "Run cargo test before committing.".to_string(),
            truncated: false,
        }];
        let prompt = super::build_system_prompt_with_workspace_instructions(
            None,
            false,
            false,
            false,
            false,
            &instructions,
        );

        assert!(prompt.contains("Workspace instructions"));
        assert!(prompt.contains("AGENTS.md"));
        assert!(prompt.contains("Run cargo test before committing."));
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
            triggers: Vec::new(),
            initial_todos: Vec::new(),
            references: Vec::new(),
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
    fn research_bootstrap_keyword_match_detects_research_in_task() {
        let prompt = super::build_system_prompt_with_flags(None, true, false, false, false);
        assert!(prompt.contains("research-bootstrap mode"));
        assert!(prompt.contains("INTENTIONALLY EMPTY"));
        assert!(prompt.contains("gh search"));
        assert!(prompt.contains("Step 1 MUST be a REAL research call"));
        assert!(prompt.contains("DO NOT start with todo_write"));
        assert!(prompt.contains("DO NOT call mkdir"));
    }

    #[test]
    fn research_bootstrap_disabled_omits_nudge() {
        let prompt = super::build_system_prompt_with_flags(None, false, false, false, false);
        assert!(!prompt.contains("research-bootstrap mode"));
        assert!(!prompt.contains("INTENTIONALLY EMPTY"));
        // TODO_NUDGE still applies (always on)
        assert!(prompt.contains("todo_write"));
    }

    #[test]
    fn explicit_planning_prompt_requires_todo_plan_before_execution() {
        let prompt = super::build_system_prompt_with_flags(None, false, true, false, false);
        assert!(prompt.contains("explicit-planning mode"));
        assert!(prompt.contains("NEXT turn MUST call todo_write"));
        assert!(prompt.contains("execute"));
    }

    #[test]
    fn explicit_planning_prompt_switches_to_execution_once_plan_exists() {
        let prompt = super::build_system_prompt_with_flags(None, false, true, true, false);
        assert!(prompt.contains("plan execution"));
        assert!(prompt.contains("Continue from the current in_progress step"));
        assert!(!prompt.contains("NEXT turn MUST call todo_write"));
    }

    #[test]
    fn subagent_prompt_nudge_only_appears_when_tool_available() {
        let prompt = super::build_system_prompt_with_flags(None, false, false, false, true);
        assert!(prompt.contains("sub-agent delegation"));
        assert!(prompt.contains("dispatch_subagent"));

        let without = super::build_system_prompt_with_flags(None, false, false, false, false);
        assert!(!without.contains("sub-agent delegation"));
    }

    #[test]
    fn workspace_is_bootstrap_empty_ignores_hidden_entries() {
        let dir = unique_tmp("bootstrap_hidden_only");
        std::fs::create_dir_all(dir.join(".dscode")).unwrap();
        std::fs::write(dir.join(".gitkeep"), "").unwrap();
        assert!(super::workspace_is_bootstrap_empty(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn workspace_is_bootstrap_empty_rejects_visible_entries() {
        let dir = unique_tmp("bootstrap_visible_file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "hello").unwrap();
        assert!(!super::workspace_is_bootstrap_empty(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn research_bootstrap_requires_keyword_empty_workspace_and_run_shell() {
        let dir = unique_tmp("bootstrap_research");
        std::fs::create_dir_all(dir.join(".dscode")).unwrap();

        let tools = vec!["run_shell".to_string(), "todo_write".to_string()];
        assert!(super::should_apply_research_bootstrap(
            "research the ACP protocol on github",
            &dir,
            &tools,
        ));

        let no_shell = vec!["todo_write".to_string()];
        assert!(!super::should_apply_research_bootstrap(
            "research the ACP protocol on github",
            &dir,
            &no_shell,
        ));

        std::fs::write(dir.join("README.md"), "not empty").unwrap();
        assert!(!super::should_apply_research_bootstrap(
            "research the ACP protocol on github",
            &dir,
            &tools,
        ));

        let _ = std::fs::remove_dir_all(dir);
    }

    fn unique_tmp(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("dscode_loop_runtime_test_{label}_{nanos}"))
    }

    #[test]
    fn task_looks_like_research_matches_expected_keywords() {
        assert!(!super::task_looks_like_research(""));
        assert!(!super::task_looks_like_research("rename foo to bar"));
        assert!(super::task_looks_like_research(
            "research the ACP protocol on github"
        ));
        assert!(super::task_looks_like_research("帮我调研这个项目"));
    }

    #[test]
    fn shell_exit_code_reads_structured_metadata_first() {
        let output = "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nexit_code: 0";
        assert_eq!(super::shell_exit_code(output), Some(101));
    }

    #[test]
    fn shell_failure_reason_mentions_failed_tests_when_present() {
        let output = "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=parser::rejects_bad_input\nmeta.stderr_summary=test failed\nexit_code: 101";
        let reason = super::shell_failure_reason(output);
        assert!(reason.contains("parser::rejects_bad_input"));
        assert!(reason.contains("failing tests"));
    }

    #[test]
    fn derive_replan_hint_triggers_after_multiple_recovery_hints() {
        let observations = vec![
            Observation::ok("search_text", "No matches for `x`."),
            Observation::ok(
                "recovery_hint",
                "after=search_text; next=list_files; reason=first recovery",
            ),
            Observation::failed("read_file", "No such file"),
            Observation::ok(
                "recovery_hint",
                "after=read_file; next=search_text; reason=second recovery",
            ),
        ];
        let hint = super::derive_replan_hint("read_file", "No such file", &observations)
            .expect("expected replan hint");
        assert!(hint.contains("multiple recovery hints"));
    }

    #[test]
    fn derive_replan_hint_triggers_for_blocked_subagent_summary() {
        let observations = vec![Observation::ok(
            "dispatch_subagent",
            "meta.child_outcome=blocked\nchild outcome: blocked",
        )];
        let hint = super::derive_replan_hint(
            "dispatch_subagent",
            "meta.child_outcome=blocked\nsubagent finished task `x`\nchild outcome: blocked",
            &observations,
        )
        .expect("expected subagent blocker replan hint");
        assert!(hint.contains("subagent blocker"));
    }

    #[test]
    fn derive_replan_hint_triggers_for_blocked_parallel_subagent_summary() {
        let hint = super::derive_replan_hint(
            "dispatch_subagents",
            "meta.parallel_child_1_outcome=ok\nmeta.parallel_child_2_outcome=blocked",
            &[],
        )
        .expect("expected parallel subagent blocker replan hint");
        assert!(hint.contains("subagent blocker"));
    }

    #[test]
    fn shell_recovery_directive_uses_read_file_for_failed_test_path() {
        let tools = vec!["read_file".to_string(), "search_text".to_string()];
        let output = "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=src/cli/app.rs::cli_from_argv_routes_benchmark_subcommand\nmeta.stderr_summary=test failed\nexit_code: 101";
        let plan = super::shell_recovery_directive(output, &tools, None, &[])
            .expect("expected recovery directive");
        assert_eq!(plan.next, "read_file");
        assert_eq!(plan.path.as_deref(), Some("src/cli/app.rs"));
    }

    #[test]
    fn shell_recovery_directive_prefers_js_test_file_after_failed_validation() {
        let tools = vec!["read_file".to_string(), "git_diff".to_string()];
        let observations = vec![Observation::ok("apply_patch", "patched src/math.js")];
        let output = "meta.command_kind=test\nmeta.exit_code=1\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=test/math.test.js\nmeta.stderr_summary=test failed\nexit_code: 1";
        let plan =
            super::shell_recovery_directive(output, &tools, Some("src/math.js"), &observations)
                .expect("expected recovery directive");
        assert_eq!(plan.next, "read_file");
        assert_eq!(plan.path.as_deref(), Some("test/math.test.js"));
    }

    #[test]
    fn shell_recovery_directive_uses_search_text_for_lint_failure_query() {
        let tools = vec!["search_text".to_string(), "read_file".to_string()];
        let output = "meta.command_kind=lint\nmeta.exit_code=1\nmeta.result=failed\nmeta.failure_kind=lint_failure\nmeta.stderr_summary=cannot find value `dispatch_subagent` in this scope\nexit_code: 1";
        let plan = super::shell_recovery_directive(output, &tools, None, &[])
            .expect("expected recovery directive");
        assert_eq!(plan.next, "search_text");
        assert_eq!(plan.query.as_deref(), Some("dispatch_subagent"));
    }

    #[test]
    fn explicit_planning_heuristic_skips_simple_replace_task() {
        let tools = vec!["todo_write".to_string()];
        assert!(!super::should_use_explicit_planning(
            "replace \"a\" with \"b\" in src/lib.rs",
            None,
            &tools,
        ));
    }

    #[test]
    fn explicit_planning_heuristic_skips_pr_replace_task_when_edit_request_is_clear() {
        let tools = vec!["todo_write".to_string()];
        assert!(!super::should_use_explicit_planning(
            "Address PR #44 review feedback: replace `a - b` with `a + b` in src/lib.rs and validate with cargo test.",
            None,
            &tools,
        ));
    }

    #[test]
    fn explicit_planning_heuristic_triggers_for_complex_task_or_skill_steps() {
        let tools = vec!["todo_write".to_string()];
        assert!(super::should_use_explicit_planning(
            "implement the new auth flow and verify the tests still pass",
            None,
            &tools,
        ));

        use crate::skills::schema::{SkillPolicy, SkillSpec};
        let skill = SkillSpec {
            name: "demo".to_string(),
            description: "demo skill".to_string(),
            allowed_tools: Vec::new(),
            system_append: String::new(),
            suggested_steps: vec!["one".to_string(), "two".to_string(), "three".to_string()],
            triggers: Vec::new(),
            initial_todos: Vec::new(),
            references: Vec::new(),
            policy: SkillPolicy {
                require_write_confirmation: false,
                require_shell_confirmation: false,
                shell_allowlist: Vec::new(),
            },
        };
        assert!(super::should_use_explicit_planning(
            "short task",
            Some(&skill),
            &tools
        ));
    }

    #[test]
    fn explicit_planning_heuristic_triggers_for_ambiguous_improvement_tasks() {
        let tools = vec!["todo_write".to_string()];
        assert!(super::should_use_explicit_planning(
            "improve benchmark reliability",
            None,
            &tools,
        ));
        assert!(super::should_use_explicit_planning(
            "make the CLI onboarding better",
            None,
            &tools,
        ));
        assert!(super::should_use_explicit_planning(
            "make DeepSeekCode more like Claude Code",
            None,
            &tools,
        ));
        assert!(super::should_use_explicit_planning(
            "close the product gap for PR review",
            None,
            &tools,
        ));
        assert!(super::should_use_explicit_planning(
            "make the CLI production-ready",
            None,
            &tools,
        ));
        assert!(super::should_use_explicit_planning(
            "productionize DeepSeekCode for daily coding work",
            None,
            &tools,
        ));
        assert!(super::should_use_explicit_planning(
            "make this ship-ready for daily use",
            None,
            &tools,
        ));
    }

    #[test]
    fn build_system_prompt_includes_skill_references_when_present() {
        use crate::skills::schema::{SkillPolicy, SkillSpec};
        let skill = SkillSpec {
            name: "demo".to_string(),
            description: "demo skill".to_string(),
            allowed_tools: Vec::new(),
            system_append: String::new(),
            suggested_steps: Vec::new(),
            triggers: Vec::new(),
            initial_todos: Vec::new(),
            references: vec!["docs/guide.md".to_string(), "README.md".to_string()],
            policy: SkillPolicy {
                require_write_confirmation: false,
                require_shell_confirmation: false,
                shell_allowlist: Vec::new(),
            },
        };
        let prompt = super::build_system_prompt(Some(&skill));
        assert!(prompt.contains("Skill references: [docs/guide.md] [README.md]."));
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
                ..AgentLoopOptions::default()
            },
            &client,
        );

        let captured = client.captured_recent_steps.borrow();
        assert_eq!(captured.len(), 3, "should have called respond 3 times");
        // First call: no prior steps yet.
        assert!(
            captured[0].is_empty(),
            "step 1 should see empty recent_steps"
        );
        // Second call: should see step 1's message.
        assert_eq!(captured[1].len(), 1);
        assert!(captured[1][0].contains("step ONE"));
        // Third call: should see steps 1 + 2.
        assert_eq!(captured[2].len(), 2);
        assert!(captured[2][0].contains("step ONE"));
        assert!(captured[2][1].contains("step TWO"));
    }

    struct ScriptedReasoningClient {
        captured_recent_steps: RefCell<Vec<Vec<String>>>,
    }

    impl ModelClient for ScriptedReasoningClient {
        fn respond(
            &self,
            input: ModelRequest,
            events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            self.captured_recent_steps
                .borrow_mut()
                .push(input.recent_steps.clone());
            let n = self.captured_recent_steps.borrow().len() - 1;
            events.on_reasoning_delta(&format!("thinking through step {n}"));
            let action = if n == 0 {
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
            Ok((
                ModelResponse {
                    message: format!("assistant message {n}"),
                    action,
                },
                None,
            ))
        }
    }

    #[test]
    fn run_with_client_replays_recent_reasoning_into_next_request() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let client = ScriptedReasoningClient {
            captured_recent_steps: RefCell::new(Vec::new()),
        };

        let _ = agent.run_with_client(
            context,
            AgentLoopOptions {
                steps: 2,
                emit_progress: false,
                ..AgentLoopOptions::default()
            },
            &client,
        );

        let captured = client.captured_recent_steps.borrow();
        assert_eq!(captured.len(), 2);
        assert!(captured[0].is_empty());
        assert_eq!(captured[1].len(), 1);
        assert!(captured[1][0].contains("reasoning: thinking through step 0"));
        assert!(captured[1][0].contains("assistant: assistant message 0"));
    }

    struct CapturingRunEvents {
        entries: Rc<RefCell<Vec<String>>>,
    }

    impl AgentRunEvents for CapturingRunEvents {
        fn on_tool_call(&mut self, tool_name: &str, _input: &BTreeMap<String, String>) {
            self.entries.borrow_mut().push(format!("call:{tool_name}"));
        }

        fn on_permission_request(
            &mut self,
            tool_name: &str,
            _input: &BTreeMap<String, String>,
            kind: &str,
            target: &str,
        ) {
            self.entries
                .borrow_mut()
                .push(format!("permission:{tool_name}:{kind}:{target}"));
        }

        fn on_tool_result(&mut self, event: &ToolEvent) {
            self.entries.borrow_mut().push(format!(
                "result:{}:{}",
                event.tool_name,
                match event.status {
                    crate::model::protocol::ObservationStatus::Ok => "ok",
                    crate::model::protocol::ObservationStatus::Failed => "failed",
                }
            ));
        }
    }

    struct CountingCancelCheck {
        calls: usize,
        cancel_after: usize,
    }

    impl AgentCancelCheck for CountingCancelCheck {
        fn is_cancelled(&mut self) -> AppResult<bool> {
            self.calls += 1;
            Ok(self.calls >= self.cancel_after)
        }
    }

    #[test]
    fn run_with_client_stops_when_cancel_check_trips() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let client = ScriptedReplyClient {
            replies: RefCell::new(vec!["step one".to_string(), "step two".to_string()]),
            captured_recent_steps: RefCell::new(Vec::new()),
        };
        let cancel_check: SharedAgentCancelCheck = Rc::new(RefCell::new(CountingCancelCheck {
            calls: 0,
            cancel_after: 2,
        }));

        let error = agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 3,
                    emit_progress: false,
                    cancel_check: Some(cancel_check),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap_err();

        assert!(error.to_string().contains("agent run cancelled"));
        assert_eq!(client.captured_recent_steps.borrow().len(), 1);
    }

    #[test]
    fn run_with_client_emits_live_tool_call_and_result_events() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let todos = Rc::new(RefCell::new(TodoList::default()));
        let entries = Rc::new(RefCell::new(Vec::new()));
        let sink: SharedAgentRunEvents = Rc::new(RefCell::new(CapturingRunEvents {
            entries: entries.clone(),
        }));
        let client = ScriptedReplyClient {
            replies: RefCell::new(vec!["step ONE: looking at files".to_string()]),
            captured_recent_steps: RefCell::new(Vec::new()),
        };

        agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 1,
                    initial_observations: Vec::new(),
                    todos,
                    emit_progress: false,
                    run_events: Some(sink),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();

        assert_eq!(
            entries.borrow().as_slice(),
            ["call:list_files", "result:list_files:ok"]
        );
    }

    struct FixedApprovalResolver {
        decision: AgentApprovalDecision,
        seen: Rc<RefCell<Vec<AgentApprovalRequest>>>,
    }

    impl AgentApprovalResolver for FixedApprovalResolver {
        fn resolve(
            &mut self,
            request: &AgentApprovalRequest,
        ) -> crate::error::AppResult<AgentApprovalDecision> {
            self.seen.borrow_mut().push(request.clone());
            Ok(self.decision)
        }
    }

    #[test]
    fn run_with_client_uses_approval_resolver_for_permissioned_tools() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let resolver: SharedAgentApprovalResolver = Rc::new(RefCell::new(FixedApprovalResolver {
            decision: AgentApprovalDecision::Approved,
            seen: seen.clone(),
        }));
        let client = ScriptedActionsClient {
            captured_observations: RefCell::new(Vec::new()),
            actions: vec![
                ModelAction::CallTool {
                    tool_name: "run_shell".to_string(),
                    input: ToolInput::new()
                        .with_arg("command", "pwd")
                        .with_arg("cwd", "."),
                },
                ModelAction::Finish,
            ],
            calls: RefCell::new(0),
        };

        let result = agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 2,
                    initial_observations: Vec::new(),
                    todos: Rc::new(RefCell::new(TodoList::default())),
                    emit_progress: false,
                    approval_resolver: Some(resolver),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();

        assert_eq!(seen.borrow().len(), 1);
        assert_eq!(seen.borrow()[0].kind, "shell");
        assert_eq!(seen.borrow()[0].target, "pwd");
        assert_eq!(result.tool_events.len(), 1);
        assert!(result.tool_events[0].output.contains("exit_code: 0"));
        assert_eq!(
            result.tool_events[0].status,
            crate::model::protocol::ObservationStatus::Ok
        );
    }

    #[test]
    fn run_with_client_stops_permissioned_tool_when_resolver_denies() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let resolver: SharedAgentApprovalResolver = Rc::new(RefCell::new(FixedApprovalResolver {
            decision: AgentApprovalDecision::Denied,
            seen: seen.clone(),
        }));
        let client = ScriptedActionsClient {
            captured_observations: RefCell::new(Vec::new()),
            actions: vec![
                ModelAction::CallTool {
                    tool_name: "run_shell".to_string(),
                    input: ToolInput::new()
                        .with_arg("command", "pwd")
                        .with_arg("cwd", "."),
                },
                ModelAction::Finish,
            ],
            calls: RefCell::new(0),
        };

        let result = agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 2,
                    initial_observations: Vec::new(),
                    todos: Rc::new(RefCell::new(TodoList::default())),
                    emit_progress: false,
                    approval_resolver: Some(resolver),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();

        assert_eq!(seen.borrow().len(), 1);
        assert_eq!(result.tool_events.len(), 1);
        assert_eq!(
            result.tool_events[0].status,
            crate::model::protocol::ObservationStatus::Failed
        );
        assert!(result.tool_events[0]
            .output
            .contains("permission denied for shell: pwd"));
        assert!(!result.tool_events[0].output.contains("exit_code: 0"));
    }

    /// Phase 10c-2: scripted client emits N identical list_files calls in a row.
    /// Used to verify repeat-call detection windowing.
    struct RepeatScriptedClient {
        max_calls: usize,
        calls: RefCell<usize>,
    }

    impl ModelClient for RepeatScriptedClient {
        fn respond(
            &self,
            _input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            let n = *self.calls.borrow();
            *self.calls.borrow_mut() = n + 1;
            let action = if n < self.max_calls {
                let mut tin = ToolInput::new();
                tin.args.insert("root".to_string(), "/empty".to_string());
                tin.args.insert("max_depth".to_string(), "1".to_string());
                tin.args.insert("limit".to_string(), "5".to_string());
                ModelAction::CallTool {
                    tool_name: "list_files".to_string(),
                    input: tin,
                }
            } else {
                ModelAction::Finish
            };
            Ok((
                ModelResponse {
                    message: format!("step {n}"),
                    action,
                },
                None,
            ))
        }
    }

    #[test]
    fn repeat_detection_first_call_passes_through_clean() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let client = RepeatScriptedClient {
            max_calls: 1,
            calls: RefCell::new(0),
        };
        let result = agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 2,
                    initial_observations: Vec::new(),
                    todos: Rc::new(RefCell::new(TodoList::default())),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();
        assert_eq!(result.tool_events.len(), 1);
        assert!(
            !result.tool_events[0].output.contains("stuck-warning"),
            "first call must NOT have stuck-warning"
        );
        // It's an OK status (list_files ran, even if /empty doesn't exist — registry returns
        // ToolFailure or empty listing depending on platform).
    }

    #[test]
    fn repeat_detection_second_identical_call_does_not_short_circuit() {
        // 2nd identical call should NOT trigger the short-circuit (only the 3rd does).
        // The stuck-warning is now injected as a separate Observation rather than
        // appended to output.summary (codex review: warning was being eaten by
        // head_trim / Todos summarize when buried in the tail).
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let client = RepeatScriptedClient {
            max_calls: 2,
            calls: RefCell::new(0),
        };
        let result = agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 3,
                    initial_observations: Vec::new(),
                    todos: Rc::new(RefCell::new(TodoList::default())),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();
        assert_eq!(result.tool_events.len(), 2, "expected 2 tool events");
        let second = &result.tool_events[1].output;
        assert!(
            !second.contains("repeated identical tool call detected"),
            "2nd call must NOT short-circuit (only the 3rd does); output: {second}"
        );
    }

    /// Mock client that captures the `observations` field of every ModelRequest it sees.
    /// Used to verify side effects on the observation stream (e.g., stuck-warning).
    struct ObservationCapturingClient {
        captured_observations: RefCell<Vec<Vec<crate::model::protocol::Observation>>>,
        max_calls: usize,
        calls: RefCell<usize>,
    }

    impl ModelClient for ObservationCapturingClient {
        fn respond(
            &self,
            input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            self.captured_observations
                .borrow_mut()
                .push(input.observations.clone());
            let n = *self.calls.borrow();
            *self.calls.borrow_mut() = n + 1;
            let action = if n < self.max_calls {
                let mut tin = ToolInput::new();
                tin.args.insert("root".to_string(), "/empty".to_string());
                tin.args.insert("max_depth".to_string(), "1".to_string());
                tin.args.insert("limit".to_string(), "5".to_string());
                ModelAction::CallTool {
                    tool_name: "list_files".to_string(),
                    input: tin,
                }
            } else {
                ModelAction::Finish
            };
            Ok((
                ModelResponse {
                    message: format!("step {n}"),
                    action,
                },
                None,
            ))
        }
    }

    #[test]
    fn repeat_detection_emits_stuck_warning_observation_on_second_identical_call() {
        // After 2nd identical call, the next ModelRequest must include a stuck-warning
        // Observation in its observations field (not buried in the tool's summary).
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let client = ObservationCapturingClient {
            captured_observations: RefCell::new(Vec::new()),
            max_calls: 3,
            calls: RefCell::new(0),
        };
        let _ = agent.run_with_client(
            context,
            AgentLoopOptions {
                steps: 4,
                initial_observations: Vec::new(),
                todos: Rc::new(RefCell::new(TodoList::default())),
                ..AgentLoopOptions::default()
            },
            &client,
        );
        let captures = client.captured_observations.borrow();
        // After step 1 (1st list_files), step 2 sees observations including the result —
        // no warning yet. After step 2 (2nd identical), step 3's request observations
        // should include the stuck-warning entry (tool_name == "stuck-warning").
        let step3_obs = captures
            .get(2)
            .expect("at least 3 model calls (step 1 + 2 + 3 setup)");
        let has_warning = step3_obs
            .iter()
            .any(|o| o.tool_name == "stuck-warning" && o.summary.contains("stuck-warning"));
        assert!(
            has_warning,
            "step 3 request should see a stuck-warning Observation: {:?}",
            step3_obs
                .iter()
                .map(|o| (&o.tool_name, &o.summary))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn repeat_detection_third_identical_call_short_circuits_as_failure() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let client = RepeatScriptedClient {
            max_calls: 5, // emit identical calls forever; loop budget will end it
            calls: RefCell::new(0),
        };
        let result = agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 4,
                    initial_observations: Vec::new(),
                    todos: Rc::new(RefCell::new(TodoList::default())),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();
        // Step 1: list_files. Step 2: list_files (warning). Step 3: list_files (short-circuit).
        // Step 4: list_files (short-circuit).
        assert!(result.tool_events.len() >= 3, "expected ≥3 tool events");
        let third = &result.tool_events[2].output;
        assert!(
            third.contains("repeated identical tool call detected"),
            "3rd call must short-circuit: {third}"
        );
        assert!(matches!(
            result.tool_events[2].status,
            crate::model::protocol::ObservationStatus::Failed
        ));
    }

    struct ScriptedActionsClient {
        captured_observations: RefCell<Vec<Vec<crate::model::protocol::Observation>>>,
        actions: Vec<ModelAction>,
        calls: RefCell<usize>,
    }

    impl ModelClient for ScriptedActionsClient {
        fn respond(
            &self,
            input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            self.captured_observations
                .borrow_mut()
                .push(input.observations.clone());
            let index = *self.calls.borrow();
            *self.calls.borrow_mut() = index + 1;
            let action = self
                .actions
                .get(index)
                .cloned()
                .unwrap_or(ModelAction::Finish);
            Ok((
                ModelResponse {
                    message: format!("scripted step {index}"),
                    action,
                },
                None,
            ))
        }
    }

    #[test]
    fn run_with_client_cancels_in_flight_shell_tool() {
        let mut cfg = crate::config::types::AppConfig::default();
        cfg.approval.require_shell_confirmation = false;
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("run cancellable shell".to_string(), None);
        let client = ScriptedActionsClient {
            captured_observations: RefCell::new(Vec::new()),
            actions: vec![ModelAction::CallTool {
                tool_name: "run_shell".to_string(),
                input: ToolInput::new()
                    .with_arg("command", "tail -f /dev/null")
                    .with_arg("cwd", "."),
            }],
            calls: RefCell::new(0),
        };
        let cancel_check: SharedAgentCancelCheck = Rc::new(RefCell::new(CountingCancelCheck {
            calls: 0,
            cancel_after: 4,
        }));

        let started = Instant::now();
        let error = agent
            .run_with_client(
                context,
                AgentLoopOptions {
                    steps: 1,
                    emit_progress: false,
                    cancel_check: Some(cancel_check),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap_err();

        assert!(error.to_string().contains("agent run cancelled"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "agent loop should abort the shell process promptly"
        );
        assert_eq!(*client.calls.borrow(), 1);
    }

    #[test]
    fn run_with_client_injects_recovery_hint_after_search_text_returns_no_matches() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("find a definitely missing symbol".to_string(), None);
        let dir = unique_tmp("empty_search");
        fs::create_dir_all(&dir).unwrap();
        let client = ScriptedActionsClient {
            captured_observations: RefCell::new(Vec::new()),
            actions: vec![
                ModelAction::CallTool {
                    tool_name: "search_text".to_string(),
                    input: ToolInput::new()
                        .with_arg("root", dir.to_string_lossy().to_string())
                        .with_arg("query", "missing_symbol_that_should_not_exist")
                        .with_arg("limit", "5"),
                },
                ModelAction::Finish,
            ],
            calls: RefCell::new(0),
        };

        let _ = agent.run_with_client(
            context,
            AgentLoopOptions {
                steps: 2,
                initial_observations: Vec::new(),
                todos: Rc::new(RefCell::new(TodoList::default())),
                ..AgentLoopOptions::default()
            },
            &client,
        );

        let captures = client.captured_observations.borrow();
        let step2_obs = captures.get(1).expect("expected second model request");
        let has_hint = step2_obs.iter().any(|observation| {
            observation.tool_name == "recovery_hint"
                && observation.summary.contains("after=search_text")
                && observation.summary.contains("next=list_files")
        });
        assert!(has_hint, "expected recovery_hint after empty search result");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn run_with_client_injects_recovery_hint_after_failed_read_file() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("inspect a missing file".to_string(), None);
        let client = ScriptedActionsClient {
            captured_observations: RefCell::new(Vec::new()),
            actions: vec![
                ModelAction::CallTool {
                    tool_name: "read_file".to_string(),
                    input: ToolInput::new().with_arg("path", "definitely-missing-file.rs"),
                },
                ModelAction::Finish,
            ],
            calls: RefCell::new(0),
        };

        let _ = agent.run_with_client(
            context,
            AgentLoopOptions {
                steps: 2,
                initial_observations: Vec::new(),
                todos: Rc::new(RefCell::new(TodoList::default())),
                ..AgentLoopOptions::default()
            },
            &client,
        );

        let captures = client.captured_observations.borrow();
        let step2_obs = captures.get(1).expect("expected second model request");
        let has_hint = step2_obs.iter().any(|observation| {
            observation.tool_name == "recovery_hint"
                && observation.summary.contains("after=read_file")
                && observation.summary.contains("next=search_text")
        });
        assert!(has_hint, "expected recovery_hint after failed read_file");
    }

    #[test]
    fn run_with_client_records_raw_tool_output_for_benchmark_and_dogfood() {
        // Regression guard: ToolEvent.output should keep the raw tool body for
        // benchmark and dogfood assertions, even though observations still use
        // summarize_for_kind(...) for prompt compaction.
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let context = TaskContext::new("dummy".to_string(), None);
        let todos = Rc::new(RefCell::new(TodoList::default()));
        let options = AgentLoopOptions {
            steps: 2,
            initial_observations: Vec::new(),
            todos: todos.clone(),
            ..AgentLoopOptions::default()
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

        // The ToolEvent.output must keep the raw todo_write body:
        assert_eq!(result.tool_events.len(), 1);
        let observed = &result.tool_events[0].output;
        assert_eq!(
            observed.lines().count(),
            4,
            "raw output expected: {observed}"
        );
        assert!(observed.starts_with("3 todos"), "observed: {observed}");
        assert!(
            observed.contains("[in_progress]  Bing"),
            "observed: {observed}"
        );
    }

    struct TodoCapturingClient {
        captured_todos: RefCell<Vec<Vec<crate::core::todos::Todo>>>,
    }

    impl ModelClient for TodoCapturingClient {
        fn respond(
            &self,
            input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            self.captured_todos.borrow_mut().push(input.todos.clone());
            Ok((
                ModelResponse {
                    message: "done".to_string(),
                    action: ModelAction::Finish,
                },
                None,
            ))
        }
    }

    fn unique_tmp(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("dscode_loop_runtime_skill_test_{label}_{nanos}"))
    }

    #[test]
    fn run_with_client_seeds_skill_initial_todos_into_first_request() {
        let dir = unique_tmp("skill_seed");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("seeded.toml"),
            r#"
name = "seeded"
description = "seed test"
allowed_tools = ["todo_write", "list_files"]
triggers = ["seed"]

[[initial_todos]]
content = "Inspect the repo"
active_form = "Inspecting the repo"
status = "in_progress"

[[initial_todos]]
content = "Summarize findings"
active_form = "Summarizing findings"
status = "pending"

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
"#,
        )
        .unwrap();

        let mut cfg = crate::config::types::AppConfig::default();
        cfg.workspace.user_skills_dir = dir.to_string_lossy().to_string();
        let agent = AgentLoop::new(cfg);
        let client = TodoCapturingClient {
            captured_todos: RefCell::new(Vec::new()),
        };

        let _ = agent.run_with_client(
            TaskContext::new("seed todos".to_string(), Some("seeded".to_string())),
            AgentLoopOptions {
                steps: 1,
                initial_observations: Vec::new(),
                todos: Rc::new(RefCell::new(TodoList::default())),
                ..AgentLoopOptions::default()
            },
            &client,
        );

        let captured = client.captured_todos.borrow();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].len(), 2);
        assert_eq!(captured[0][0].content, "Inspect the repo");
        assert_eq!(captured[0][0].status, TodoStatus::InProgress);
        assert_eq!(captured[0][1].content, "Summarize findings");
        assert_eq!(captured[0][1].status, TodoStatus::Pending);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn run_with_client_auto_selects_skill_from_triggers() {
        let dir = unique_tmp("skill_auto");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("write-tests.toml"),
            r#"
name = "write-tests"
description = "auto select test"
allowed_tools = ["todo_write", "list_files"]
triggers = ["write tests", "coverage"]

[[initial_todos]]
content = "Write the first failing test"
active_form = "Writing the first failing test"
status = "in_progress"

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
"#,
        )
        .unwrap();

        let mut cfg = crate::config::types::AppConfig::default();
        cfg.workspace.user_skills_dir = dir.to_string_lossy().to_string();
        let agent = AgentLoop::new(cfg);
        let client = TodoCapturingClient {
            captured_todos: RefCell::new(Vec::new()),
        };

        let _ = agent.run_with_client(
            TaskContext::new("please write tests for the parser".to_string(), None),
            AgentLoopOptions {
                steps: 1,
                initial_observations: Vec::new(),
                todos: Rc::new(RefCell::new(TodoList::default())),
                ..AgentLoopOptions::default()
            },
            &client,
        );

        let captured = client.captured_todos.borrow();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].len(), 1);
        assert_eq!(captured[0][0].content, "Write the first failing test");
        assert_eq!(captured[0][0].status, TodoStatus::InProgress);

        let _ = fs::remove_dir_all(dir);
    }

    struct DispatchingClient {
        calls: RefCell<usize>,
    }

    impl ModelClient for DispatchingClient {
        fn respond(
            &self,
            _input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            let n = *self.calls.borrow();
            *self.calls.borrow_mut() = n + 1;
            let action = if n == 0 {
                ModelAction::CallTool {
                    tool_name: "dispatch_subagent".to_string(),
                    input: ToolInput::new()
                        .with_arg("task", "inspect repository layout")
                        .with_arg("steps", "2"),
                }
            } else {
                ModelAction::Finish
            };
            Ok((
                ModelResponse {
                    message: format!("dispatch step {n}"),
                    action,
                },
                None,
            ))
        }
    }

    struct HookBlockingClient {
        calls: RefCell<usize>,
    }

    impl ModelClient for HookBlockingClient {
        fn respond(
            &self,
            _input: ModelRequest,
            _events: &mut dyn StreamEvents,
        ) -> crate::error::AppResult<(ModelResponse, Option<TokenUsage>)> {
            let n = *self.calls.borrow();
            *self.calls.borrow_mut() = n + 1;
            let action = if n == 0 {
                ModelAction::CallTool {
                    tool_name: "list_files".to_string(),
                    input: ToolInput::new()
                        .with_arg("root", ".")
                        .with_arg("max_depth", "1"),
                }
            } else {
                ModelAction::Finish
            };
            Ok((
                ModelResponse {
                    message: format!("hook step {n}"),
                    action,
                },
                None,
            ))
        }
    }

    #[test]
    #[cfg(unix)]
    fn run_with_client_blocks_tool_when_pre_tool_hook_denies() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_tmp("pre_tool_hook");
        let hook_dir = root.join("hooks/pre_tool_use");
        fs::create_dir_all(&hook_dir).unwrap();
        let hook_path = hook_dir.join("10-block");
        fs::write(
            &hook_path,
            "#!/bin/sh\nprintf 'blocked by pre hook' >&2\nexit 7\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&hook_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook_path, permissions).unwrap();

        let mut cfg = crate::config::types::AppConfig::default();
        cfg.hooks.enabled = true;
        cfg.hooks.project_dir = root.join("hooks").display().to_string();
        let agent = AgentLoop::new(cfg);
        let client = HookBlockingClient {
            calls: RefCell::new(0),
        };

        let result = agent
            .run_with_client(
                TaskContext::new("inspect with hook".to_string(), None),
                AgentLoopOptions {
                    steps: 2,
                    emit_progress: false,
                    persist_session: false,
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();

        assert_eq!(result.tool_events.len(), 1);
        let event = &result.tool_events[0];
        assert_eq!(event.tool_name, "list_files");
        assert_eq!(
            event.status,
            crate::model::protocol::ObservationStatus::Failed
        );
        assert!(event.output.contains("blocked by pre hook"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_with_client_executes_dispatch_subagent_with_isolated_child_loop() {
        let cfg = crate::config::types::AppConfig::default();
        let agent = AgentLoop::new(cfg);
        let client = DispatchingClient {
            calls: RefCell::new(0),
        };
        let todos = Rc::new(RefCell::new(TodoList::default()));
        todos.borrow_mut().replace(vec![
            crate::core::todos::Todo {
                content: "Inspect repository layout".to_string(),
                active_form: "Inspecting repository layout".to_string(),
                status: TodoStatus::InProgress,
            },
            crate::core::todos::Todo {
                content: "Implement the requested changes".to_string(),
                active_form: "Implementing the requested changes".to_string(),
                status: TodoStatus::Pending,
            },
        ]);

        let result = agent
            .run_with_client(
                TaskContext::new("delegate repository inspection".to_string(), None),
                AgentLoopOptions {
                    steps: 2,
                    initial_observations: Vec::new(),
                    todos: todos.clone(),
                    ..AgentLoopOptions::default()
                },
                &client,
            )
            .unwrap();

        assert_eq!(result.tool_events.len(), 1);
        let event = &result.tool_events[0];
        assert_eq!(event.tool_name, "dispatch_subagent");
        assert!(event.output.contains("subagent finished task"));
        assert!(event.output.contains("child tool calls:"));
        assert!(event.output.contains("parent todos auto-advanced"));

        let todos = todos.borrow();
        assert_eq!(todos.items[0].status, TodoStatus::Completed);
        assert_eq!(todos.items[1].status, TodoStatus::InProgress);
    }
}
