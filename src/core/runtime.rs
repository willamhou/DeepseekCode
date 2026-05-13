use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{app_error, AppResult};
use crate::util::json::{
    json_as_array, json_as_string, json_as_u64, json_value_to_string, parse_root_object, JsonValue,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub title: String,
    pub workspace: String,
    pub status: String,
    pub active_thread_id: Option<String>,
    pub thread_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub title: String,
    pub workspace: String,
    pub model: String,
    pub mode: String,
    pub status: String,
    pub latest_turn_id: Option<String>,
    pub event_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRecord {
    pub id: String,
    pub thread_id: String,
    pub index: u64,
    pub role: String,
    pub content: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRecord {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub index: u64,
    pub item_type: String,
    pub role: Option<String>,
    pub content: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub model: String,
    pub source: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub prompt_cache_hit_tokens: u64,
    pub prompt_cache_miss_tokens: u64,
    pub estimated_input_cost_microusd: Option<u64>,
    pub estimated_output_cost_microusd: Option<u64>,
    pub estimated_total_cost_microusd: Option<u64>,
    pub pricing_source: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub kind: String,
    pub status: String,
    pub summary: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub name: String,
    pub status: String,
    pub schedule: String,
    pub prompt: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeEvent {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub seq: u64,
    pub kind: String,
    pub created_at: String,
    pub payload: JsonValue,
}

#[derive(Debug, Clone)]
pub struct ThreadCompactionRecord {
    pub thread_id: String,
    pub keep_tail_turns: usize,
    pub summarized_turn_count: usize,
    pub kept_turn_count: usize,
    pub summarized_turn_ids: Vec<String>,
    pub kept_turn_ids: Vec<String>,
    pub summary_source: String,
    pub summary_turn: TurnRecord,
    pub summary_item: ItemRecord,
    pub event: RuntimeEvent,
}

#[derive(Debug, Clone)]
pub struct ThreadForkRecord {
    pub source_thread_id: String,
    pub thread: ThreadRecord,
    pub copied_turn_count: usize,
    pub copied_item_count: usize,
    pub event: RuntimeEvent,
}

#[derive(Debug, Clone)]
pub struct RuntimeStore {
    root: PathBuf,
}

fn runtime_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn write_json_file(path: PathBuf, content: String) -> AppResult<()> {
    let mut temp_path = path.clone();
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    temp_path.set_extension(format!("json.tmp-{}-{suffix}", std::process::id()));
    fs::write(&temp_path, content)?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&temp_path, &path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        error
    })?;
    Ok(())
}

impl RuntimeStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create_session(&self, title: String, workspace: String) -> AppResult<SessionRecord> {
        self.ensure_dirs()?;
        let now = epoch_label();
        let session = SessionRecord {
            id: new_id("session"),
            created_at: now.clone(),
            updated_at: now,
            title,
            workspace,
            status: "active".to_string(),
            active_thread_id: None,
            thread_count: 0,
        };
        self.write_session(&session)?;
        Ok(session)
    }

    pub fn list_sessions(&self, limit: usize) -> AppResult<Vec<SessionRecord>> {
        let dir = self.sessions_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            records.push(parse_session_record(&parse_root_object(&content)?)?);
        }
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        records.truncate(limit);
        Ok(records)
    }

    pub fn load_session(&self, id: &str) -> AppResult<SessionRecord> {
        validate_record_id(id)?;
        let path = self.session_path(id);
        if !path.exists() {
            return Err(app_error(format!("runtime session not found: {id}")));
        }
        let content = fs::read_to_string(path)?;
        parse_session_record(&parse_root_object(&content)?)
    }

    pub fn rename_session(&self, id: &str, title: String) -> AppResult<SessionRecord> {
        let mut session = self.load_session(id)?;
        session.title = title;
        session.updated_at = epoch_label();
        self.write_session(&session)?;
        Ok(session)
    }

    pub fn create_thread(
        &self,
        title: String,
        workspace: String,
        model: String,
        mode: String,
    ) -> AppResult<ThreadRecord> {
        self.create_thread_with_session(None, title, workspace, model, mode)
    }

    pub fn create_thread_for_session(
        &self,
        session_id: &str,
        title: String,
        workspace: String,
        model: String,
        mode: String,
    ) -> AppResult<ThreadRecord> {
        self.load_session(session_id)?;
        let thread =
            self.create_thread_with_session(Some(session_id), title, workspace, model, mode)?;
        let mut session = self.load_session(session_id)?;
        session.active_thread_id = Some(thread.id.clone());
        session.thread_count += 1;
        session.updated_at = thread.updated_at.clone();
        self.write_session(&session)?;
        Ok(thread)
    }

    fn create_thread_with_session(
        &self,
        session_id: Option<&str>,
        title: String,
        workspace: String,
        model: String,
        mode: String,
    ) -> AppResult<ThreadRecord> {
        self.ensure_dirs()?;
        if let Some(session_id) = session_id {
            validate_record_id(session_id)?;
        }
        let now = epoch_label();
        let mut thread = ThreadRecord {
            id: new_id("thread"),
            session_id: session_id.map(str::to_string),
            created_at: now.clone(),
            updated_at: now.clone(),
            title,
            workspace,
            model,
            mode,
            status: "active".to_string(),
            latest_turn_id: None,
            event_seq: 0,
        };
        self.write_thread(&thread)?;
        let event = self.append_event(
            &thread.id,
            None,
            "thread_created",
            JsonValue::Object(object([
                ("title", JsonValue::String(thread.title.clone())),
                ("mode", JsonValue::String(thread.mode.clone())),
                (
                    "session_id",
                    thread
                        .session_id
                        .clone()
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::Null),
                ),
            ])),
        )?;
        thread.event_seq = event.seq;
        thread.updated_at = event.created_at;
        self.write_thread(&thread)?;
        Ok(thread)
    }

    pub fn fork_thread(
        &self,
        source_thread_id: &str,
        title: Option<String>,
    ) -> AppResult<ThreadForkRecord> {
        validate_record_id(source_thread_id)?;
        self.ensure_dirs()?;
        let source = self.load_thread(source_thread_id)?;
        let source_turns = self.list_turns(source_thread_id)?;
        let source_items = self.list_items(source_thread_id, None)?;
        let now = epoch_label();
        let title = title
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("Fork: {}", source.title));
        let mut thread = ThreadRecord {
            id: new_id("thread"),
            session_id: source.session_id.clone(),
            created_at: now.clone(),
            updated_at: now,
            title,
            workspace: source.workspace.clone(),
            model: source.model.clone(),
            mode: source.mode.clone(),
            status: "active".to_string(),
            latest_turn_id: None,
            event_seq: 0,
        };
        self.write_thread(&thread)?;
        let created = self.append_event(
            &thread.id,
            None,
            "thread_created",
            JsonValue::Object(object([
                ("title", JsonValue::String(thread.title.clone())),
                ("mode", JsonValue::String(thread.mode.clone())),
                (
                    "session_id",
                    thread
                        .session_id
                        .clone()
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::Null),
                ),
                (
                    "source_thread_id",
                    JsonValue::String(source_thread_id.to_string()),
                ),
            ])),
        )?;
        thread.event_seq = created.seq;
        thread.updated_at = created.created_at;
        self.write_thread(&thread)?;

        let mut turn_id_map = BTreeMap::new();
        for source_turn in &source_turns {
            let turn = TurnRecord {
                id: new_id("turn"),
                thread_id: thread.id.clone(),
                index: source_turn.index,
                role: source_turn.role.clone(),
                content: source_turn.content.clone(),
                status: source_turn.status.clone(),
                created_at: source_turn.created_at.clone(),
            };
            turn_id_map.insert(source_turn.id.clone(), turn.id.clone());
            self.write_turn(&turn)?;
        }

        for source_item in &source_items {
            let item = ItemRecord {
                id: new_id("item"),
                thread_id: thread.id.clone(),
                turn_id: source_item
                    .turn_id
                    .as_ref()
                    .and_then(|turn_id| turn_id_map.get(turn_id).cloned()),
                index: source_item.index,
                item_type: source_item.item_type.clone(),
                role: source_item.role.clone(),
                content: source_item.content.clone(),
                status: source_item.status.clone(),
                created_at: source_item.created_at.clone(),
            };
            self.write_item(&item)?;
        }

        thread.latest_turn_id = source
            .latest_turn_id
            .as_ref()
            .and_then(|turn_id| turn_id_map.get(turn_id).cloned());
        let event = self.append_event(
            &thread.id,
            thread.latest_turn_id.as_deref(),
            "thread_forked",
            JsonValue::Object(object([
                (
                    "source_thread_id",
                    JsonValue::String(source_thread_id.to_string()),
                ),
                (
                    "source_session_id",
                    source
                        .session_id
                        .clone()
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::Null),
                ),
                (
                    "copied_turn_count",
                    JsonValue::Number(source_turns.len().to_string()),
                ),
                (
                    "copied_item_count",
                    JsonValue::Number(source_items.len().to_string()),
                ),
            ])),
        )?;
        thread.event_seq = event.seq;
        thread.updated_at = event.created_at.clone();
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.thread_count += 1;
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(ThreadForkRecord {
            source_thread_id: source_thread_id.to_string(),
            thread,
            copied_turn_count: source_turns.len(),
            copied_item_count: source_items.len(),
            event,
        })
    }

    pub fn list_threads(&self, limit: usize) -> AppResult<Vec<ThreadRecord>> {
        let dir = self.threads_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            records.push(parse_thread_record(&parse_root_object(&content)?)?);
        }
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        records.truncate(limit);
        Ok(records)
    }

    pub fn list_session_threads(
        &self,
        session_id: &str,
        limit: usize,
    ) -> AppResult<Vec<ThreadRecord>> {
        validate_record_id(session_id)?;
        let mut records = self
            .list_threads(usize::MAX)?
            .into_iter()
            .filter(|thread| thread.session_id.as_deref() == Some(session_id))
            .collect::<Vec<_>>();
        records.truncate(limit);
        Ok(records)
    }

    pub fn load_thread(&self, id: &str) -> AppResult<ThreadRecord> {
        validate_record_id(id)?;
        let path = self.thread_path(id);
        if !path.exists() {
            return Err(app_error(format!("runtime thread not found: {id}")));
        }
        let content = fs::read_to_string(path)?;
        parse_thread_record(&parse_root_object(&content)?)
    }

    pub fn append_turn(
        &self,
        thread_id: &str,
        role: String,
        content: String,
    ) -> AppResult<TurnRecord> {
        validate_record_id(thread_id)?;
        validate_turn_role(&role)?;
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        let index = self.list_turns(thread_id)?.len() as u64 + 1;
        let now = epoch_label();
        let turn = TurnRecord {
            id: new_id("turn"),
            thread_id: thread_id.to_string(),
            index,
            role,
            content,
            status: "completed".to_string(),
            created_at: now,
        };
        self.write_turn(&turn)?;
        let event = self.append_event(
            thread_id,
            Some(&turn.id),
            "turn_recorded",
            JsonValue::Object(object([
                ("turn_id", JsonValue::String(turn.id.clone())),
                ("role", JsonValue::String(turn.role.clone())),
                ("status", JsonValue::String(turn.status.clone())),
            ])),
        )?;
        thread.latest_turn_id = Some(turn.id.clone());
        thread.updated_at = event.created_at;
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(turn)
    }

    pub fn update_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
        content: String,
        status: String,
    ) -> AppResult<TurnRecord> {
        validate_record_id(thread_id)?;
        validate_record_id(turn_id)?;
        validate_record_status(&status)?;
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        let mut turn = self.load_turn(thread_id, turn_id)?;
        turn.content = content;
        turn.status = status;
        self.write_turn(&turn)?;
        let event = self.append_event(
            thread_id,
            Some(turn_id),
            "turn_updated",
            JsonValue::Object(object([
                ("turn_id", JsonValue::String(turn.id.clone())),
                ("role", JsonValue::String(turn.role.clone())),
                ("status", JsonValue::String(turn.status.clone())),
            ])),
        )?;
        thread.latest_turn_id = Some(turn.id.clone());
        thread.updated_at = event.created_at;
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(turn)
    }

    pub fn list_turns(&self, thread_id: &str) -> AppResult<Vec<TurnRecord>> {
        validate_record_id(thread_id)?;
        let dir = self.turns_dir(thread_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            records.push(parse_turn_record(&parse_root_object(&content)?)?);
        }
        records.sort_by_key(|record| record.index);
        Ok(records)
    }

    pub fn load_turn(&self, thread_id: &str, turn_id: &str) -> AppResult<TurnRecord> {
        validate_record_id(thread_id)?;
        validate_record_id(turn_id)?;
        let path = self.turn_path(thread_id, turn_id);
        if !path.exists() {
            return Err(app_error(format!("runtime turn not found: {turn_id}")));
        }
        let content = fs::read_to_string(path)?;
        let turn = parse_turn_record(&parse_root_object(&content)?)?;
        if turn.thread_id != thread_id {
            return Err(app_error(format!(
                "runtime turn `{turn_id}` does not belong to thread `{thread_id}`"
            )));
        }
        Ok(turn)
    }

    pub fn append_item(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        item_type: String,
        role: Option<String>,
        content: String,
        status: String,
    ) -> AppResult<ItemRecord> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        validate_item_type(&item_type)?;
        validate_item_status(&status)?;
        if let Some(role) = role.as_deref() {
            validate_turn_role(role)?;
        }
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        if let Some(turn_id) = turn_id {
            let path = self.turn_path(thread_id, turn_id);
            if !path.exists() {
                return Err(app_error(format!("runtime turn not found: {turn_id}")));
            }
        }
        let index = self.list_items(thread_id, None)?.len() as u64 + 1;
        let item = ItemRecord {
            id: new_id("item"),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.map(str::to_string),
            index,
            item_type,
            role,
            content,
            status,
            created_at: epoch_label(),
        };
        self.write_item(&item)?;
        let event = self.append_event(
            thread_id,
            turn_id,
            "item_recorded",
            JsonValue::Object(object([
                ("item_id", JsonValue::String(item.id.clone())),
                ("item_type", JsonValue::String(item.item_type.clone())),
                (
                    "role",
                    item.role
                        .clone()
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::Null),
                ),
                ("status", JsonValue::String(item.status.clone())),
            ])),
        )?;
        thread.updated_at = event.created_at;
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(item)
    }

    pub fn update_item(
        &self,
        thread_id: &str,
        item_id: &str,
        content: String,
        status: String,
    ) -> AppResult<ItemRecord> {
        validate_record_id(thread_id)?;
        validate_record_id(item_id)?;
        validate_record_status(&status)?;
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        let mut item = self.load_item(thread_id, item_id)?;
        item.content = content;
        item.status = status;
        self.write_item(&item)?;
        let event = self.append_event(
            thread_id,
            item.turn_id.as_deref(),
            "item_updated",
            JsonValue::Object(object([
                ("item_id", JsonValue::String(item.id.clone())),
                ("item_type", JsonValue::String(item.item_type.clone())),
                ("status", JsonValue::String(item.status.clone())),
            ])),
        )?;
        thread.updated_at = event.created_at;
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(item)
    }

    pub fn list_items(&self, thread_id: &str, turn_id: Option<&str>) -> AppResult<Vec<ItemRecord>> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        let dir = self.items_dir(thread_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            let record = parse_item_record(&parse_root_object(&content)?)?;
            if turn_id.is_some_and(|id| record.turn_id.as_deref() != Some(id)) {
                continue;
            }
            records.push(record);
        }
        records.sort_by_key(|record| record.index);
        Ok(records)
    }

    pub fn recent_reasoning_replay_entries(
        &self,
        thread_id: &str,
        limit: usize,
    ) -> AppResult<Vec<String>> {
        self.reasoning_replay_entries_with_pinned_turns(thread_id, limit, &[])
    }

    pub fn reasoning_replay_entries_with_pinned_turns(
        &self,
        thread_id: &str,
        limit: usize,
        pinned_turn_ids: &[String],
    ) -> AppResult<Vec<String>> {
        validate_record_id(thread_id)?;
        let pinned_turn_ids = pinned_turn_ids
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect::<BTreeSet<_>>();
        if limit == 0 && pinned_turn_ids.is_empty() {
            return Ok(Vec::new());
        }
        let records = self
            .list_items(thread_id, None)?
            .into_iter()
            .filter(|item| item.item_type == "reasoning" && !item.content.trim().is_empty())
            .collect::<Vec<_>>();
        let mut selected_ids = BTreeSet::new();
        let mut selected = Vec::new();
        for item in records.iter().rev().take(limit) {
            selected_ids.insert(item.id.clone());
            selected.push(item.clone());
        }
        for item in &records {
            if item
                .turn_id
                .as_deref()
                .is_some_and(|turn_id| pinned_turn_ids.contains(turn_id))
                && selected_ids.insert(item.id.clone())
            {
                selected.push(item.clone());
            }
        }
        selected.sort_by_key(|item| item.index);
        Ok(selected
            .into_iter()
            .map(|item| {
                let turn = item
                    .turn_id
                    .as_deref()
                    .map(|turn_id| format!(" turn={turn_id}"))
                    .unwrap_or_default();
                format!(
                    "persisted reasoning{turn}: {}",
                    compact_excerpt(&item.content, 600)
                )
            })
            .collect::<Vec<_>>())
    }

    pub fn compact_thread(
        &self,
        thread_id: &str,
        keep_tail_turns: usize,
        summary: Option<String>,
    ) -> AppResult<ThreadCompactionRecord> {
        let summary = summary
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let source = summary.as_ref().map(|_| "provided".to_string());
        self.compact_thread_inner(thread_id, keep_tail_turns, summary, source)
    }

    pub fn compact_thread_with_summary_source(
        &self,
        thread_id: &str,
        keep_tail_turns: usize,
        summary: String,
        summary_source: &str,
    ) -> AppResult<ThreadCompactionRecord> {
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err(app_error("runtime compaction summary must not be empty"));
        }
        let summary_source = summary_source.trim();
        if summary_source.is_empty() {
            return Err(app_error(
                "runtime compaction summary_source must not be empty",
            ));
        }
        self.compact_thread_inner(
            thread_id,
            keep_tail_turns,
            Some(summary),
            Some(summary_source.to_string()),
        )
    }

    fn compact_thread_inner(
        &self,
        thread_id: &str,
        keep_tail_turns: usize,
        summary: Option<String>,
        summary_source: Option<String>,
    ) -> AppResult<ThreadCompactionRecord> {
        validate_record_id(thread_id)?;
        self.ensure_dirs()?;
        let thread = self.load_thread(thread_id)?;
        let turns = self.list_turns(thread_id)?;
        if turns.is_empty() {
            return Err(app_error(
                "invalid runtime compaction request: thread has no turns",
            ));
        }
        if keep_tail_turns >= turns.len() {
            return Err(app_error(
                "invalid runtime compaction request: keep_tail_turns leaves no turns to summarize",
            ));
        }
        let split_at = turns.len() - keep_tail_turns;
        let summarized_turns = &turns[..split_at];
        let kept_turns = &turns[split_at..];
        let summarized_turn_ids = summarized_turns
            .iter()
            .map(|turn| turn.id.clone())
            .collect::<Vec<_>>();
        let kept_turn_ids = kept_turns
            .iter()
            .map(|turn| turn.id.clone())
            .collect::<Vec<_>>();
        let (summary_content, summary_source) = match summary {
            Some(summary) => (
                summary,
                summary_source.unwrap_or_else(|| "provided".to_string()),
            ),
            _ => (
                build_compaction_summary(&thread, summarized_turns, kept_turns),
                "extractive".to_string(),
            ),
        };

        let summary_turn =
            self.append_turn(thread_id, "system".to_string(), summary_content.clone())?;
        let summary_item = self.append_item(
            thread_id,
            Some(&summary_turn.id),
            "summary".to_string(),
            Some("system".to_string()),
            summary_content,
            "completed".to_string(),
        )?;
        let event = self.append_event(
            thread_id,
            Some(&summary_turn.id),
            "thread_compacted",
            JsonValue::Object(object([
                ("type", JsonValue::String("thread_compacted".to_string())),
                (
                    "summary_turn_id",
                    JsonValue::String(summary_turn.id.clone()),
                ),
                (
                    "summary_item_id",
                    JsonValue::String(summary_item.id.clone()),
                ),
                (
                    "keep_tail_turns",
                    JsonValue::Number(keep_tail_turns.to_string()),
                ),
                (
                    "summarized_turn_count",
                    JsonValue::Number(summarized_turns.len().to_string()),
                ),
                (
                    "kept_turn_count",
                    JsonValue::Number(kept_turns.len().to_string()),
                ),
                ("summary_source", JsonValue::String(summary_source.clone())),
                ("summarized_turn_ids", string_array(&summarized_turn_ids)),
                ("kept_turn_ids", string_array(&kept_turn_ids)),
            ])),
        )?;
        let mut thread = self.load_thread(thread_id)?;
        thread.latest_turn_id = Some(summary_turn.id.clone());
        thread.updated_at = event.created_at.clone();
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }

        Ok(ThreadCompactionRecord {
            thread_id: thread_id.to_string(),
            keep_tail_turns,
            summarized_turn_count: summarized_turns.len(),
            kept_turn_count: kept_turns.len(),
            summarized_turn_ids,
            kept_turn_ids,
            summary_source,
            summary_turn,
            summary_item,
            event,
        })
    }

    pub fn load_item(&self, thread_id: &str, item_id: &str) -> AppResult<ItemRecord> {
        validate_record_id(thread_id)?;
        validate_record_id(item_id)?;
        let path = self.item_path(thread_id, item_id);
        if !path.exists() {
            return Err(app_error(format!("runtime item not found: {item_id}")));
        }
        let content = fs::read_to_string(path)?;
        parse_item_record(&parse_root_object(&content)?)
    }

    pub fn read_events(&self, thread_id: &str, since_seq: u64) -> AppResult<Vec<RuntimeEvent>> {
        validate_record_id(thread_id)?;
        let path = self.events_path(thread_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        let lines = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let root = match parse_root_object(line) {
                Ok(root) => root,
                Err(_) if index + 1 == lines.len() => break,
                Err(error) => return Err(error),
            };
            let event = parse_runtime_event(&root)?;
            if event.seq > since_seq {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub fn append_thread_event(
        &self,
        thread_id: &str,
        kind: &str,
        payload: JsonValue,
    ) -> AppResult<RuntimeEvent> {
        validate_record_id(thread_id)?;
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        let event = self.append_event(thread_id, None, kind, payload)?;
        thread.event_seq = event.seq;
        thread.updated_at = event.created_at.clone();
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(event)
    }

    pub fn append_permission_request(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        tool: String,
        kind: String,
        target: String,
        input: BTreeMap<String, String>,
    ) -> AppResult<RuntimeEvent> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        if let Some(turn_id) = turn_id {
            let path = self.turn_path(thread_id, turn_id);
            if !path.exists() {
                return Err(app_error(format!("runtime turn not found: {turn_id}")));
            }
        }
        let event = self.append_event(
            thread_id,
            turn_id,
            "permission_request",
            JsonValue::Object(object([
                ("type", JsonValue::String("permission_request".to_string())),
                ("tool", JsonValue::String(tool)),
                ("kind", JsonValue::String(kind)),
                ("target", JsonValue::String(target)),
                ("status", JsonValue::String("pending".to_string())),
                (
                    "input",
                    JsonValue::Object(
                        input
                            .into_iter()
                            .map(|(key, value)| (key, JsonValue::String(value)))
                            .collect(),
                    ),
                ),
            ])),
        )?;
        thread.updated_at = event.created_at.clone();
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(event)
    }

    pub fn append_permission_response(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        request_id: String,
        decision: String,
    ) -> AppResult<RuntimeEvent> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        validate_record_id(&request_id)?;
        if !matches!(decision.as_str(), "approved" | "denied") {
            return Err(app_error(format!(
                "invalid permission response decision `{decision}`"
            )));
        }
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        if let Some(turn_id) = turn_id {
            let path = self.turn_path(thread_id, turn_id);
            if !path.exists() {
                return Err(app_error(format!("runtime turn not found: {turn_id}")));
            }
        }
        let event = self.append_event(
            thread_id,
            turn_id,
            "permission_response",
            JsonValue::Object(object([
                ("type", JsonValue::String("permission_response".to_string())),
                ("request_id", JsonValue::String(request_id)),
                ("decision", JsonValue::String(decision)),
            ])),
        )?;
        thread.updated_at = event.created_at.clone();
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(event)
    }

    pub fn append_user_input_request(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        questions: JsonValue,
    ) -> AppResult<RuntimeEvent> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        validate_user_input_questions(&questions)?;
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        if let Some(turn_id) = turn_id {
            let path = self.turn_path(thread_id, turn_id);
            if !path.exists() {
                return Err(app_error(format!("runtime turn not found: {turn_id}")));
            }
        }
        let event = self.append_event(
            thread_id,
            turn_id,
            "user_input_request",
            JsonValue::Object(object([
                ("type", JsonValue::String("user_input_request".to_string())),
                ("status", JsonValue::String("pending".to_string())),
                ("questions", questions),
            ])),
        )?;
        thread.updated_at = event.created_at.clone();
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(event)
    }

    pub fn append_user_input_response(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        request_id: String,
        answers: BTreeMap<String, String>,
    ) -> AppResult<RuntimeEvent> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        validate_record_id(&request_id)?;
        if answers.is_empty() {
            return Err(app_error("user input response answers must not be empty"));
        }
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        if let Some(turn_id) = turn_id {
            let path = self.turn_path(thread_id, turn_id);
            if !path.exists() {
                return Err(app_error(format!("runtime turn not found: {turn_id}")));
            }
        }
        let event = self.append_event(
            thread_id,
            turn_id,
            "user_input_response",
            JsonValue::Object(object([
                ("type", JsonValue::String("user_input_response".to_string())),
                ("request_id", JsonValue::String(request_id)),
                (
                    "answers",
                    JsonValue::Object(
                        answers
                            .into_iter()
                            .map(|(key, value)| (key, JsonValue::String(value)))
                            .collect(),
                    ),
                ),
            ])),
        )?;
        thread.updated_at = event.created_at.clone();
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(event)
    }

    pub fn append_cancel_request(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        task_id: Option<&str>,
        reason: String,
    ) -> AppResult<RuntimeEvent> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        if let Some(task_id) = task_id {
            validate_record_id(task_id)?;
        }
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        if let Some(turn_id) = turn_id {
            let path = self.turn_path(thread_id, turn_id);
            if !path.exists() {
                return Err(app_error(format!("runtime turn not found: {turn_id}")));
            }
        }
        if let Some(task_id) = task_id {
            let task = self.load_task(task_id)?;
            if task.thread_id.as_deref() != Some(thread_id) {
                return Err(app_error(format!(
                    "runtime task {task_id} is not linked to thread {thread_id}"
                )));
            }
        }
        let mut payload = object([
            ("type", JsonValue::String("cancel_requested".to_string())),
            ("reason", JsonValue::String(reason)),
            ("status", JsonValue::String("pending".to_string())),
        ]);
        if let Some(task_id) = task_id {
            payload.insert(
                "task_id".to_string(),
                JsonValue::String(task_id.to_string()),
            );
        }
        let event = self.append_event(
            thread_id,
            turn_id,
            "cancel_requested",
            JsonValue::Object(payload),
        )?;
        thread.updated_at = event.created_at.clone();
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(event)
    }

    pub fn cancel_task(
        &self,
        task_id: &str,
        reason: String,
    ) -> AppResult<(TaskRecord, Option<RuntimeEvent>)> {
        validate_record_id(task_id)?;
        let reason = if reason.trim().is_empty() {
            "cancelled by user".to_string()
        } else {
            reason
        };
        let current = self.load_task(task_id)?;
        if matches!(current.status.as_str(), "completed" | "failed") {
            return Err(app_error(format!(
                "runtime task {task_id} is `{}` and cannot be cancelled",
                current.status
            )));
        }
        let task = self.update_task(task_id, "cancelled".to_string(), reason.clone())?;
        let event = if let Some(thread_id) = task.thread_id.as_deref() {
            Some(self.append_cancel_request(thread_id, None, Some(&task.id), reason)?)
        } else {
            None
        };
        Ok((task, event))
    }

    pub fn pause_task(
        &self,
        task_id: &str,
        summary_override: Option<String>,
    ) -> AppResult<TaskRecord> {
        validate_record_id(task_id)?;
        let current = self.load_task(task_id)?;
        match current.status.as_str() {
            "pending" => {
                let summary = nonempty_override(summary_override).unwrap_or(current.summary);
                self.update_task(task_id, "paused".to_string(), summary)
            }
            "paused" => Ok(current),
            status => Err(app_error(format!(
                "runtime task {task_id} is `{status}` and cannot be paused"
            ))),
        }
    }

    pub fn resume_task(
        &self,
        task_id: &str,
        summary_override: Option<String>,
    ) -> AppResult<TaskRecord> {
        validate_record_id(task_id)?;
        let current = self.load_task(task_id)?;
        match current.status.as_str() {
            "paused" => {
                let summary = nonempty_override(summary_override).unwrap_or(current.summary);
                self.update_task(task_id, "pending".to_string(), summary)
            }
            "pending" => Ok(current),
            status => Err(app_error(format!(
                "runtime task {task_id} is `{status}` and cannot be resumed"
            ))),
        }
    }

    pub fn append_usage(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        model: String,
        source: String,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) -> AppResult<UsageRecord> {
        self.append_usage_with_cache(
            thread_id,
            turn_id,
            model,
            source,
            prompt_tokens,
            completion_tokens,
            0,
            prompt_tokens,
        )
    }

    pub fn append_usage_with_cache(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        model: String,
        source: String,
        prompt_tokens: u64,
        completion_tokens: u64,
        prompt_cache_hit_tokens: u64,
        prompt_cache_miss_tokens: u64,
    ) -> AppResult<UsageRecord> {
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        self.ensure_dirs()?;
        let mut thread = self.load_thread(thread_id)?;
        if let Some(turn_id) = turn_id {
            let path = self.turn_path(thread_id, turn_id);
            if !path.exists() {
                return Err(app_error(format!("runtime turn not found: {turn_id}")));
            }
        }
        let cost = estimate_deepseek_cost_microusd(
            &model,
            prompt_cache_hit_tokens,
            prompt_cache_miss_tokens,
            completion_tokens,
        );
        let usage = UsageRecord {
            id: new_id("usage"),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.map(str::to_string),
            model,
            source,
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_cache_hit_tokens,
            prompt_cache_miss_tokens,
            estimated_input_cost_microusd: cost.as_ref().map(|cost| cost.input),
            estimated_output_cost_microusd: cost.as_ref().map(|cost| cost.output),
            estimated_total_cost_microusd: cost.as_ref().map(|cost| cost.total),
            pricing_source: cost.map(|cost| cost.source),
            created_at: epoch_label(),
        };
        self.write_usage(&usage)?;
        let event = self.append_event(
            thread_id,
            turn_id,
            "usage_recorded",
            JsonValue::Object(object([
                ("usage_id", JsonValue::String(usage.id.clone())),
                ("model", JsonValue::String(usage.model.clone())),
                ("source", JsonValue::String(usage.source.clone())),
                (
                    "prompt_tokens",
                    JsonValue::Number(usage.prompt_tokens.to_string()),
                ),
                (
                    "completion_tokens",
                    JsonValue::Number(usage.completion_tokens.to_string()),
                ),
                (
                    "total_tokens",
                    JsonValue::Number(usage.total_tokens.to_string()),
                ),
                (
                    "prompt_cache_hit_tokens",
                    JsonValue::Number(usage.prompt_cache_hit_tokens.to_string()),
                ),
                (
                    "prompt_cache_miss_tokens",
                    JsonValue::Number(usage.prompt_cache_miss_tokens.to_string()),
                ),
            ])),
        )?;
        thread.updated_at = event.created_at;
        thread.event_seq = event.seq;
        self.write_thread(&thread)?;
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.active_thread_id = Some(thread.id.clone());
                session.updated_at = thread.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(usage)
    }

    pub fn list_usage(&self, thread_id: Option<&str>, limit: usize) -> AppResult<Vec<UsageRecord>> {
        let mut records = Vec::new();
        match thread_id {
            Some(thread_id) => {
                validate_record_id(thread_id)?;
                self.read_usage_dir(thread_id, &mut records)?;
            }
            None => {
                let root = self.usage_root_dir();
                if root.exists() {
                    for entry in fs::read_dir(root)? {
                        let path = entry?.path();
                        if !path.is_dir() {
                            continue;
                        }
                        let Some(thread_id) = path.file_name().and_then(|value| value.to_str())
                        else {
                            continue;
                        };
                        if validate_record_id(thread_id).is_ok() {
                            self.read_usage_dir(thread_id, &mut records)?;
                        }
                    }
                }
            }
        }
        records.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        records.truncate(limit);
        Ok(records)
    }

    pub fn create_task(
        &self,
        session_id: Option<&str>,
        thread_id: Option<&str>,
        parent_task_id: Option<&str>,
        kind: String,
        status: String,
        summary: String,
    ) -> AppResult<TaskRecord> {
        self.ensure_dirs()?;
        let thread = match thread_id {
            Some(thread_id) => Some(self.load_thread(thread_id)?),
            None => None,
        };
        let resolved_session_id = match (
            session_id,
            thread.as_ref().and_then(|t| t.session_id.as_deref()),
        ) {
            (Some(session_id), Some(thread_session_id)) if session_id != thread_session_id => {
                return Err(app_error(format!(
                    "runtime task session_id `{session_id}` does not match thread session `{thread_session_id}`"
                )));
            }
            (Some(session_id), _) => Some(session_id.to_string()),
            (None, Some(thread_session_id)) => Some(thread_session_id.to_string()),
            (None, None) => None,
        };
        if let Some(session_id) = resolved_session_id.as_deref() {
            self.load_session(session_id)?;
        }
        if let Some(parent_task_id) = parent_task_id {
            self.load_task(parent_task_id)?;
        }
        validate_task_status(&status)?;
        if kind.trim().is_empty() {
            return Err(app_error("runtime task kind must not be empty"));
        }
        let now = epoch_label();
        let task = TaskRecord {
            id: new_id("task"),
            session_id: resolved_session_id,
            thread_id: thread_id.map(str::to_string),
            parent_task_id: parent_task_id.map(str::to_string),
            kind,
            status,
            summary,
            created_at: now.clone(),
            updated_at: now,
        };
        self.write_task(&task)?;
        if let Some(thread_id) = thread_id {
            let event = self.append_event(
                thread_id,
                None,
                "task_recorded",
                JsonValue::Object(object([
                    ("task_id", JsonValue::String(task.id.clone())),
                    ("kind", JsonValue::String(task.kind.clone())),
                    ("status", JsonValue::String(task.status.clone())),
                    ("summary", JsonValue::String(task.summary.clone())),
                ])),
            )?;
            let mut thread = self.load_thread(thread_id)?;
            thread.updated_at = event.created_at;
            thread.event_seq = event.seq;
            self.write_thread(&thread)?;
            if let Some(session_id) = thread.session_id.as_deref() {
                if let Ok(mut session) = self.load_session(session_id) {
                    session.active_thread_id = Some(thread.id.clone());
                    session.updated_at = thread.updated_at.clone();
                    self.write_session(&session)?;
                }
            }
        } else if let Some(session_id) = task.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.updated_at = task.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(task)
    }

    pub fn update_task(
        &self,
        task_id: &str,
        status: String,
        summary: String,
    ) -> AppResult<TaskRecord> {
        validate_record_id(task_id)?;
        validate_task_status(&status)?;
        let mut task = self.load_task(task_id)?;
        task.status = status;
        task.summary = summary;
        task.updated_at = epoch_label();
        self.write_task(&task)?;
        if let Some(thread_id) = task.thread_id.as_deref() {
            let event = self.append_event(
                thread_id,
                None,
                "task_updated",
                JsonValue::Object(object([
                    ("task_id", JsonValue::String(task.id.clone())),
                    ("kind", JsonValue::String(task.kind.clone())),
                    ("status", JsonValue::String(task.status.clone())),
                    ("summary", JsonValue::String(task.summary.clone())),
                ])),
            )?;
            let mut thread = self.load_thread(thread_id)?;
            thread.updated_at = event.created_at;
            thread.event_seq = event.seq;
            self.write_thread(&thread)?;
            if let Some(session_id) = thread.session_id.as_deref() {
                if let Ok(mut session) = self.load_session(session_id) {
                    session.active_thread_id = Some(thread.id.clone());
                    session.updated_at = thread.updated_at.clone();
                    self.write_session(&session)?;
                }
            }
        } else if let Some(session_id) = task.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.updated_at = task.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(task)
    }

    pub fn claim_task(&self, task_id: &str, runner_id: String) -> AppResult<TaskRecord> {
        validate_record_id(task_id)?;
        if runner_id.trim().is_empty() {
            return Err(app_error("runtime task runner_id must not be empty"));
        }
        let mut task = self.load_task(task_id)?;
        if task.status != "pending" {
            return Err(app_error(format!(
                "runtime task {task_id} is `{}` and cannot be claimed",
                task.status
            )));
        }
        task.status = "running".to_string();
        task.updated_at = epoch_label();
        self.write_task(&task)?;
        if let Some(thread_id) = task.thread_id.as_deref() {
            let event = self.append_event(
                thread_id,
                None,
                "task_claimed",
                JsonValue::Object(object([
                    ("task_id", JsonValue::String(task.id.clone())),
                    ("kind", JsonValue::String(task.kind.clone())),
                    ("status", JsonValue::String(task.status.clone())),
                    ("runner_id", JsonValue::String(runner_id)),
                    ("summary", JsonValue::String(task.summary.clone())),
                ])),
            )?;
            let mut thread = self.load_thread(thread_id)?;
            thread.updated_at = event.created_at;
            thread.event_seq = event.seq;
            self.write_thread(&thread)?;
            if let Some(session_id) = thread.session_id.as_deref() {
                if let Ok(mut session) = self.load_session(session_id) {
                    session.active_thread_id = Some(thread.id.clone());
                    session.updated_at = thread.updated_at.clone();
                    self.write_session(&session)?;
                }
            }
        } else if let Some(session_id) = task.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.updated_at = task.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(task)
    }

    pub fn load_task(&self, id: &str) -> AppResult<TaskRecord> {
        validate_record_id(id)?;
        let path = self.task_path(id);
        if !path.exists() {
            return Err(app_error(format!("runtime task not found: {id}")));
        }
        let content = fs::read_to_string(path)?;
        parse_task_record(&parse_root_object(&content)?)
    }

    pub fn list_tasks(
        &self,
        session_id: Option<&str>,
        thread_id: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<TaskRecord>> {
        if let Some(session_id) = session_id {
            validate_record_id(session_id)?;
        }
        if let Some(thread_id) = thread_id {
            validate_record_id(thread_id)?;
        }
        let dir = self.tasks_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            let record = parse_task_record(&parse_root_object(&content)?)?;
            if session_id.is_some_and(|id| record.session_id.as_deref() != Some(id)) {
                continue;
            }
            if thread_id.is_some_and(|id| record.thread_id.as_deref() != Some(id)) {
                continue;
            }
            records.push(record);
        }
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        records.truncate(limit);
        Ok(records)
    }

    pub fn create_automation(
        &self,
        session_id: Option<&str>,
        thread_id: Option<&str>,
        name: String,
        status: String,
        schedule: String,
        prompt: String,
        last_run_at: Option<String>,
        next_run_at: Option<String>,
    ) -> AppResult<AutomationRecord> {
        self.ensure_dirs()?;
        let thread = match thread_id {
            Some(thread_id) => Some(self.load_thread(thread_id)?),
            None => None,
        };
        let resolved_session_id = match (
            session_id,
            thread.as_ref().and_then(|t| t.session_id.as_deref()),
        ) {
            (Some(session_id), Some(thread_session_id)) if session_id != thread_session_id => {
                return Err(app_error(format!(
                    "runtime automation session_id `{session_id}` does not match thread session `{thread_session_id}`"
                )));
            }
            (Some(session_id), _) => Some(session_id.to_string()),
            (None, Some(thread_session_id)) => Some(thread_session_id.to_string()),
            (None, None) => None,
        };
        if let Some(session_id) = resolved_session_id.as_deref() {
            self.load_session(session_id)?;
        }
        validate_automation_status(&status)?;
        if name.trim().is_empty() {
            return Err(app_error("runtime automation name must not be empty"));
        }
        if schedule.trim().is_empty() {
            return Err(app_error("runtime automation schedule must not be empty"));
        }
        let now = epoch_label();
        let automation = AutomationRecord {
            id: new_id("automation"),
            session_id: resolved_session_id,
            thread_id: thread_id.map(str::to_string),
            name,
            status,
            schedule,
            prompt,
            created_at: now.clone(),
            updated_at: now,
            last_run_at,
            next_run_at,
        };
        self.write_automation(&automation)?;
        if let Some(thread_id) = thread_id {
            let event = self.append_event(
                thread_id,
                None,
                "automation_recorded",
                JsonValue::Object(object([
                    ("automation_id", JsonValue::String(automation.id.clone())),
                    ("name", JsonValue::String(automation.name.clone())),
                    ("status", JsonValue::String(automation.status.clone())),
                    ("schedule", JsonValue::String(automation.schedule.clone())),
                ])),
            )?;
            let mut thread = self.load_thread(thread_id)?;
            thread.updated_at = event.created_at;
            thread.event_seq = event.seq;
            self.write_thread(&thread)?;
            if let Some(session_id) = thread.session_id.as_deref() {
                if let Ok(mut session) = self.load_session(session_id) {
                    session.active_thread_id = Some(thread.id.clone());
                    session.updated_at = thread.updated_at.clone();
                    self.write_session(&session)?;
                }
            }
        } else if let Some(session_id) = automation.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.updated_at = automation.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(automation)
    }

    pub fn load_automation(&self, id: &str) -> AppResult<AutomationRecord> {
        validate_record_id(id)?;
        let path = self.automation_path(id);
        if !path.exists() {
            return Err(app_error(format!("runtime automation not found: {id}")));
        }
        let content = fs::read_to_string(path)?;
        parse_automation_record(&parse_root_object(&content)?)
    }

    pub fn trigger_automation(
        &self,
        automation_id: &str,
        prompt_override: Option<String>,
    ) -> AppResult<(AutomationRecord, TaskRecord)> {
        validate_record_id(automation_id)?;
        let mut automation = self.load_automation(automation_id)?;
        if automation.status != "active" {
            return Err(app_error(format!(
                "runtime automation {automation_id} is `{}` and cannot be triggered",
                automation.status
            )));
        }
        let summary = prompt_override
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| automation.prompt.clone());
        if summary.trim().is_empty() {
            return Err(app_error(
                "runtime automation prompt must not be empty when triggered",
            ));
        }
        let task = self.create_task(
            automation.session_id.as_deref(),
            automation.thread_id.as_deref(),
            None,
            "automation".to_string(),
            "pending".to_string(),
            summary.clone(),
        )?;
        let now = epoch_label();
        automation.last_run_at = Some(now.clone());
        automation.updated_at = now;
        self.write_automation(&automation)?;
        if let Some(thread_id) = automation.thread_id.as_deref() {
            let event = self.append_event(
                thread_id,
                None,
                "automation_triggered",
                JsonValue::Object(object([
                    ("automation_id", JsonValue::String(automation.id.clone())),
                    ("task_id", JsonValue::String(task.id.clone())),
                    ("name", JsonValue::String(automation.name.clone())),
                    ("status", JsonValue::String(automation.status.clone())),
                    ("summary", JsonValue::String(summary)),
                ])),
            )?;
            let mut thread = self.load_thread(thread_id)?;
            thread.updated_at = event.created_at;
            thread.event_seq = event.seq;
            self.write_thread(&thread)?;
            if let Some(session_id) = thread.session_id.as_deref() {
                if let Ok(mut session) = self.load_session(session_id) {
                    session.active_thread_id = Some(thread.id.clone());
                    session.updated_at = thread.updated_at.clone();
                    self.write_session(&session)?;
                }
            }
        } else if let Some(session_id) = automation.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.updated_at = automation.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok((automation, task))
    }

    pub fn update_automation(
        &self,
        automation_id: &str,
        name: Option<String>,
        status: Option<String>,
        schedule: Option<String>,
        prompt: Option<String>,
        next_run_at: Option<String>,
    ) -> AppResult<AutomationRecord> {
        validate_record_id(automation_id)?;
        let mut automation = self.load_automation(automation_id)?;
        let mut changed = false;
        if let Some(name) = nonempty_override(name) {
            automation.name = name;
            changed = true;
        }
        if let Some(status) = nonempty_override(status) {
            validate_automation_status(&status)?;
            automation.status = status;
            changed = true;
        }
        if let Some(schedule) = nonempty_override(schedule) {
            automation.schedule = schedule;
            changed = true;
        }
        if let Some(prompt) = nonempty_override(prompt) {
            automation.prompt = prompt;
            changed = true;
        }
        if let Some(next_run_at) = next_run_at {
            automation.next_run_at = nonempty_override(Some(next_run_at));
            changed = true;
        }
        if !changed {
            return Err(app_error(
                "runtime automation update requires at least one field",
            ));
        }
        automation.updated_at = epoch_label();
        self.write_automation(&automation)?;
        self.append_automation_event(&automation, "automation_updated")?;
        Ok(automation)
    }

    pub fn pause_automation(&self, automation_id: &str) -> AppResult<AutomationRecord> {
        self.set_automation_status(automation_id, "paused", "automation_paused")
    }

    pub fn resume_automation(&self, automation_id: &str) -> AppResult<AutomationRecord> {
        self.set_automation_status(automation_id, "active", "automation_resumed")
    }

    pub fn delete_automation(&self, automation_id: &str) -> AppResult<AutomationRecord> {
        validate_record_id(automation_id)?;
        let mut automation = self.load_automation(automation_id)?;
        fs::remove_file(self.automation_path(automation_id))?;
        automation.status = "cancelled".to_string();
        automation.updated_at = epoch_label();
        self.append_automation_event(&automation, "automation_deleted")?;
        Ok(automation)
    }

    pub fn update_automation_next_run(
        &self,
        automation_id: &str,
        next_run_at: Option<String>,
    ) -> AppResult<AutomationRecord> {
        validate_record_id(automation_id)?;
        let mut automation = self.load_automation(automation_id)?;
        automation.next_run_at = next_run_at;
        automation.updated_at = epoch_label();
        self.write_automation(&automation)?;
        if let Some(thread_id) = automation.thread_id.as_deref() {
            let next_run_at = automation
                .next_run_at
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null);
            let event = self.append_event(
                thread_id,
                None,
                "automation_scheduled",
                JsonValue::Object(object([
                    ("automation_id", JsonValue::String(automation.id.clone())),
                    ("name", JsonValue::String(automation.name.clone())),
                    ("status", JsonValue::String(automation.status.clone())),
                    ("schedule", JsonValue::String(automation.schedule.clone())),
                    ("next_run_at", next_run_at),
                ])),
            )?;
            let mut thread = self.load_thread(thread_id)?;
            thread.updated_at = event.created_at;
            thread.event_seq = event.seq;
            self.write_thread(&thread)?;
            if let Some(session_id) = thread.session_id.as_deref() {
                if let Ok(mut session) = self.load_session(session_id) {
                    session.active_thread_id = Some(thread.id.clone());
                    session.updated_at = thread.updated_at.clone();
                    self.write_session(&session)?;
                }
            }
        } else if let Some(session_id) = automation.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.updated_at = automation.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(automation)
    }

    pub fn list_automations(
        &self,
        session_id: Option<&str>,
        thread_id: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<AutomationRecord>> {
        if let Some(session_id) = session_id {
            validate_record_id(session_id)?;
        }
        if let Some(thread_id) = thread_id {
            validate_record_id(thread_id)?;
        }
        let dir = self.automations_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            let record = parse_automation_record(&parse_root_object(&content)?)?;
            if session_id.is_some_and(|id| record.session_id.as_deref() != Some(id)) {
                continue;
            }
            if thread_id.is_some_and(|id| record.thread_id.as_deref() != Some(id)) {
                continue;
            }
            records.push(record);
        }
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        records.truncate(limit);
        Ok(records)
    }

    fn append_event(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        kind: &str,
        payload: JsonValue,
    ) -> AppResult<RuntimeEvent> {
        let _write_guard = runtime_write_lock()
            .lock()
            .map_err(|_| app_error("runtime event write lock is poisoned"))?;
        validate_record_id(thread_id)?;
        if let Some(turn_id) = turn_id {
            validate_record_id(turn_id)?;
        }
        let seq = self
            .read_events(thread_id, 0)?
            .last()
            .map(|event| event.seq + 1)
            .unwrap_or(1);
        let event = RuntimeEvent {
            id: new_id("event"),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.map(str::to_string),
            seq,
            kind: kind.to_string(),
            created_at: epoch_label(),
            payload,
        };
        fs::create_dir_all(self.events_dir())?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.events_path(thread_id))?;
        let mut line = json_value_to_string(&event_to_json(&event));
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(event)
    }

    fn ensure_dirs(&self) -> AppResult<()> {
        fs::create_dir_all(self.threads_dir())?;
        fs::create_dir_all(self.sessions_dir())?;
        fs::create_dir_all(self.turns_root_dir())?;
        fs::create_dir_all(self.items_root_dir())?;
        fs::create_dir_all(self.events_dir())?;
        fs::create_dir_all(self.usage_root_dir())?;
        fs::create_dir_all(self.tasks_dir())?;
        fs::create_dir_all(self.automations_dir())?;
        Ok(())
    }

    fn write_session(&self, session: &SessionRecord) -> AppResult<()> {
        fs::create_dir_all(self.sessions_dir())?;
        write_json_file(
            self.session_path(&session.id),
            json_value_to_string(&session_to_json(session)),
        )?;
        Ok(())
    }

    fn write_thread(&self, thread: &ThreadRecord) -> AppResult<()> {
        fs::create_dir_all(self.threads_dir())?;
        write_json_file(
            self.thread_path(&thread.id),
            json_value_to_string(&thread_to_json(thread)),
        )?;
        Ok(())
    }

    fn write_turn(&self, turn: &TurnRecord) -> AppResult<()> {
        fs::create_dir_all(self.turns_dir(&turn.thread_id))?;
        write_json_file(
            self.turn_path(&turn.thread_id, &turn.id),
            json_value_to_string(&turn_to_json(turn)),
        )?;
        Ok(())
    }

    fn write_item(&self, item: &ItemRecord) -> AppResult<()> {
        fs::create_dir_all(self.items_dir(&item.thread_id))?;
        write_json_file(
            self.item_path(&item.thread_id, &item.id),
            json_value_to_string(&item_to_json(item)),
        )?;
        Ok(())
    }

    fn write_usage(&self, usage: &UsageRecord) -> AppResult<()> {
        fs::create_dir_all(self.usage_dir(&usage.thread_id))?;
        write_json_file(
            self.usage_path(&usage.thread_id, &usage.id),
            json_value_to_string(&usage_to_json(usage)),
        )?;
        Ok(())
    }

    fn write_task(&self, task: &TaskRecord) -> AppResult<()> {
        fs::create_dir_all(self.tasks_dir())?;
        write_json_file(
            self.task_path(&task.id),
            json_value_to_string(&task_to_json(task)),
        )?;
        Ok(())
    }

    fn write_automation(&self, automation: &AutomationRecord) -> AppResult<()> {
        fs::create_dir_all(self.automations_dir())?;
        write_json_file(
            self.automation_path(&automation.id),
            json_value_to_string(&automation_to_json(automation)),
        )?;
        Ok(())
    }

    fn set_automation_status(
        &self,
        automation_id: &str,
        status: &str,
        event_kind: &str,
    ) -> AppResult<AutomationRecord> {
        validate_record_id(automation_id)?;
        validate_automation_status(status)?;
        let mut automation = self.load_automation(automation_id)?;
        automation.status = status.to_string();
        automation.updated_at = epoch_label();
        self.write_automation(&automation)?;
        self.append_automation_event(&automation, event_kind)?;
        Ok(automation)
    }

    fn append_automation_event(&self, automation: &AutomationRecord, kind: &str) -> AppResult<()> {
        if let Some(thread_id) = automation.thread_id.as_deref() {
            let event =
                self.append_event(thread_id, None, kind, automation_event_payload(automation))?;
            let mut thread = self.load_thread(thread_id)?;
            thread.updated_at = event.created_at;
            thread.event_seq = event.seq;
            self.write_thread(&thread)?;
            if let Some(session_id) = thread.session_id.as_deref() {
                if let Ok(mut session) = self.load_session(session_id) {
                    session.active_thread_id = Some(thread.id.clone());
                    session.updated_at = thread.updated_at.clone();
                    self.write_session(&session)?;
                }
            }
        } else if let Some(session_id) = automation.session_id.as_deref() {
            if let Ok(mut session) = self.load_session(session_id) {
                session.updated_at = automation.updated_at.clone();
                self.write_session(&session)?;
            }
        }
        Ok(())
    }

    fn read_usage_dir(&self, thread_id: &str, records: &mut Vec<UsageRecord>) -> AppResult<()> {
        validate_record_id(thread_id)?;
        let dir = self.usage_dir(thread_id);
        if !dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            records.push(parse_usage_record(&parse_root_object(&content)?)?);
        }
        Ok(())
    }

    fn threads_dir(&self) -> PathBuf {
        self.root.join("threads")
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.json"))
    }

    fn thread_path(&self, id: &str) -> PathBuf {
        self.threads_dir().join(format!("{id}.json"))
    }

    fn turns_root_dir(&self) -> PathBuf {
        self.root.join("turns")
    }

    fn turns_dir(&self, thread_id: &str) -> PathBuf {
        self.turns_root_dir().join(thread_id)
    }

    fn turn_path(&self, thread_id: &str, turn_id: &str) -> PathBuf {
        self.turns_dir(thread_id).join(format!("{turn_id}.json"))
    }

    fn items_root_dir(&self) -> PathBuf {
        self.root.join("items")
    }

    fn items_dir(&self, thread_id: &str) -> PathBuf {
        self.items_root_dir().join(thread_id)
    }

    fn item_path(&self, thread_id: &str, item_id: &str) -> PathBuf {
        self.items_dir(thread_id).join(format!("{item_id}.json"))
    }

    fn events_dir(&self) -> PathBuf {
        self.root.join("events")
    }

    fn events_path(&self, thread_id: &str) -> PathBuf {
        self.events_dir().join(format!("{thread_id}.jsonl"))
    }

    fn usage_root_dir(&self) -> PathBuf {
        self.root.join("usage")
    }

    fn usage_dir(&self, thread_id: &str) -> PathBuf {
        self.usage_root_dir().join(thread_id)
    }

    fn usage_path(&self, thread_id: &str, usage_id: &str) -> PathBuf {
        self.usage_dir(thread_id).join(format!("{usage_id}.json"))
    }

    fn tasks_dir(&self) -> PathBuf {
        self.root.join("tasks")
    }

    fn task_path(&self, id: &str) -> PathBuf {
        self.tasks_dir().join(format!("{id}.json"))
    }

    fn automations_dir(&self) -> PathBuf {
        self.root.join("automations")
    }

    fn automation_path(&self, id: &str) -> PathBuf {
        self.automations_dir().join(format!("{id}.json"))
    }
}

pub fn session_to_json(session: &SessionRecord) -> JsonValue {
    let active_thread_id = session
        .active_thread_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("id", JsonValue::String(session.id.clone())),
        ("created_at", JsonValue::String(session.created_at.clone())),
        ("updated_at", JsonValue::String(session.updated_at.clone())),
        ("title", JsonValue::String(session.title.clone())),
        ("workspace", JsonValue::String(session.workspace.clone())),
        ("status", JsonValue::String(session.status.clone())),
        ("active_thread_id", active_thread_id),
        (
            "thread_count",
            JsonValue::Number(session.thread_count.to_string()),
        ),
    ]))
}

pub fn thread_to_json(thread: &ThreadRecord) -> JsonValue {
    let latest_turn_id = thread
        .latest_turn_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let session_id = thread
        .session_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("id", JsonValue::String(thread.id.clone())),
        ("session_id", session_id),
        ("created_at", JsonValue::String(thread.created_at.clone())),
        ("updated_at", JsonValue::String(thread.updated_at.clone())),
        ("title", JsonValue::String(thread.title.clone())),
        ("workspace", JsonValue::String(thread.workspace.clone())),
        ("model", JsonValue::String(thread.model.clone())),
        ("mode", JsonValue::String(thread.mode.clone())),
        ("status", JsonValue::String(thread.status.clone())),
        ("latest_turn_id", latest_turn_id),
        ("event_seq", JsonValue::Number(thread.event_seq.to_string())),
    ]))
}

pub fn turn_to_json(turn: &TurnRecord) -> JsonValue {
    JsonValue::Object(object([
        ("id", JsonValue::String(turn.id.clone())),
        ("thread_id", JsonValue::String(turn.thread_id.clone())),
        ("index", JsonValue::Number(turn.index.to_string())),
        ("role", JsonValue::String(turn.role.clone())),
        ("content", JsonValue::String(turn.content.clone())),
        ("status", JsonValue::String(turn.status.clone())),
        ("created_at", JsonValue::String(turn.created_at.clone())),
    ]))
}

pub fn item_to_json(item: &ItemRecord) -> JsonValue {
    let turn_id = item
        .turn_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let role = item
        .role
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("id", JsonValue::String(item.id.clone())),
        ("thread_id", JsonValue::String(item.thread_id.clone())),
        ("turn_id", turn_id),
        ("index", JsonValue::Number(item.index.to_string())),
        ("item_type", JsonValue::String(item.item_type.clone())),
        ("role", role),
        ("content", JsonValue::String(item.content.clone())),
        ("status", JsonValue::String(item.status.clone())),
        ("created_at", JsonValue::String(item.created_at.clone())),
    ]))
}

pub fn event_to_json(event: &RuntimeEvent) -> JsonValue {
    let turn_id = event
        .turn_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("id", JsonValue::String(event.id.clone())),
        ("thread_id", JsonValue::String(event.thread_id.clone())),
        ("turn_id", turn_id),
        ("seq", JsonValue::Number(event.seq.to_string())),
        ("kind", JsonValue::String(event.kind.clone())),
        ("created_at", JsonValue::String(event.created_at.clone())),
        ("payload", event.payload.clone()),
    ]))
}

pub fn thread_compaction_to_json(record: &ThreadCompactionRecord) -> JsonValue {
    JsonValue::Object(object([
        ("thread_id", JsonValue::String(record.thread_id.clone())),
        (
            "keep_tail_turns",
            JsonValue::Number(record.keep_tail_turns.to_string()),
        ),
        (
            "summarized_turn_count",
            JsonValue::Number(record.summarized_turn_count.to_string()),
        ),
        (
            "kept_turn_count",
            JsonValue::Number(record.kept_turn_count.to_string()),
        ),
        (
            "summary_source",
            JsonValue::String(record.summary_source.clone()),
        ),
        (
            "summarized_turn_ids",
            string_array(&record.summarized_turn_ids),
        ),
        ("kept_turn_ids", string_array(&record.kept_turn_ids)),
        ("summary_turn", turn_to_json(&record.summary_turn)),
        ("summary_item", item_to_json(&record.summary_item)),
        ("event", event_to_json(&record.event)),
    ]))
}

pub fn thread_fork_to_json(record: &ThreadForkRecord) -> JsonValue {
    JsonValue::Object(object([
        (
            "source_thread_id",
            JsonValue::String(record.source_thread_id.clone()),
        ),
        ("thread", thread_to_json(&record.thread)),
        (
            "copied_turn_count",
            JsonValue::Number(record.copied_turn_count.to_string()),
        ),
        (
            "copied_item_count",
            JsonValue::Number(record.copied_item_count.to_string()),
        ),
        ("event", event_to_json(&record.event)),
    ]))
}

pub fn usage_to_json(usage: &UsageRecord) -> JsonValue {
    let turn_id = usage
        .turn_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let estimated_input_cost_microusd = usage
        .estimated_input_cost_microusd
        .map(|value| JsonValue::Number(value.to_string()))
        .unwrap_or(JsonValue::Null);
    let estimated_output_cost_microusd = usage
        .estimated_output_cost_microusd
        .map(|value| JsonValue::Number(value.to_string()))
        .unwrap_or(JsonValue::Null);
    let estimated_total_cost_microusd = usage
        .estimated_total_cost_microusd
        .map(|value| JsonValue::Number(value.to_string()))
        .unwrap_or(JsonValue::Null);
    let pricing_source = usage
        .pricing_source
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("id", JsonValue::String(usage.id.clone())),
        ("thread_id", JsonValue::String(usage.thread_id.clone())),
        ("turn_id", turn_id),
        ("model", JsonValue::String(usage.model.clone())),
        ("source", JsonValue::String(usage.source.clone())),
        (
            "prompt_tokens",
            JsonValue::Number(usage.prompt_tokens.to_string()),
        ),
        (
            "completion_tokens",
            JsonValue::Number(usage.completion_tokens.to_string()),
        ),
        (
            "total_tokens",
            JsonValue::Number(usage.total_tokens.to_string()),
        ),
        (
            "prompt_cache_hit_tokens",
            JsonValue::Number(usage.prompt_cache_hit_tokens.to_string()),
        ),
        (
            "prompt_cache_miss_tokens",
            JsonValue::Number(usage.prompt_cache_miss_tokens.to_string()),
        ),
        (
            "estimated_input_cost_microusd",
            estimated_input_cost_microusd,
        ),
        (
            "estimated_output_cost_microusd",
            estimated_output_cost_microusd,
        ),
        (
            "estimated_total_cost_microusd",
            estimated_total_cost_microusd,
        ),
        ("pricing_source", pricing_source),
        ("created_at", JsonValue::String(usage.created_at.clone())),
    ]))
}

pub fn task_to_json(task: &TaskRecord) -> JsonValue {
    let session_id = task
        .session_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let thread_id = task
        .thread_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let parent_task_id = task
        .parent_task_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("id", JsonValue::String(task.id.clone())),
        ("session_id", session_id),
        ("thread_id", thread_id),
        ("parent_task_id", parent_task_id),
        ("kind", JsonValue::String(task.kind.clone())),
        ("status", JsonValue::String(task.status.clone())),
        ("summary", JsonValue::String(task.summary.clone())),
        ("created_at", JsonValue::String(task.created_at.clone())),
        ("updated_at", JsonValue::String(task.updated_at.clone())),
    ]))
}

pub fn automation_to_json(automation: &AutomationRecord) -> JsonValue {
    let session_id = automation
        .session_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let thread_id = automation
        .thread_id
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let last_run_at = automation
        .last_run_at
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let next_run_at = automation
        .next_run_at
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("id", JsonValue::String(automation.id.clone())),
        ("session_id", session_id),
        ("thread_id", thread_id),
        ("name", JsonValue::String(automation.name.clone())),
        ("status", JsonValue::String(automation.status.clone())),
        ("schedule", JsonValue::String(automation.schedule.clone())),
        ("prompt", JsonValue::String(automation.prompt.clone())),
        (
            "created_at",
            JsonValue::String(automation.created_at.clone()),
        ),
        (
            "updated_at",
            JsonValue::String(automation.updated_at.clone()),
        ),
        ("last_run_at", last_run_at),
        ("next_run_at", next_run_at),
    ]))
}

fn automation_event_payload(automation: &AutomationRecord) -> JsonValue {
    let last_run_at = automation
        .last_run_at
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let next_run_at = automation
        .next_run_at
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    JsonValue::Object(object([
        ("automation_id", JsonValue::String(automation.id.clone())),
        ("name", JsonValue::String(automation.name.clone())),
        ("status", JsonValue::String(automation.status.clone())),
        ("schedule", JsonValue::String(automation.schedule.clone())),
        ("prompt", JsonValue::String(automation.prompt.clone())),
        ("last_run_at", last_run_at),
        ("next_run_at", next_run_at),
    ]))
}

pub(crate) fn parse_session_record(root: &BTreeMap<String, JsonValue>) -> AppResult<SessionRecord> {
    Ok(SessionRecord {
        id: required_string(root, "id")?,
        created_at: required_string(root, "created_at")?,
        updated_at: required_string(root, "updated_at")?,
        title: required_string(root, "title")?,
        workspace: required_string(root, "workspace")?,
        status: required_string(root, "status")?,
        active_thread_id: optional_string(root, "active_thread_id")?,
        thread_count: required_u64(root, "thread_count")?,
    })
}

pub(crate) fn parse_thread_record(root: &BTreeMap<String, JsonValue>) -> AppResult<ThreadRecord> {
    Ok(ThreadRecord {
        id: required_string(root, "id")?,
        session_id: optional_string(root, "session_id")?,
        created_at: required_string(root, "created_at")?,
        updated_at: required_string(root, "updated_at")?,
        title: required_string(root, "title")?,
        workspace: required_string(root, "workspace")?,
        model: required_string(root, "model")?,
        mode: required_string(root, "mode")?,
        status: required_string(root, "status")?,
        latest_turn_id: optional_string(root, "latest_turn_id")?,
        event_seq: required_u64(root, "event_seq")?,
    })
}

pub(crate) fn parse_turn_record(root: &BTreeMap<String, JsonValue>) -> AppResult<TurnRecord> {
    Ok(TurnRecord {
        id: required_string(root, "id")?,
        thread_id: required_string(root, "thread_id")?,
        index: required_u64(root, "index")?,
        role: required_string(root, "role")?,
        content: required_string(root, "content")?,
        status: required_string(root, "status")?,
        created_at: required_string(root, "created_at")?,
    })
}

pub(crate) fn parse_item_record(root: &BTreeMap<String, JsonValue>) -> AppResult<ItemRecord> {
    Ok(ItemRecord {
        id: required_string(root, "id")?,
        thread_id: required_string(root, "thread_id")?,
        turn_id: optional_string(root, "turn_id")?,
        index: required_u64(root, "index")?,
        item_type: required_string(root, "item_type")?,
        role: optional_string(root, "role")?,
        content: required_string(root, "content")?,
        status: required_string(root, "status")?,
        created_at: required_string(root, "created_at")?,
    })
}

pub(crate) fn parse_usage_record(root: &BTreeMap<String, JsonValue>) -> AppResult<UsageRecord> {
    let prompt_tokens = required_u64(root, "prompt_tokens")?;
    let prompt_cache_hit_tokens = optional_u64(root, "prompt_cache_hit_tokens")?.unwrap_or(0);
    let prompt_cache_miss_tokens = optional_u64(root, "prompt_cache_miss_tokens")?
        .unwrap_or_else(|| prompt_tokens.saturating_sub(prompt_cache_hit_tokens));
    Ok(UsageRecord {
        id: required_string(root, "id")?,
        thread_id: required_string(root, "thread_id")?,
        turn_id: optional_string(root, "turn_id")?,
        model: required_string(root, "model")?,
        source: required_string(root, "source")?,
        prompt_tokens,
        completion_tokens: required_u64(root, "completion_tokens")?,
        total_tokens: required_u64(root, "total_tokens")?,
        prompt_cache_hit_tokens,
        prompt_cache_miss_tokens,
        estimated_input_cost_microusd: optional_u64(root, "estimated_input_cost_microusd")?,
        estimated_output_cost_microusd: optional_u64(root, "estimated_output_cost_microusd")?,
        estimated_total_cost_microusd: optional_u64(root, "estimated_total_cost_microusd")?,
        pricing_source: optional_string(root, "pricing_source")?,
        created_at: required_string(root, "created_at")?,
    })
}

pub(crate) fn parse_task_record(root: &BTreeMap<String, JsonValue>) -> AppResult<TaskRecord> {
    Ok(TaskRecord {
        id: required_string(root, "id")?,
        session_id: optional_string(root, "session_id")?,
        thread_id: optional_string(root, "thread_id")?,
        parent_task_id: optional_string(root, "parent_task_id")?,
        kind: required_string(root, "kind")?,
        status: required_string(root, "status")?,
        summary: required_string(root, "summary")?,
        created_at: required_string(root, "created_at")?,
        updated_at: required_string(root, "updated_at")?,
    })
}

pub(crate) fn parse_automation_record(
    root: &BTreeMap<String, JsonValue>,
) -> AppResult<AutomationRecord> {
    Ok(AutomationRecord {
        id: required_string(root, "id")?,
        session_id: optional_string(root, "session_id")?,
        thread_id: optional_string(root, "thread_id")?,
        name: required_string(root, "name")?,
        status: required_string(root, "status")?,
        schedule: required_string(root, "schedule")?,
        prompt: required_string(root, "prompt")?,
        created_at: required_string(root, "created_at")?,
        updated_at: required_string(root, "updated_at")?,
        last_run_at: optional_string(root, "last_run_at")?,
        next_run_at: optional_string(root, "next_run_at")?,
    })
}

pub(crate) fn parse_runtime_event(root: &BTreeMap<String, JsonValue>) -> AppResult<RuntimeEvent> {
    let payload = root
        .get("payload")
        .cloned()
        .unwrap_or_else(|| JsonValue::Object(BTreeMap::new()));
    Ok(RuntimeEvent {
        id: required_string(root, "id")?,
        thread_id: required_string(root, "thread_id")?,
        turn_id: optional_string(root, "turn_id")?,
        seq: required_u64(root, "seq")?,
        kind: required_string(root, "kind")?,
        created_at: required_string(root, "created_at")?,
        payload,
    })
}

fn build_compaction_summary(
    thread: &ThreadRecord,
    summarized_turns: &[TurnRecord],
    kept_turns: &[TurnRecord],
) -> String {
    let mut summary = String::new();
    summary.push_str("Compacted runtime thread summary\n");
    summary.push_str("Thread: ");
    summary.push_str(&thread.title);
    summary.push('\n');
    summary.push_str("Summarized turns: ");
    summary.push_str(&summarized_turns.len().to_string());
    summary.push('\n');
    summary.push_str("Kept tail turns: ");
    summary.push_str(&kept_turns.len().to_string());
    summary.push_str("\n\nPrior turns:\n");

    const MAX_TURN_LINES: usize = 24;
    for turn in summarized_turns.iter().take(MAX_TURN_LINES) {
        summary.push_str("- #");
        summary.push_str(&turn.index.to_string());
        summary.push(' ');
        summary.push_str(&turn.role);
        summary.push_str(": ");
        summary.push_str(&compact_excerpt(&turn.content, 180));
        summary.push('\n');
    }
    if summarized_turns.len() > MAX_TURN_LINES {
        summary.push_str("- ... ");
        summary.push_str(&(summarized_turns.len() - MAX_TURN_LINES).to_string());
        summary.push_str(" additional prior turns omitted from extractive summary\n");
    }
    if let Some(first_kept) = kept_turns.first() {
        summary.push_str("\nTail preserved from turn #");
        summary.push_str(&first_kept.index.to_string());
        summary.push_str(" onward.");
    }
    summary
}

fn compact_excerpt(content: &str, max_chars: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        excerpt.push_str("...");
    }
    excerpt
}

fn string_array(values: &[String]) -> JsonValue {
    JsonValue::Array(
        values
            .iter()
            .map(|value| JsonValue::String(value.clone()))
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageCostEstimate {
    input: u64,
    output: u64,
    total: u64,
    source: String,
}

fn estimate_deepseek_cost_microusd(
    model: &str,
    cache_hit_tokens: u64,
    cache_miss_tokens: u64,
    output_tokens: u64,
) -> Option<UsageCostEstimate> {
    let pricing = pricing_for_model(model)?;
    let input =
        cost_microusd(cache_hit_tokens, pricing.cache_hit_microusd_per_million).saturating_add(
            cost_microusd(cache_miss_tokens, pricing.cache_miss_microusd_per_million),
        );
    let output = cost_microusd(output_tokens, pricing.output_microusd_per_million);
    let total = input.saturating_add(output);
    Some(UsageCostEstimate {
        input,
        output,
        total,
        source: pricing.source.to_string(),
    })
}

#[derive(Debug, Clone, Copy)]
struct DeepSeekPricing {
    cache_hit_microusd_per_million: u64,
    cache_miss_microusd_per_million: u64,
    output_microusd_per_million: u64,
    source: &'static str,
}

fn pricing_for_model(model: &str) -> Option<DeepSeekPricing> {
    let model = model.to_ascii_lowercase();
    if model.contains("deepseek-v4-pro") {
        return Some(DeepSeekPricing {
            cache_hit_microusd_per_million: 3_625,
            cache_miss_microusd_per_million: 435_000,
            output_microusd_per_million: 870_000,
            source: "DeepSeek V4 Pro official USD pricing, 75% promo through 2026-05-31",
        });
    }
    if model.contains("deepseek-v4-flash")
        || model == "deepseek-chat"
        || model == "deepseek-reasoner"
    {
        return Some(DeepSeekPricing {
            cache_hit_microusd_per_million: 2_800,
            cache_miss_microusd_per_million: 140_000,
            output_microusd_per_million: 280_000,
            source: "DeepSeek V4 Flash official USD pricing, effective 2026-04-26",
        });
    }
    None
}

fn cost_microusd(tokens: u64, microusd_per_million: u64) -> u64 {
    let raw = tokens as u128 * microusd_per_million as u128;
    ((raw + 500_000) / 1_000_000).min(u64::MAX as u128) as u64
}

fn required_string(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<String> {
    root.get(key)
        .and_then(json_as_string)
        .map(str::to_string)
        .ok_or_else(|| app_error(format!("runtime record missing string `{key}`")))
}

fn optional_string(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<Option<String>> {
    match root.get(key) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(value) => json_as_string(value)
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| app_error(format!("runtime record `{key}` must be string or null"))),
    }
}

fn required_u64(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<u64> {
    root.get(key)
        .and_then(json_as_u64)
        .ok_or_else(|| app_error(format!("runtime record missing number `{key}`")))
}

fn optional_u64(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<Option<u64>> {
    match root.get(key) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(value) => json_as_u64(value)
            .map(Some)
            .ok_or_else(|| app_error(format!("runtime record `{key}` must be number or null"))),
    }
}

pub fn validate_record_id(id: &str) -> AppResult<()> {
    let valid = !id.is_empty()
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(app_error(format!("invalid runtime record id `{id}`")))
    }
}

fn validate_turn_role(role: &str) -> AppResult<()> {
    if matches!(role, "user" | "assistant" | "tool" | "system") {
        Ok(())
    } else {
        Err(app_error(format!("invalid runtime turn role `{role}`")))
    }
}

fn validate_item_type(item_type: &str) -> AppResult<()> {
    if matches!(
        item_type,
        "message" | "tool_call" | "tool_result" | "reasoning" | "diagnostic" | "event" | "summary"
    ) {
        Ok(())
    } else {
        Err(app_error(format!(
            "invalid runtime item type `{item_type}`"
        )))
    }
}

fn validate_item_status(status: &str) -> AppResult<()> {
    validate_record_status(status)
}

fn validate_record_status(status: &str) -> AppResult<()> {
    if matches!(
        status,
        "pending" | "running" | "completed" | "failed" | "cancelled"
    ) {
        Ok(())
    } else {
        Err(app_error(format!("invalid runtime item status `{status}`")))
    }
}

fn validate_task_status(status: &str) -> AppResult<()> {
    if matches!(
        status,
        "pending" | "paused" | "running" | "completed" | "failed" | "cancelled"
    ) {
        Ok(())
    } else {
        Err(app_error(format!("invalid runtime task status `{status}`")))
    }
}

fn validate_user_input_questions(value: &JsonValue) -> AppResult<()> {
    let questions = json_as_array(value)
        .ok_or_else(|| app_error("user_input_request.questions must be a JSON array"))?;
    if questions.is_empty() || questions.len() > 3 {
        return Err(app_error(
            "user_input_request.questions must contain 1 to 3 items",
        ));
    }
    for question in questions {
        let JsonValue::Object(question) = question else {
            return Err(app_error(
                "user_input_request.questions items must be objects",
            ));
        };
        required_nonempty_payload_string(question, "header", "user_input_request question header")?;
        required_nonempty_payload_string(question, "id", "user_input_request question id")?;
        required_nonempty_payload_string(question, "question", "user_input_request question text")?;
        let options = question
            .get("options")
            .and_then(json_as_array)
            .ok_or_else(|| app_error("user_input_request question options must be an array"))?;
        if options.len() < 2 || options.len() > 3 {
            return Err(app_error(
                "user_input_request question options must contain 2 or 3 items",
            ));
        }
        for option in options {
            let JsonValue::Object(option) = option else {
                return Err(app_error(
                    "user_input_request question options must be objects",
                ));
            };
            required_nonempty_payload_string(option, "label", "user_input_request option label")?;
            required_nonempty_payload_string(
                option,
                "description",
                "user_input_request option description",
            )?;
        }
    }
    Ok(())
}

fn required_nonempty_payload_string(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
    label: &str,
) -> AppResult<String> {
    let value = root
        .get(key)
        .and_then(json_as_string)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error(format!("{label} cannot be empty")))?;
    Ok(value.to_string())
}

fn nonempty_override(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn validate_automation_status(status: &str) -> AppResult<()> {
    if matches!(
        status,
        "active" | "paused" | "completed" | "failed" | "cancelled"
    ) {
        Ok(())
    } else {
        Err(app_error(format!(
            "invalid runtime automation status `{status}`"
        )))
    }
}

fn object<const N: usize>(items: [(&str, JsonValue); N]) -> BTreeMap<String, JsonValue> {
    let mut map = BTreeMap::new();
    for (key, value) in items {
        map.insert(key.to_string(), value);
    }
    map
}

fn epoch_label() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("epoch+{secs}")
}

fn new_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos}")
}

pub fn json_string_field(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
    default: &str,
) -> AppResult<String> {
    match root.get(key) {
        Some(value) => json_as_string(value)
            .map(str::to_string)
            .ok_or_else(|| app_error(format!("request field `{key}` must be a string"))),
        None => Ok(default.to_string()),
    }
}

pub fn json_limit_field(root: &BTreeMap<String, JsonValue>, key: &str, default: usize) -> usize {
    root.get(key)
        .and_then(json_as_u64)
        .map(|value| value.clamp(1, 200) as usize)
        .unwrap_or(default)
}

pub fn json_array(items: Vec<JsonValue>) -> JsonValue {
    JsonValue::Array(items)
}

pub fn json_object<const N: usize>(items: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(object(items))
}

pub fn parse_json_object_body(body: &str) -> AppResult<BTreeMap<String, JsonValue>> {
    if body.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    parse_root_object(body)
}

pub fn json_as_object_array(value: &JsonValue) -> Option<&Vec<JsonValue>> {
    json_as_array(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-runtime-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn creates_thread_and_persists_event() {
        let store = RuntimeStore::new(temp_root("thread"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let loaded = store.load_thread(&thread.id).unwrap();
        assert_eq!(loaded.title, "Investigate");
        assert_eq!(loaded.event_seq, 1);
        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "thread_created");
    }

    #[test]
    fn runtime_store_appends_external_thread_event() {
        let store = RuntimeStore::new(temp_root("external-event"));
        let session = store
            .create_session("External events".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "External event thread".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let event = store
            .append_thread_event(
                &thread.id,
                "external_progress",
                JsonValue::Object(object([(
                    "message",
                    JsonValue::String("running".to_string()),
                )])),
            )
            .unwrap();

        assert_eq!(event.kind, "external_progress");
        assert_eq!(event.seq, 2);
        let loaded_thread = store.load_thread(&thread.id).unwrap();
        assert_eq!(loaded_thread.event_seq, 2);
        let loaded_session = store.load_session(&session.id).unwrap();
        assert_eq!(
            loaded_session.active_thread_id.as_deref(),
            Some(thread.id.as_str())
        );
        assert_eq!(
            store.read_events(&thread.id, 1).unwrap()[0].kind,
            "external_progress"
        );
    }

    #[test]
    fn append_turn_updates_thread_and_event_stream() {
        let store = RuntimeStore::new(temp_root("turn"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "user".to_string(), "hello".to_string())
            .unwrap();

        let loaded = store.load_thread(&thread.id).unwrap();
        assert_eq!(loaded.latest_turn_id.as_deref(), Some(turn.id.as_str()));
        assert_eq!(loaded.event_seq, 2);
        assert_eq!(store.list_turns(&thread.id).unwrap().len(), 1);
        assert_eq!(store.read_events(&thread.id, 1).unwrap().len(), 1);
    }

    #[test]
    fn append_item_links_turn_and_event_stream() {
        let store = RuntimeStore::new(temp_root("item"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();
        let item = store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "done".to_string(),
                "completed".to_string(),
            )
            .unwrap();

        assert_eq!(item.turn_id.as_deref(), Some(turn.id.as_str()));
        assert_eq!(item.index, 1);
        let items = store.list_items(&thread.id, Some(&turn.id)).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].role.as_deref(), Some("assistant"));
        assert_eq!(
            store.load_item(&thread.id, &item.id).unwrap().content,
            "done"
        );
        let loaded = store.load_thread(&thread.id).unwrap();
        assert_eq!(loaded.event_seq, 3);
        let events = store.read_events(&thread.id, 2).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "item_recorded");
    }

    #[test]
    fn fork_thread_copies_turns_and_items_with_new_ids() {
        let store = RuntimeStore::new(temp_root("fork-thread"));
        let session = store
            .create_session("Session".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let user_turn = store
            .append_turn(&thread.id, "user".to_string(), "hello".to_string())
            .unwrap();
        let assistant_turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();
        let message = store
            .append_item(
                &thread.id,
                Some(&assistant_turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "done".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&assistant_turn.id),
                "reasoning".to_string(),
                Some("assistant".to_string()),
                "reasoning trace".to_string(),
                "completed".to_string(),
            )
            .unwrap();

        let fork = store
            .fork_thread(&thread.id, Some("Alternative path".to_string()))
            .unwrap();

        assert_ne!(fork.thread.id, thread.id);
        assert_eq!(fork.source_thread_id, thread.id);
        assert_eq!(fork.thread.title, "Alternative path");
        assert_eq!(fork.thread.session_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(fork.copied_turn_count, 2);
        assert_eq!(fork.copied_item_count, 2);
        assert_eq!(fork.event.kind, "thread_forked");

        let fork_turns = store.list_turns(&fork.thread.id).unwrap();
        assert_eq!(fork_turns.len(), 2);
        assert_ne!(fork_turns[0].id, user_turn.id);
        assert_eq!(fork_turns[0].content, "hello");
        assert_eq!(fork_turns[1].content, "done");
        assert_eq!(
            store
                .load_thread(&fork.thread.id)
                .unwrap()
                .latest_turn_id
                .as_deref(),
            Some(fork_turns[1].id.as_str())
        );

        let fork_items = store.list_items(&fork.thread.id, None).unwrap();
        assert_eq!(fork_items.len(), 2);
        assert_ne!(fork_items[0].id, message.id);
        assert_eq!(fork_items[0].thread_id, fork.thread.id);
        assert_eq!(
            fork_items[0].turn_id.as_deref(),
            Some(fork_turns[1].id.as_str())
        );
        assert_eq!(fork_items[0].content, "done");

        let events = store.read_events(&fork.thread.id, 0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "thread_created");
        assert_eq!(events[1].kind, "thread_forked");
        let JsonValue::Object(payload) = &events[1].payload else {
            panic!("fork payload should be an object");
        };
        assert_eq!(
            payload.get("source_thread_id").and_then(json_as_string),
            Some(thread.id.as_str())
        );
        assert_eq!(
            store
                .load_session(&session.id)
                .unwrap()
                .active_thread_id
                .as_deref(),
            Some(fork.thread.id.as_str())
        );
        assert_eq!(store.load_session(&session.id).unwrap().thread_count, 2);
    }

    #[test]
    fn compact_thread_writes_summary_item_and_event() {
        let store = RuntimeStore::new(temp_root("compact"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        for index in 1..=5 {
            store
                .append_turn(
                    &thread.id,
                    if index % 2 == 0 {
                        "assistant".to_string()
                    } else {
                        "user".to_string()
                    },
                    format!("turn {index} content"),
                )
                .unwrap();
        }

        let compaction = store.compact_thread(&thread.id, 2, None).unwrap();

        assert_eq!(compaction.summarized_turn_count, 3);
        assert_eq!(compaction.kept_turn_count, 2);
        assert_eq!(compaction.summary_source, "extractive");
        assert_eq!(compaction.summary_turn.role, "system");
        assert_eq!(compaction.summary_item.item_type, "summary");
        assert!(compaction
            .summary_turn
            .content
            .contains("Summarized turns: 3"));
        assert_eq!(
            store
                .load_thread(&thread.id)
                .unwrap()
                .latest_turn_id
                .as_deref(),
            Some(compaction.summary_turn.id.as_str())
        );
        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(
            events.last().map(|event| event.kind.as_str()),
            Some("thread_compacted")
        );
        assert_eq!(
            events.last().unwrap().turn_id.as_deref(),
            Some(compaction.summary_turn.id.as_str())
        );
    }

    #[test]
    fn compact_thread_records_custom_summary_source() {
        let store = RuntimeStore::new(temp_root("compact-source"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        for index in 1..=4 {
            store
                .append_turn(&thread.id, "assistant".to_string(), format!("turn {index}"))
                .unwrap();
        }

        let compaction = store
            .compact_thread_with_summary_source(
                &thread.id,
                2,
                "Model summary with decisions".to_string(),
                "model",
            )
            .unwrap();

        assert_eq!(compaction.summary_source, "model");
        assert_eq!(
            compaction.summary_turn.content,
            "Model summary with decisions"
        );
        let events = store.read_events(&thread.id, 0).unwrap();
        let payload = match &events.last().unwrap().payload {
            JsonValue::Object(payload) => payload,
            _ => panic!("expected object payload"),
        };
        assert_eq!(
            payload.get("summary_source").and_then(json_as_string),
            Some("model")
        );
    }

    #[test]
    fn recent_reasoning_replay_entries_reads_persisted_reasoning_items() {
        let store = RuntimeStore::new(temp_root("reasoning-replay"));
        let thread = store
            .create_thread(
                "Reasoning replay".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let old_turn = store
            .append_turn(&thread.id, "assistant".to_string(), "old".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&old_turn.id),
                "reasoning".to_string(),
                Some("assistant".to_string()),
                "old reasoning".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let latest_turn = store
            .append_turn(&thread.id, "assistant".to_string(), "latest".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&latest_turn.id),
                "reasoning".to_string(),
                Some("assistant".to_string()),
                "latest reasoning\nwith details".to_string(),
                "completed".to_string(),
            )
            .unwrap();

        let entries = store
            .recent_reasoning_replay_entries(&thread.id, 2)
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("old reasoning"));
        assert!(entries[1].contains("latest reasoning with details"));
        assert!(entries[1].contains(&latest_turn.id));
    }

    #[test]
    fn reasoning_replay_entries_include_pinned_turns_beyond_latest_limit() {
        let store = RuntimeStore::new(temp_root("reasoning-replay-pins"));
        let thread = store
            .create_thread(
                "Reasoning replay pins".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let pinned_turn = store
            .append_turn(&thread.id, "assistant".to_string(), "pinned".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&pinned_turn.id),
                "reasoning".to_string(),
                Some("assistant".to_string()),
                "pinned old reasoning".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        for index in 1..=3 {
            let turn = store
                .append_turn(
                    &thread.id,
                    "assistant".to_string(),
                    format!("latest {index}"),
                )
                .unwrap();
            store
                .append_item(
                    &thread.id,
                    Some(&turn.id),
                    "reasoning".to_string(),
                    Some("assistant".to_string()),
                    format!("latest reasoning {index}"),
                    "completed".to_string(),
                )
                .unwrap();
        }

        let entries = store
            .reasoning_replay_entries_with_pinned_turns(
                &thread.id,
                1,
                std::slice::from_ref(&pinned_turn.id),
            )
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("pinned old reasoning"));
        assert!(entries[0].contains(&pinned_turn.id));
        assert!(entries[1].contains("latest reasoning 3"));
    }

    #[test]
    fn update_turn_and_item_record_events_for_streaming_runtime_state() {
        let store = RuntimeStore::new(temp_root("streaming-item"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "starting".to_string())
            .unwrap();
        let item = store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "starting".to_string(),
                "running".to_string(),
            )
            .unwrap();

        let updated_turn = store
            .update_turn(
                &thread.id,
                &turn.id,
                "streamed final".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let updated_item = store
            .update_item(
                &thread.id,
                &item.id,
                "streamed final".to_string(),
                "completed".to_string(),
            )
            .unwrap();

        assert_eq!(updated_turn.content, "streamed final");
        assert_eq!(updated_item.status, "completed");
        let events = store.read_events(&thread.id, 3).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "turn_updated");
        assert_eq!(events[1].kind, "item_updated");
        assert_eq!(store.load_thread(&thread.id).unwrap().event_seq, 5);
    }

    #[test]
    fn append_permission_request_updates_thread_and_event_stream() {
        let store = RuntimeStore::new(temp_root("permission"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "checking".to_string())
            .unwrap();
        let mut input = BTreeMap::new();
        input.insert("command".to_string(), "cargo test".to_string());

        let request = store
            .append_permission_request(
                &thread.id,
                Some(&turn.id),
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo test".to_string(),
                input,
            )
            .unwrap();

        assert_eq!(request.kind, "permission_request");
        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2].id, request.id);
        assert_eq!(events[2].turn_id.as_deref(), Some(turn.id.as_str()));
        let JsonValue::Object(payload) = &events[2].payload else {
            panic!("permission request payload should be an object");
        };
        assert_eq!(
            payload.get("tool").and_then(json_as_string),
            Some("run_shell")
        );
        assert_eq!(
            payload.get("target").and_then(json_as_string),
            Some("cargo test")
        );
        assert_eq!(
            store.load_thread(&thread.id).unwrap().event_seq,
            request.seq
        );
        assert_eq!(
            store
                .load_session(&session.id)
                .unwrap()
                .active_thread_id
                .as_deref(),
            Some(thread.id.as_str())
        );
    }

    #[test]
    fn append_permission_response_records_decision_event() {
        let store = RuntimeStore::new(temp_root("permission-response"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo test".to_string(),
                BTreeMap::new(),
            )
            .unwrap();

        let response = store
            .append_permission_response(
                &thread.id,
                None,
                request.id.clone(),
                "approved".to_string(),
            )
            .unwrap();

        assert_eq!(response.kind, "permission_response");
        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.len(), 3);
        let JsonValue::Object(payload) = &events[2].payload else {
            panic!("permission response payload should be an object");
        };
        assert_eq!(
            payload.get("request_id").and_then(json_as_string),
            Some(request.id.as_str())
        );
        assert_eq!(
            payload.get("decision").and_then(json_as_string),
            Some("approved")
        );
        assert_eq!(
            store.load_thread(&thread.id).unwrap().event_seq,
            response.seq
        );
    }

    #[test]
    fn append_user_input_request_and_response_records_events() {
        let store = RuntimeStore::new(temp_root("user-input"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let questions = JsonValue::Array(vec![JsonValue::Object(object([
            ("header", JsonValue::String("Mode".to_string())),
            ("id", JsonValue::String("mode".to_string())),
            (
                "question",
                JsonValue::String("Which mode should be used?".to_string()),
            ),
            (
                "options",
                JsonValue::Array(vec![
                    JsonValue::Object(object([
                        ("label", JsonValue::String("Plan".to_string())),
                        (
                            "description",
                            JsonValue::String("Plan the change first.".to_string()),
                        ),
                    ])),
                    JsonValue::Object(object([
                        ("label", JsonValue::String("Apply".to_string())),
                        (
                            "description",
                            JsonValue::String("Implement directly.".to_string()),
                        ),
                    ])),
                ]),
            ),
        ]))]);

        let request = store
            .append_user_input_request(&thread.id, None, questions)
            .unwrap();
        let mut answers = BTreeMap::new();
        answers.insert("mode".to_string(), "Plan".to_string());
        let response = store
            .append_user_input_response(&thread.id, None, request.id.clone(), answers)
            .unwrap();

        assert_eq!(request.kind, "user_input_request");
        assert_eq!(response.kind, "user_input_response");
        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].id, request.id);
        let JsonValue::Object(payload) = &events[2].payload else {
            panic!("user input response payload should be an object");
        };
        assert_eq!(
            payload.get("request_id").and_then(json_as_string),
            Some(request.id.as_str())
        );
        let answers = payload
            .get("answers")
            .and_then(|value| match value {
                JsonValue::Object(map) => Some(map),
                _ => None,
            })
            .expect("answers object");
        assert_eq!(answers.get("mode").and_then(json_as_string), Some("Plan"));
        assert_eq!(
            store.load_thread(&thread.id).unwrap().event_seq,
            response.seq
        );
    }

    #[test]
    fn append_cancel_request_records_thread_event() {
        let store = RuntimeStore::new(temp_root("cancel-request"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "running".to_string())
            .unwrap();

        let event = store
            .append_cancel_request(
                &thread.id,
                Some(&turn.id),
                None,
                "user requested cancellation".to_string(),
            )
            .unwrap();

        assert_eq!(event.kind, "cancel_requested");
        assert_eq!(event.turn_id.as_deref(), Some(turn.id.as_str()));
        let JsonValue::Object(payload) = &event.payload else {
            panic!("cancel request payload should be an object");
        };
        assert_eq!(
            payload.get("reason").and_then(json_as_string),
            Some("user requested cancellation")
        );
        assert_eq!(store.load_thread(&thread.id).unwrap().event_seq, event.seq);
    }

    #[test]
    fn cancel_task_marks_task_and_records_cancel_event() {
        let store = RuntimeStore::new(temp_root("task-cancel"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                None,
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "running task".to_string(),
            )
            .unwrap();

        let (cancelled, event) = store
            .cancel_task(&task.id, "stop it".to_string())
            .expect("task should cancel");

        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(cancelled.summary, "stop it");
        let event = event.expect("thread-linked task should create cancel event");
        assert_eq!(event.kind, "cancel_requested");
        let JsonValue::Object(payload) = &event.payload else {
            panic!("cancel request payload should be an object");
        };
        assert_eq!(
            payload.get("task_id").and_then(json_as_string),
            Some(task.id.as_str())
        );
        assert_eq!(
            payload.get("reason").and_then(json_as_string),
            Some("stop it")
        );
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events.iter().any(|event| event.kind == "task_updated"));
        assert!(events.iter().any(|event| event.kind == "cancel_requested"));
    }

    #[test]
    fn pause_and_resume_task_updates_pending_queue_state() {
        let store = RuntimeStore::new(temp_root("task-pause-resume"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                None,
                Some(&thread.id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                "queued task".to_string(),
            )
            .unwrap();

        let paused = store
            .pause_task(&task.id, Some("paused for review".to_string()))
            .expect("pending task should pause");
        assert_eq!(paused.status, "paused");
        assert_eq!(paused.summary, "paused for review");
        let claim_error = store
            .claim_task(&task.id, "local-runner".to_string())
            .expect_err("paused task should not be claimable");
        assert!(claim_error.to_string().contains("cannot be claimed"));

        let resumed = store
            .resume_task(&task.id, None)
            .expect("paused task should resume");
        assert_eq!(resumed.status, "pending");
        assert_eq!(resumed.summary, "paused for review");
        let events = store.read_events(&thread.id, 0).unwrap();
        let task_updates = events
            .iter()
            .filter(|event| event.kind == "task_updated")
            .count();
        assert_eq!(task_updates, 2);
    }

    #[test]
    fn creates_session_and_links_threads() {
        let store = RuntimeStore::new(temp_root("session"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        assert_eq!(thread.session_id.as_deref(), Some(session.id.as_str()));
        let loaded = store.load_session(&session.id).unwrap();
        assert_eq!(loaded.active_thread_id.as_deref(), Some(thread.id.as_str()));
        assert_eq!(loaded.thread_count, 1);
        let session_threads = store.list_session_threads(&session.id, 10).unwrap();
        assert_eq!(session_threads.len(), 1);
        assert_eq!(session_threads[0].id, thread.id);
        assert_eq!(store.list_sessions(10).unwrap().len(), 1);
    }

    #[test]
    fn creates_task_and_links_thread_event() {
        let store = RuntimeStore::new(temp_root("task"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "exec".to_string(),
                "completed".to_string(),
                "finished investigation".to_string(),
            )
            .unwrap();

        assert_eq!(task.session_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(task.thread_id.as_deref(), Some(thread.id.as_str()));
        let tasks = store
            .list_tasks(Some(&session.id), Some(&thread.id), 10)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "completed");
        assert_eq!(store.load_thread(&thread.id).unwrap().event_seq, 2);
        let events = store.read_events(&thread.id, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "task_recorded");
    }

    #[test]
    fn update_task_records_status_event() {
        let store = RuntimeStore::new(temp_root("task-update"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "running investigation".to_string(),
            )
            .unwrap();

        let updated = store
            .update_task(
                &task.id,
                "cancelled".to_string(),
                "cancelled by user".to_string(),
            )
            .unwrap();

        assert_eq!(updated.status, "cancelled");
        assert_eq!(updated.summary, "cancelled by user");
        let events = store.read_events(&thread.id, 2).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "task_updated");
        let JsonValue::Object(payload) = &events[0].payload else {
            panic!("task update payload should be an object");
        };
        assert_eq!(
            payload.get("status").and_then(json_as_string),
            Some("cancelled")
        );
    }

    #[test]
    fn claim_task_marks_pending_task_running_and_records_event() {
        let store = RuntimeStore::new(temp_root("task-claim"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                "run queued work".to_string(),
            )
            .unwrap();

        let claimed = store
            .claim_task(&task.id, "local-runner".to_string())
            .unwrap();

        assert_eq!(claimed.status, "running");
        let loaded = store.load_task(&task.id).unwrap();
        assert_eq!(loaded.status, "running");
        let events = store.read_events(&thread.id, 2).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "task_claimed");
        let JsonValue::Object(payload) = &events[0].payload else {
            panic!("task claim payload should be an object");
        };
        assert_eq!(
            payload.get("runner_id").and_then(json_as_string),
            Some("local-runner")
        );
    }

    #[test]
    fn claim_task_rejects_non_pending_tasks() {
        let store = RuntimeStore::new(temp_root("task-claim-non-pending"));
        let task = store
            .create_task(
                None,
                None,
                None,
                "agent".to_string(),
                "completed".to_string(),
                "already done".to_string(),
            )
            .unwrap();

        let error = store
            .claim_task(&task.id, "local-runner".to_string())
            .expect_err("completed task should not be claimable");

        assert!(error.to_string().contains("cannot be claimed"));
    }

    #[test]
    fn creates_automation_and_links_thread_event() {
        let store = RuntimeStore::new(temp_root("automation"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let automation = store
            .create_automation(
                Some(&session.id),
                Some(&thread.id),
                "Nightly check".to_string(),
                "active".to_string(),
                "daily".to_string(),
                "run diagnostics".to_string(),
                None,
                Some("epoch+1745963600".to_string()),
            )
            .unwrap();

        assert_eq!(automation.session_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(automation.thread_id.as_deref(), Some(thread.id.as_str()));
        let automations = store
            .list_automations(Some(&session.id), Some(&thread.id), 10)
            .unwrap();
        assert_eq!(automations.len(), 1);
        assert_eq!(automations[0].status, "active");
        assert_eq!(
            automations[0].next_run_at.as_deref(),
            Some("epoch+1745963600")
        );
        assert_eq!(store.load_thread(&thread.id).unwrap().event_seq, 2);
        let events = store.read_events(&thread.id, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "automation_recorded");
    }

    #[test]
    fn trigger_automation_creates_pending_task_and_records_event() {
        let store = RuntimeStore::new(temp_root("automation-trigger"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let automation = store
            .create_automation(
                Some(&session.id),
                Some(&thread.id),
                "Nightly check".to_string(),
                "active".to_string(),
                "manual".to_string(),
                "run diagnostics".to_string(),
                None,
                None,
            )
            .unwrap();

        let (updated_automation, task) = store
            .trigger_automation(&automation.id, Some("override prompt".to_string()))
            .unwrap();

        assert!(updated_automation.last_run_at.is_some());
        assert_eq!(task.status, "pending");
        assert_eq!(task.kind, "automation");
        assert_eq!(task.summary, "override prompt");
        assert_eq!(task.thread_id.as_deref(), Some(thread.id.as_str()));
        let events = store.read_events(&thread.id, 2).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "task_recorded");
        assert_eq!(events[1].kind, "automation_triggered");
        let JsonValue::Object(payload) = &events[1].payload else {
            panic!("automation trigger payload should be an object");
        };
        assert_eq!(
            payload.get("task_id").and_then(json_as_string),
            Some(task.id.as_str())
        );
    }

    #[test]
    fn trigger_automation_rejects_paused_automation() {
        let store = RuntimeStore::new(temp_root("automation-trigger-paused"));
        let automation = store
            .create_automation(
                None,
                None,
                "Paused check".to_string(),
                "paused".to_string(),
                "manual".to_string(),
                "run diagnostics".to_string(),
                None,
                None,
            )
            .unwrap();

        let error = store
            .trigger_automation(&automation.id, None)
            .expect_err("paused automation should not trigger");

        assert!(error.to_string().contains("cannot be triggered"));
    }

    #[test]
    fn update_pause_resume_and_delete_automation_records_events() {
        let store = RuntimeStore::new(temp_root("automation-lifecycle"));
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let automation = store
            .create_automation(
                Some(&session.id),
                Some(&thread.id),
                "Nightly check".to_string(),
                "active".to_string(),
                "manual".to_string(),
                "run diagnostics".to_string(),
                None,
                None,
            )
            .unwrap();

        let updated = store
            .update_automation(
                &automation.id,
                Some("Weekly review".to_string()),
                None,
                Some("FREQ=WEEKLY;BYDAY=MO".to_string()),
                Some("summarize open work".to_string()),
                Some("epoch+999".to_string()),
            )
            .unwrap();
        assert_eq!(updated.name, "Weekly review");
        assert_eq!(updated.schedule, "FREQ=WEEKLY;BYDAY=MO");
        assert_eq!(updated.prompt, "summarize open work");
        assert_eq!(updated.next_run_at.as_deref(), Some("epoch+999"));

        let paused = store.pause_automation(&automation.id).unwrap();
        assert_eq!(paused.status, "paused");
        let resumed = store.resume_automation(&automation.id).unwrap();
        assert_eq!(resumed.status, "active");
        let deleted = store.delete_automation(&automation.id).unwrap();
        assert_eq!(deleted.status, "cancelled");
        assert!(store.load_automation(&automation.id).is_err());

        let events = store.read_events(&thread.id, 0).unwrap();
        let kinds = events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"automation_updated"));
        assert!(kinds.contains(&"automation_paused"));
        assert!(kinds.contains(&"automation_resumed"));
        assert!(kinds.contains(&"automation_deleted"));
    }

    #[test]
    fn append_usage_updates_thread_and_event_stream() {
        let store = RuntimeStore::new(temp_root("usage"));
        let thread = store
            .create_thread(
                "Investigate".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();

        let usage = store
            .append_usage_with_cache(
                &thread.id,
                Some(&turn.id),
                "deepseek-v4-flash".to_string(),
                "exec".to_string(),
                12,
                3,
                7,
                5,
            )
            .unwrap();

        assert_eq!(usage.total_tokens, 15);
        assert_eq!(usage.prompt_cache_hit_tokens, 7);
        assert_eq!(usage.prompt_cache_miss_tokens, 5);
        assert_eq!(usage.estimated_total_cost_microusd, Some(2));
        let loaded = store.load_thread(&thread.id).unwrap();
        assert_eq!(loaded.event_seq, 3);
        let records = store.list_usage(Some(&thread.id), 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].prompt_tokens, 12);
        assert_eq!(records[0].prompt_cache_hit_tokens, 7);
        assert_eq!(store.list_usage(None, 10).unwrap().len(), 1);
        let events = store.read_events(&thread.id, 2).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "usage_recorded");
    }

    #[test]
    fn rejects_unsafe_ids() {
        assert!(validate_record_id("thread-123").is_ok());
        assert!(validate_record_id("../thread-123").is_err());
        assert!(validate_record_id(".hidden").is_err());
    }
}
