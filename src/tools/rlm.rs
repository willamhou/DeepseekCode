use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::types::AppConfig;
use crate::core::loop_runtime::{
    AgentCancelCheck, AgentRunEvents, SharedAgentCancelCheck, SharedAgentRunEvents, ToolEvent,
};
use crate::core::runtime::{RuntimeStore, TaskRecord};
use crate::error::{tool_failure, AppResult};
use crate::model::protocol::ObservationStatus;
use crate::tools::dispatch_subagent::{DispatchSubagentTool, DispatchSubagentsTool};
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::ui::stream::StreamEvents;
use crate::util::json::{
    json_as_string, json_as_u64, json_escape, json_value_to_string, parse_json_value, JsonValue,
};

const MAX_RLM_BATCH_QUESTIONS: usize = 16;
const MAX_RLM_PROCESS_CONTENT_CHARS: usize = 200_000;
const DEFAULT_RLM_CHUNK_CHARS: usize = 20_000;
const MIN_RLM_CHUNK_CHARS: usize = 1;
const MAX_RLM_CHUNK_CHARS: usize = 50_000;
const MAX_RLM_CHUNKS: usize = 256;
const DEFAULT_RLM_MAP_REDUCE_STEPS: &str = "4";
const DEFAULT_RLM_RECURSIVE_FAN_IN: usize = 8;
const MAX_RLM_RECURSIVE_FAN_IN: usize = 16;
const MAX_RLM_PYTHON_CODE_BYTES: usize = 4_000;
const DEFAULT_RLM_PYTHON_TIMEOUT_MS: u64 = 2_000;
const MAX_RLM_PYTHON_TIMEOUT_MS: u64 = 5_000;
const MAX_RLM_MODEL_SESSION_TURNS: usize = 20;
const MAX_RLM_MODEL_SESSION_CONTEXT_TURNS: usize = 6;
const MAX_RLM_MODEL_SESSION_SUMMARY_CHARS: usize = 12_000;
const MAX_RLM_LIVE_TASK_LIST: usize = 10_000;
const MAX_RLM_LIVE_TURN_PREVIEW_CHARS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RlmPythonInterpreter {
    program: &'static str,
    args: &'static [&'static str],
}

const RLM_PYTHON_INTERPRETERS: &[RlmPythonInterpreter] = &[
    RlmPythonInterpreter {
        program: "python3",
        args: &[],
    },
    RlmPythonInterpreter {
        program: "python",
        args: &[],
    },
    RlmPythonInterpreter {
        program: "py",
        args: &["-3"],
    },
];

const RLM_PYTHON_SANDBOX: &str = r#"
import json
import math
import re
import statistics
import sys
from collections import Counter, defaultdict

payload = json.loads(sys.stdin.read() or "{}")
code = payload.get("code", "")
blocked = [
    "__", "import ", "from ", "open(", "exec(", "eval(", "compile(",
    "globals(", "locals(", "input(", "breakpoint(", "help(",
    "dir(", "getattr(", "setattr(", "delattr(", "type(", "super(",
    "object", "class ", "subprocess", "socket", "pathlib", "requests",
    "os.", "sys.",
]
lower = code.lower()
for token in blocked:
    if token in lower:
        print(json.dumps({"ok": False, "error": "blocked token: " + token}))
        sys.exit(1)

_context_value = payload.get("context", "")
_state_value = payload.get("state", {})
if not isinstance(_state_value, dict):
    _state_value = {}
_stdout = []
_stdout_len = 0
_max_stdout = 12000

def _safe_print(*args, sep=" ", end="\n"):
    global _stdout_len
    text = sep.join(str(arg) for arg in args) + end
    _stdout_len += len(text)
    if _stdout_len > _max_stdout:
        raise RuntimeError("stdout limit exceeded")
    _stdout.append(text)

def chunk_context(max_chars=20000, overlap=0):
    max_chars = int(max_chars)
    overlap = max(0, int(overlap))
    if max_chars <= 0:
        raise ValueError("max_chars must be > 0")
    if overlap >= max_chars:
        raise ValueError("overlap must be smaller than max_chars")
    chunks = []
    start = 0
    idx = 0
    total = len(_context_value)
    while start < total:
        end = min(total, start + max_chars)
        chunks.append({"index": idx, "start": start, "end": end, "text": _context_value[start:end]})
        idx += 1
        if end >= total:
            break
        start = end - overlap
    return chunks

def chunk_coverage(chunks):
    spans = []
    for chunk in chunks:
        try:
            spans.append((int(chunk["start"]), int(chunk["end"])))
        except Exception:
            continue
    spans.sort()
    covered = 0
    cursor = 0
    gaps = []
    for start, end in spans:
        if start > cursor:
            gaps.append([cursor, start])
        if end > cursor:
            covered += end - max(start, cursor)
            cursor = end
    if cursor < len(_context_value):
        gaps.append([cursor, len(_context_value)])
    return {
        "chunks": len(chunks),
        "context_chars": len(_context_value),
        "covered_chars": covered,
        "gaps": gaps,
        "complete": covered >= len(_context_value) and not gaps,
    }

_helper_names = {
    "__builtins__", "Counter", "defaultdict", "chunk_context", "chunk_coverage",
    "SHOW_VARS", "repl_get", "repl_set", "FINAL", "FINAL_VAR",
    "math", "re", "statistics", "context", "ctx", "question", "state",
}

def _caller_globals():
    return sys._getframe(2).f_globals

def SHOW_VARS():
    caller = _caller_globals()
    out = {}
    for key, value in list(caller.items()):
        if key in _helper_names or key.startswith("_") or callable(value):
            continue
        out[key] = type(value).__name__
    return out

def repl_get(name, default=None):
    key = str(name)
    caller = _caller_globals()
    if key in caller:
        return caller[key]
    return _state_value.get(key, default)

def repl_set(name, value):
    key = str(name)
    caller = _caller_globals()
    caller[key] = value
    _state_value[key] = value
    return value

def _final_for(caller, value):
    caller["final"] = str(value)
    _safe_print(str(value))
    return str(value)

def FINAL(value):
    return _final_for(_caller_globals(), value)

def FINAL_VAR(name):
    key = str(name).strip().strip("'\"")
    caller = _caller_globals()
    if key in caller:
        return _final_for(caller, caller[key])
    if key in _state_value:
        return _final_for(caller, _state_value[key])
    _safe_print("FINAL_VAR error: variable '" + key + "' not found")
    return ""

def _is_repl_local_name(name):
    return (
        isinstance(name, str)
        and name.isidentifier()
        and not name.startswith("_")
        and name not in _helper_names
    )

def _json_signature(value):
    try:
        return json.dumps(value, sort_keys=True)
    except TypeError:
        return None

safe_builtins = {
    "abs": abs,
    "all": all,
    "any": any,
    "bool": bool,
    "dict": dict,
    "enumerate": enumerate,
    "filter": filter,
    "float": float,
    "int": int,
    "len": len,
    "list": list,
    "map": map,
    "max": max,
    "min": min,
    "pow": pow,
    "print": _safe_print,
    "range": range,
    "reversed": reversed,
    "round": round,
    "set": set,
    "sorted": sorted,
    "str": str,
    "sum": sum,
    "tuple": tuple,
    "zip": zip,
}
env = {
    "__builtins__": safe_builtins,
    "Counter": Counter,
    "defaultdict": defaultdict,
    "chunk_context": chunk_context,
    "chunk_coverage": chunk_coverage,
    "SHOW_VARS": SHOW_VARS,
    "repl_get": repl_get,
    "repl_set": repl_set,
    "FINAL": FINAL,
    "FINAL_VAR": FINAL_VAR,
    "math": math,
    "re": re,
    "statistics": statistics,
    "context": _context_value,
    "ctx": _context_value,
    "question": payload.get("question", ""),
    "state": _state_value,
}
initial_keys = set(env.keys())
_injected_local_names = set()
_injected_local_signatures = {}
if isinstance(_state_value, dict):
    for key, value in list(_state_value.items()):
        if _is_repl_local_name(key):
            env[key] = value
            _injected_local_names.add(key)
            _injected_local_signatures[key] = _json_signature(value)
try:
    exec(compile(code, "<rlm_python>", "exec"), env, env)
except Exception as exc:
    print(json.dumps({"ok": False, "error": str(exc), "stdout": "".join(_stdout)}, ensure_ascii=False))
    sys.exit(1)

variables = {}
for key, value in env.items():
    if key in initial_keys or key.startswith("_") or callable(value):
        continue
    try:
        json.dumps(value)
        variables[key] = value
    except TypeError:
        variables[key] = repr(value)
state_value = env.get("state", {})
if isinstance(state_value, dict):
    for key in list(_injected_local_names):
        if key not in env:
            state_value.pop(key, None)
    for key, value in variables.items():
        if _is_repl_local_name(key):
            if key in _injected_local_names:
                before = _injected_local_signatures.get(key)
                local_after = _json_signature(env.get(key))
                state_after = _json_signature(state_value.get(key))
                if local_after == before and state_after != before:
                    continue
            state_value[key] = value
try:
    json.dumps(state_value)
except TypeError:
    state_value = repr(state_value)
print(json.dumps({"ok": True, "stdout": "".join(_stdout), "variables": variables, "state": state_value}, ensure_ascii=False))
"#;

const RLM_PYTHON_REPL_SANDBOX: &str = r#"
import json
import math
import os as _os
import re
import signal
import statistics
import sys
from collections import Counter, defaultdict

blocked = [
    "__", "import ", "from ", "open(", "exec(", "eval(", "compile(",
    "globals(", "locals(", "input(", "breakpoint(", "help(",
    "dir(", "getattr(", "setattr(", "delattr(", "type(", "super(",
    "object", "class ", "subprocess", "socket", "pathlib", "requests",
    "os.", "sys.",
]

_context_value = ""
_state_value = {}
_stdout = []
_stdout_len = 0
_max_stdout = 12000

_helper_names = {
    "__builtins__", "Counter", "defaultdict", "chunk_context", "chunk_coverage",
    "SHOW_VARS", "repl_get", "repl_set", "FINAL", "FINAL_VAR",
    "math", "re", "statistics", "context", "ctx", "question", "state",
}

def _is_repl_local_name(name):
    return (
        isinstance(name, str)
        and name.isidentifier()
        and not name.startswith("_")
        and name not in _helper_names
    )

def _json_signature(value):
    try:
        return json.dumps(value, sort_keys=True)
    except TypeError:
        return None

def _safe_print(*args, sep=" ", end="\n"):
    global _stdout_len
    text = sep.join(str(arg) for arg in args) + end
    _stdout_len += len(text)
    if _stdout_len > _max_stdout:
        raise RuntimeError("stdout limit exceeded")
    _stdout.append(text)

def chunk_context(max_chars=20000, overlap=0):
    max_chars = int(max_chars)
    overlap = max(0, int(overlap))
    if max_chars <= 0:
        raise ValueError("max_chars must be > 0")
    if overlap >= max_chars:
        raise ValueError("overlap must be smaller than max_chars")
    chunks = []
    start = 0
    idx = 0
    total = len(_context_value)
    while start < total:
        end = min(total, start + max_chars)
        chunks.append({"index": idx, "start": start, "end": end, "text": _context_value[start:end]})
        idx += 1
        if end >= total:
            break
        start = end - overlap
    return chunks

def chunk_coverage(chunks):
    spans = []
    for chunk in chunks:
        try:
            spans.append((int(chunk["start"]), int(chunk["end"])))
        except Exception:
            continue
    spans.sort()
    covered = 0
    cursor = 0
    gaps = []
    for start, end in spans:
        if start > cursor:
            gaps.append([cursor, start])
        if end > cursor:
            covered += end - max(start, cursor)
            cursor = end
    if cursor < len(_context_value):
        gaps.append([cursor, len(_context_value)])
    return {
        "chunks": len(chunks),
        "context_chars": len(_context_value),
        "covered_chars": covered,
        "gaps": gaps,
        "complete": covered >= len(_context_value) and not gaps,
    }

def _caller_globals():
    return sys._getframe(2).f_globals

def SHOW_VARS():
    caller = _caller_globals()
    out = {}
    for key, value in list(caller.items()):
        if key in _helper_names or key.startswith("_") or callable(value):
            continue
        out[key] = type(value).__name__
    return out

def repl_get(name, default=None):
    key = str(name)
    caller = _caller_globals()
    if key in caller:
        return caller[key]
    return _state_value.get(key, default)

def repl_set(name, value):
    key = str(name)
    caller = _caller_globals()
    caller[key] = value
    _state_value[key] = value
    return value

def _final_for(caller, value):
    caller["final"] = str(value)
    _safe_print(str(value))
    return str(value)

def FINAL(value):
    return _final_for(_caller_globals(), value)

def FINAL_VAR(name):
    key = str(name).strip().strip("'\"")
    caller = _caller_globals()
    if key in caller:
        return _final_for(caller, caller[key])
    if key in _state_value:
        return _final_for(caller, _state_value[key])
    _safe_print("FINAL_VAR error: variable '" + key + "' not found")
    return ""

safe_builtins = {
    "abs": abs,
    "all": all,
    "any": any,
    "bool": bool,
    "dict": dict,
    "enumerate": enumerate,
    "filter": filter,
    "float": float,
    "int": int,
    "len": len,
    "list": list,
    "map": map,
    "max": max,
    "min": min,
    "pow": pow,
    "print": _safe_print,
    "range": range,
    "reversed": reversed,
    "round": round,
    "set": set,
    "sorted": sorted,
    "str": str,
    "sum": sum,
    "tuple": tuple,
    "zip": zip,
}

_env = {
    "__builtins__": safe_builtins,
    "Counter": Counter,
    "defaultdict": defaultdict,
    "chunk_context": chunk_context,
    "chunk_coverage": chunk_coverage,
    "SHOW_VARS": SHOW_VARS,
    "repl_get": repl_get,
    "repl_set": repl_set,
    "FINAL": FINAL,
    "FINAL_VAR": FINAL_VAR,
    "math": math,
    "re": re,
    "statistics": statistics,
    "context": "",
    "ctx": "",
    "question": "",
    "state": _state_value,
}
_initial_keys = set(_env.keys())

def _timeout_handler(signum, frame):
    raise TimeoutError("rlm_python timed out")

try:
    signal.signal(signal.SIGALRM, _timeout_handler)
except Exception:
    pass

def _emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)

for raw in sys.stdin:
    try:
        payload = json.loads(raw or "{}")
    except Exception as exc:
        _emit({"ok": False, "error": "invalid payload: " + str(exc), "pid": _os.getpid(), "persistent": True})
        continue
    code = payload.get("code", "")
    lower = code.lower()
    blocked_token = None
    for token in blocked:
        if token in lower:
            blocked_token = token
            break
    if blocked_token:
        _emit({"ok": False, "error": "blocked token: " + blocked_token, "pid": _os.getpid(), "persistent": True})
        continue

    if payload.get("reset"):
        for key in list(_env.keys()):
            if key not in _initial_keys:
                _env.pop(key, None)
        _state_value.clear()
    incoming_state = payload.get("state", {})
    if isinstance(incoming_state, dict):
        _state_value.clear()
        _state_value.update(incoming_state)

    _context_value = payload.get("context", "")
    _env["context"] = _context_value
    _env["ctx"] = _context_value
    _env["question"] = payload.get("question", "")
    _env["state"] = _state_value
    _stdout = []
    _stdout_len = 0
    _injected_local_names = set()
    _injected_local_signatures = {}
    for key, value in list(_state_value.items()):
        if _is_repl_local_name(key):
            _env[key] = value
            _injected_local_names.add(key)
            _injected_local_signatures[key] = _json_signature(value)

    timeout_ms = int(payload.get("timeout_ms", 0) or 0)
    if timeout_ms > 0 and hasattr(signal, "setitimer"):
        signal.setitimer(signal.ITIMER_REAL, timeout_ms / 1000.0)
    try:
        exec(compile(code, "<rlm_python_repl>", "exec"), _env, _env)
        variables = {}
        for key, value in _env.items():
            if key in _initial_keys or key.startswith("_") or callable(value):
                continue
            try:
                json.dumps(value)
                variables[key] = value
            except TypeError:
                variables[key] = repr(value)
        for key in list(_injected_local_names):
            if key not in _env:
                _state_value.pop(key, None)
        for key, value in variables.items():
            if _is_repl_local_name(key):
                if key in _injected_local_names:
                    before = _injected_local_signatures.get(key)
                    local_after = _json_signature(_env.get(key))
                    state_after = _json_signature(_state_value.get(key))
                    if local_after == before and state_after != before:
                        continue
                _state_value[key] = value
        _emit({"ok": True, "stdout": "".join(_stdout), "variables": variables, "state": _state_value, "pid": _os.getpid(), "persistent": True})
    except Exception as exc:
        _emit({"ok": False, "error": str(exc), "stdout": "".join(_stdout), "pid": _os.getpid(), "persistent": True})
    finally:
        if hasattr(signal, "setitimer"):
            try:
                signal.setitimer(signal.ITIMER_REAL, 0)
            except Exception:
                pass
"#;

struct RlmPythonReplProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for RlmPythonReplProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static RLM_PYTHON_REPLS: OnceLock<Mutex<HashMap<String, RlmPythonReplProcess>>> = OnceLock::new();

pub struct RlmTool {
    pub tool_name: &'static str,
    pub config: AppConfig,
    pub parent_depth: usize,
}

impl Tool for RlmTool {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        if input_has_rlm_process_shape(&input) {
            return self.execute_process_input(input);
        }

        let tool_name = self.name();
        let context = input
            .get("context")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure(format!("{tool_name} requires non-empty `context`")))?;
        let question = input
            .get("question")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure(format!("{tool_name} requires non-empty `question`")))?;
        let strategy = input
            .get("strategy")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("synthesize");
        let steps = input
            .get("steps")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("4");
        let task = render_rlm_task(context, question, strategy);
        let child_input = ToolInput::new()
            .with_arg("task", task)
            .with_arg("steps", steps.to_string());
        DispatchSubagentTool {
            config: self.config.clone(),
            parent_depth: self.parent_depth,
        }
        .execute(child_input)
    }
}

impl RlmTool {
    fn execute_process_input(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let tool_name = self.name();
        let task = input
            .get("task")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure(format!("{tool_name} requires non-empty `task`")))?;
        let steps = input
            .get("steps")
            .or_else(|| input.get("max_depth"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("6");
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let reset_session = parse_bool_arg(input.get("reset"));
        if parse_bool_arg(input.get("live")) {
            return self.enqueue_live_process_turn(&input, task, steps, session_id, reset_session);
        }
        let mut session = match session_id {
            Some(session_id) => {
                validate_rlm_model_session_id(session_id)?;
                Some(read_rlm_model_session(
                    &self.config,
                    session_id,
                    reset_session,
                )?)
            }
            None => None,
        };
        let process_input = load_rlm_process_input_or_session_context(&input, session.as_ref())?;
        let child_task =
            render_rlm_process_task_with_session(task, &process_input, session.as_ref());
        let child_input = ToolInput::new()
            .with_arg("task", child_task)
            .with_arg("steps", steps.to_string());
        let mut output = DispatchSubagentTool {
            config: self.config.clone(),
            parent_depth: self.parent_depth,
        }
        .execute(child_input)?;
        if let Some(session) = session.as_mut() {
            append_rlm_model_session_turn(session, task, &process_input, &output.summary);
            write_rlm_model_session(&self.config, session)?;
            output.summary = format!(
                "meta.rlm_session_id={}\nmeta.rlm_session_turns={}\n{}",
                session.session_id,
                session.turns.len(),
                output.summary
            );
        }
        Ok(output)
    }

    fn enqueue_live_process_turn(
        &self,
        input: &ToolInput,
        task: &str,
        steps: &str,
        session_id: Option<&str>,
        reset_session: bool,
    ) -> AppResult<ToolOutput> {
        let session_id = session_id
            .ok_or_else(|| tool_failure("rlm_process live=true requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let manifest_path = rlm_live_session_manifest_path(&self.config, session_id);
        let live_exists = manifest_path.exists() && !reset_session;
        if !rlm_process_has_input_source(input) && !live_exists {
            return Err(tool_failure(
                "rlm_process live=true requires file_path/content for a new live session or an existing live session_id",
            ));
        }
        let process_input = if rlm_process_has_input_source(input) {
            load_rlm_process_input(input)?
        } else {
            RlmProcessInput {
                label: "live session context only".to_string(),
                content: String::new(),
                char_count: 0,
                line_count: 0,
            }
        };
        let store = rlm_runtime_store(&self.config);
        let model = input
            .get("model")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.config.model.model)
            .to_string();
        let mode = input
            .get("mode")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("agent")
            .to_string();
        let workspace = input
            .get("cwd")
            .or_else(|| input.get("workspace"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(default_rlm_workspace);
        let existing = if live_exists {
            read_rlm_live_session_manifest(&manifest_path, session_id).ok()
        } else {
            None
        };
        if existing
            .as_ref()
            .and_then(|manifest| rlm_live_manifest_string_field(manifest, "status"))
            .as_deref()
            == Some("stopped")
        {
            return Err(tool_failure(
                "rlm_process live session is stopped; pass reset=true to start a new live session with the same session_id",
            ));
        }
        let existing_thread_id = existing
            .as_ref()
            .and_then(|manifest| rlm_live_manifest_string_field(manifest, "runtime_thread_id"));
        let thread = match existing_thread_id
            .as_deref()
            .and_then(|thread_id| store.load_thread(thread_id).ok())
        {
            Some(thread) => thread,
            None => store.create_thread(
                format!("RLM live session {session_id}"),
                workspace.clone(),
                model.clone(),
                mode,
            )?,
        };
        let summary = format!(
            "RLM live turn: {task}\ninput: {}\ninput_chars: {}\ninput_lines: {}",
            process_input.label, process_input.char_count, process_input.line_count
        );
        let runtime_task = store.create_task(
            thread.session_id.as_deref(),
            Some(&thread.id),
            None,
            "rlm_process".to_string(),
            "pending".to_string(),
            summary,
        )?;
        write_rlm_live_session_turn_payload(
            &self.config,
            session_id,
            &thread.id,
            &runtime_task.id,
            task,
            &process_input,
            steps,
            &model,
            &workspace,
        )?;
        let previous_queued = existing
            .as_ref()
            .and_then(|manifest| rlm_live_manifest_u64_field(manifest, "queued_turns"))
            .unwrap_or(0);
        let queued_turns = previous_queued.saturating_add(1);
        write_rlm_live_session_manifest(
            &self.config,
            session_id,
            "idle",
            &thread.id,
            thread.session_id.as_deref(),
            None,
            queued_turns,
            &thread.model,
            &thread.workspace,
            existing
                .as_ref()
                .and_then(|manifest| rlm_live_manifest_string_field(manifest, "created_at"))
                .as_deref(),
        )?;
        append_rlm_live_session_event(
            &self.config,
            session_id,
            "turn_queued",
            &thread.id,
            &runtime_task.id,
            task,
            &process_input.label,
        )?;
        Ok(ToolOutput {
            summary: format!(
                "meta.rlm_live=true\nmeta.rlm_session_id={session_id}\nmeta.rlm_runtime_thread_id={}\nmeta.rlm_turn_id={}\nmeta.rlm_status=queued\nmeta.rlm_queued_turns={queued_turns}\nstatus: queued\ninput: {}\n",
                thread.id, runtime_task.id, process_input.label
            ),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmProcessInput {
    label: String,
    content: String,
    char_count: usize,
    line_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmModelSession {
    session_id: String,
    updated_at: String,
    turns: Vec<RlmModelSessionTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmModelSessionTurn {
    task: String,
    label: String,
    char_count: usize,
    line_count: usize,
    summary: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmLiveTurnPayload {
    session_id: String,
    runtime_thread_id: String,
    task_id: String,
    status: String,
    task: String,
    steps: String,
    model: String,
    workspace: String,
    input: RlmProcessInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmLiveEventBatch {
    path: PathBuf,
    exists: bool,
    next_cursor: u64,
    events_json: String,
    count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmLiveDaemonOwner {
    pid: Option<u64>,
    alive: Option<bool>,
    stale: bool,
    owner: &'static str,
}

struct RlmLiveTaskCancelCheck {
    store: RuntimeStore,
    task_id: String,
}

impl AgentCancelCheck for RlmLiveTaskCancelCheck {
    fn is_cancelled(&mut self) -> AppResult<bool> {
        Ok(self
            .store
            .load_task(&self.task_id)
            .map(|task| task.status == "cancelled")
            .unwrap_or(false))
    }
}

#[derive(Clone)]
struct RlmLiveWorkerEventTarget {
    config: AppConfig,
    session_id: String,
    runtime_thread_id: String,
    task_id: String,
}

struct RlmLiveWorkerStreamEvents {
    target: RlmLiveWorkerEventTarget,
}

impl StreamEvents for RlmLiveWorkerStreamEvents {
    fn on_reasoning_delta(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let _ = append_rlm_live_session_json_event(
            &self.target.config,
            &self.target.session_id,
            "worker_reasoning_delta",
            &self.target.runtime_thread_id,
            &self.target.task_id,
            vec![
                ("delta".to_string(), JsonValue::String(chunk.to_string())),
                (
                    "delta_chars".to_string(),
                    JsonValue::Number(chunk.chars().count().to_string()),
                ),
            ],
        );
    }

    fn on_text_delta(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let _ = append_rlm_live_session_json_event(
            &self.target.config,
            &self.target.session_id,
            "worker_text_delta",
            &self.target.runtime_thread_id,
            &self.target.task_id,
            vec![
                ("delta".to_string(), JsonValue::String(chunk.to_string())),
                (
                    "delta_chars".to_string(),
                    JsonValue::Number(chunk.chars().count().to_string()),
                ),
            ],
        );
    }

    fn on_assistant_done(&mut self, full_text: &str) {
        let (preview, chars, truncated) = rlm_preview_json(Some(full_text));
        let _ = append_rlm_live_session_json_event(
            &self.target.config,
            &self.target.session_id,
            "worker_assistant_done",
            &self.target.runtime_thread_id,
            &self.target.task_id,
            vec![
                ("text_preview".to_string(), preview),
                ("text_chars".to_string(), chars),
                ("text_truncated".to_string(), truncated),
            ],
        );
    }

    fn on_tool_call(&mut self, name: &str, input: &BTreeMap<String, String>) {
        let _ = append_rlm_live_session_json_event(
            &self.target.config,
            &self.target.session_id,
            "worker_model_tool_call",
            &self.target.runtime_thread_id,
            &self.target.task_id,
            vec![
                ("tool_name".to_string(), JsonValue::String(name.to_string())),
                ("input".to_string(), rlm_string_map_json(input)),
            ],
        );
    }
}

struct RlmLiveWorkerRunEvents {
    target: RlmLiveWorkerEventTarget,
}

impl AgentRunEvents for RlmLiveWorkerRunEvents {
    fn on_tool_call(&mut self, tool_name: &str, input: &BTreeMap<String, String>) {
        let _ = append_rlm_live_session_json_event(
            &self.target.config,
            &self.target.session_id,
            "worker_tool_call",
            &self.target.runtime_thread_id,
            &self.target.task_id,
            vec![
                (
                    "tool_name".to_string(),
                    JsonValue::String(tool_name.to_string()),
                ),
                ("input".to_string(), rlm_string_map_json(input)),
            ],
        );
    }

    fn on_permission_request(
        &mut self,
        tool_name: &str,
        input: &BTreeMap<String, String>,
        kind: &str,
        target: &str,
    ) {
        let _ = append_rlm_live_session_json_event(
            &self.target.config,
            &self.target.session_id,
            "worker_permission_request",
            &self.target.runtime_thread_id,
            &self.target.task_id,
            vec![
                (
                    "tool_name".to_string(),
                    JsonValue::String(tool_name.to_string()),
                ),
                ("input".to_string(), rlm_string_map_json(input)),
                (
                    "permission_kind".to_string(),
                    JsonValue::String(kind.to_string()),
                ),
                (
                    "permission_target".to_string(),
                    JsonValue::String(target.to_string()),
                ),
            ],
        );
    }

    fn on_tool_result(&mut self, event: &ToolEvent) {
        let (preview, chars, truncated) = rlm_preview_json(Some(&event.output));
        let _ = append_rlm_live_session_json_event(
            &self.target.config,
            &self.target.session_id,
            "worker_tool_result",
            &self.target.runtime_thread_id,
            &self.target.task_id,
            vec![
                (
                    "tool_name".to_string(),
                    JsonValue::String(event.tool_name.clone()),
                ),
                (
                    "status".to_string(),
                    JsonValue::String(rlm_observation_status_label(event.status).to_string()),
                ),
                ("input".to_string(), rlm_string_map_json(&event.input)),
                ("output_preview".to_string(), preview),
                ("output_chars".to_string(), chars),
                ("output_truncated".to_string(), truncated),
            ],
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RlmLiveRecoveryMode {
    Requeue,
    Fail,
}

impl RlmLiveRecoveryMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Requeue => "requeue",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmChunk {
    index: usize,
    start: usize,
    end: usize,
    text: String,
}

pub struct RlmBatchTool {
    pub tool_name: &'static str,
    pub config: AppConfig,
    pub parent_depth: usize,
}

impl Tool for RlmBatchTool {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let tool_name = self.name();
        let context = input
            .get("context")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure(format!("{tool_name} requires non-empty `context`")))?;
        let raw_questions = input
            .get("questions")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure(format!("{tool_name} requires non-empty `questions`")))?;
        let strategy = input
            .get("strategy")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("batch_synthesize");
        let steps = input
            .get("steps")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("4");
        let questions = parse_rlm_batch_questions(raw_questions)?;
        let tasks = render_rlm_batch_tasks(context, &questions, strategy, steps);
        let child_input = ToolInput::new().with_arg("tasks", tasks);
        DispatchSubagentsTool {
            config: self.config.clone(),
            parent_depth: self.parent_depth,
        }
        .execute(child_input)
    }
}

pub struct RlmChunkPlanTool;

impl Tool for RlmChunkPlanTool {
    fn name(&self) -> &str {
        "rlm_chunk_plan"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let process_input = load_rlm_process_input(&input)?;
        let max_chars = parse_rlm_chunk_chars(input.get("max_chars"))?;
        let overlap = parse_rlm_chunk_overlap(input.get("overlap"), max_chars)?;
        let include_text = parse_rlm_chunk_include_text(input.get("include_text"));
        Ok(ToolOutput {
            summary: render_rlm_chunk_plan(&process_input, max_chars, overlap, include_text)?,
        })
    }
}

pub struct RlmMapReducePlanTool;

impl Tool for RlmMapReducePlanTool {
    fn name(&self) -> &str {
        "rlm_map_reduce_plan"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task = input
            .get("task")
            .or_else(|| input.get("question"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_map_reduce_plan requires non-empty `task`"))?;
        let process_input = load_rlm_process_input(&input)?;
        let max_chars = parse_rlm_chunk_chars(input.get("max_chars"))?;
        let overlap = parse_rlm_chunk_overlap(input.get("overlap"), max_chars)?;
        let include_text = parse_rlm_chunk_include_text(input.get("include_text"));
        let map_limit = parse_rlm_map_limit(input.get("map_limit"))?;
        let steps = input
            .get("steps")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_RLM_MAP_REDUCE_STEPS);
        Ok(ToolOutput {
            summary: render_rlm_map_reduce_plan(
                &process_input,
                task,
                max_chars,
                overlap,
                include_text,
                map_limit,
                steps,
            )?,
        })
    }
}

pub struct RlmRecursivePlanTool;

impl Tool for RlmRecursivePlanTool {
    fn name(&self) -> &str {
        "rlm_recursive_plan"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task = input
            .get("task")
            .or_else(|| input.get("question"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_recursive_plan requires non-empty `task`"))?;
        let process_input = load_rlm_process_input(&input)?;
        let max_chars = parse_rlm_chunk_chars(input.get("max_chars"))?;
        let overlap = parse_rlm_chunk_overlap(input.get("overlap"), max_chars)?;
        let include_text = parse_rlm_chunk_include_text(input.get("include_text"));
        let map_limit = parse_rlm_map_limit(input.get("map_limit"))?;
        let fan_in = parse_rlm_recursive_fan_in(input.get("fan_in"))?;
        let steps = input
            .get("steps")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_RLM_MAP_REDUCE_STEPS);
        Ok(ToolOutput {
            summary: render_rlm_recursive_plan(
                &process_input,
                task,
                max_chars,
                overlap,
                include_text,
                map_limit,
                fan_in,
                steps,
            )?,
        })
    }
}

pub struct RlmPythonTool;

impl Tool for RlmPythonTool {
    fn name(&self) -> &str {
        "rlm_python"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let code = input
            .get("code")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_python requires non-empty `code`"))?;
        validate_rlm_python_code(code)?;
        let timeout = parse_rlm_python_timeout(input.get("timeout_ms"))?;
        let context = input.get("context").unwrap_or_default();
        let question = input.get("question").unwrap_or_default();
        let payload = rlm_python_payload(code, context, question, None);
        let summary = run_rlm_python_sandbox(&payload, timeout)?;
        Ok(ToolOutput { summary })
    }
}

pub struct RlmPythonSessionTool {
    pub config: AppConfig,
}

impl Tool for RlmPythonSessionTool {
    fn name(&self) -> &str {
        "rlm_python_session"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_python_session requires non-empty `session_id`"))?;
        validate_rlm_python_session_id(session_id)?;
        let code = input
            .get("code")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_python_session requires non-empty `code`"))?;
        validate_rlm_python_code(code)?;
        let timeout = parse_rlm_python_timeout(input.get("timeout_ms"))?;
        let context = input.get("context").unwrap_or_default();
        let question = input.get("question").unwrap_or_default();
        let reset = parse_bool_arg(input.get("reset"));
        let persistent = parse_bool_arg(input.get("persistent"));
        let state_path = rlm_python_session_path(&self.config, session_id);
        let state = if reset {
            "{}".to_string()
        } else {
            read_rlm_python_session_state(&state_path)?
        };
        let summary = if persistent {
            let payload =
                rlm_python_session_payload(code, context, question, &state, timeout, reset);
            let process_key = rlm_python_session_process_key(&self.config, session_id);
            run_rlm_python_repl_sandbox(&process_key, &payload, reset)?
        } else {
            let payload = rlm_python_payload(code, context, question, Some(&state));
            run_rlm_python_sandbox(&payload, timeout)?
        };
        let next_state = extract_rlm_python_state(&summary)?;
        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                tool_failure(format!("rlm_python_session mkdir failed: {error}"))
            })?;
        }
        fs::write(&state_path, next_state.as_bytes())
            .map_err(|error| tool_failure(format!("rlm_python_session write failed: {error}")))?;
        Ok(ToolOutput { summary })
    }
}

pub struct RlmPythonSessionsTool {
    pub config: AppConfig,
}

pub struct RlmModelSessionsTool {
    pub config: AppConfig,
}

pub struct RlmLiveStatusTool {
    pub config: AppConfig,
}

pub struct RlmLiveEventsTool {
    pub config: AppConfig,
}

pub struct RlmLiveWaitTool {
    pub config: AppConfig,
}

pub struct RlmLiveCancelTool {
    pub config: AppConfig,
}

pub struct RlmLiveRunNextTool {
    pub config: AppConfig,
    pub parent_depth: usize,
}

pub struct RlmLiveDrainTool {
    pub config: AppConfig,
    pub parent_depth: usize,
}

pub struct RlmLiveRecoverTool {
    pub config: AppConfig,
}

pub struct RlmLiveStopTool {
    pub config: AppConfig,
}

impl Tool for RlmPythonSessionsTool {
    fn name(&self) -> &str {
        "rlm_python_sessions"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        if let Some(session_id) = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            validate_rlm_python_session_id(session_id)?;
            let state_path = rlm_python_session_path(&self.config, session_id);
            let exists = state_path.exists();
            let state = read_rlm_python_session_state(&state_path)?;
            let bytes = fs::metadata(&state_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let process = rlm_python_session_process_json(&self.config, session_id)?;
            let summary = format!(
                "{{\"session_id\":\"{}\",\"path\":\"{}\",\"exists\":{},\"bytes\":{},\"state\":{},\"process\":{}}}",
                json_escape(session_id),
                json_escape(&state_path.display().to_string()),
                exists,
                bytes,
                state,
                process
            );
            return Ok(ToolOutput { summary });
        }

        let limit = parse_rlm_python_sessions_limit(input.get("limit"))?;
        let sessions_dir = rlm_python_sessions_dir(&self.config);
        let mut entries = Vec::new();
        if sessions_dir.exists() {
            for entry in fs::read_dir(&sessions_dir).map_err(|error| {
                tool_failure(format!("rlm_python_sessions read_dir failed: {error}"))
            })? {
                let entry = entry.map_err(|error| {
                    tool_failure(format!("rlm_python_sessions read entry failed: {error}"))
                })?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Some(session_id) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                if validate_rlm_python_session_id(session_id).is_err() {
                    continue;
                }
                entries.push((session_id.to_string(), path));
            }
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));

        let mut sessions_json = String::new();
        let mut errors_json = String::new();
        let mut sessions_count = 0usize;
        let mut errors_count = 0usize;
        for (session_id, path) in entries.into_iter().take(limit) {
            match read_rlm_python_session_state(&path) {
                Ok(state) => {
                    if sessions_count > 0 {
                        sessions_json.push(',');
                    }
                    let bytes = fs::metadata(&path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                    let process = rlm_python_session_process_json(&self.config, &session_id)?;
                    sessions_json.push_str(&format!(
                        "{{\"session_id\":\"{}\",\"path\":\"{}\",\"bytes\":{},\"state\":{},\"process\":{}}}",
                        json_escape(&session_id),
                        json_escape(&path.display().to_string()),
                        bytes,
                        state,
                        process
                    ));
                    sessions_count += 1;
                }
                Err(error) => {
                    if errors_count > 0 {
                        errors_json.push(',');
                    }
                    errors_json.push_str(&format!(
                        "{{\"session_id\":\"{}\",\"path\":\"{}\",\"error\":\"{}\"}}",
                        json_escape(&session_id),
                        json_escape(&path.display().to_string()),
                        json_escape(&error.to_string())
                    ));
                    errors_count += 1;
                }
            }
        }

        Ok(ToolOutput {
            summary: format!(
                "{{\"sessions\":[{}],\"errors\":[{}],\"limit\":{}}}",
                sessions_json, errors_json, limit
            ),
        })
    }
}

impl Tool for RlmModelSessionsTool {
    fn name(&self) -> &str {
        "rlm_process_sessions"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let include_turns = parse_bool_arg(input.get("include_turns"));
        let include_live = parse_bool_arg(input.get("include_live")) || include_turns;
        let limit = parse_rlm_model_sessions_limit(input.get("limit"))?;
        if let Some(session_id) = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            validate_rlm_model_session_id(session_id)?;
            let path = rlm_model_session_path(&self.config, session_id);
            let exists = path.exists();
            let session = read_rlm_model_session(&self.config, session_id, false)?;
            let bytes = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let mut summary = format!(
                "{{\"session_id\":\"{}\",\"path\":\"{}\",\"exists\":{},\"bytes\":{},\"session\":{}",
                json_escape(session_id),
                json_escape(&path.display().to_string()),
                exists,
                bytes,
                json_value_to_string(&rlm_model_session_to_json(&session))
            );
            if include_live {
                let live_path = rlm_live_session_manifest_path(&self.config, session_id);
                let live_exists = live_path.exists();
                let live_session = if live_exists {
                    json_value_to_string(&read_rlm_live_session_manifest(&live_path, session_id)?)
                } else {
                    "null".to_string()
                };
                let live_bytes = fs::metadata(&live_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                summary.push_str(&format!(
                    ",\"include_live\":true,\"live_path\":\"{}\",\"live_exists\":{},\"live_bytes\":{},\"live_session\":{}",
                    json_escape(&live_path.display().to_string()),
                    live_exists,
                    live_bytes,
                    live_session
                ));
                if include_turns {
                    summary.push_str(&format!(
                        ",\"include_turns\":true,\"live_turns\":[{}]",
                        render_rlm_live_turn_entries(&self.config, session_id, limit)?
                    ));
                }
            }
            summary.push('}');
            return Ok(ToolOutput { summary });
        }

        let sessions_dir = rlm_model_sessions_dir(&self.config);
        let mut entries = Vec::new();
        if sessions_dir.exists() {
            for entry in fs::read_dir(&sessions_dir).map_err(|error| {
                tool_failure(format!("rlm_process_sessions read_dir failed: {error}"))
            })? {
                let entry = entry.map_err(|error| {
                    tool_failure(format!("rlm_process_sessions read entry failed: {error}"))
                })?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Some(session_id) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                if validate_rlm_model_session_id(session_id).is_err() {
                    continue;
                }
                entries.push((session_id.to_string(), path));
            }
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));

        let mut sessions_json = String::new();
        let mut errors_json = String::new();
        let mut sessions_count = 0usize;
        let mut errors_count = 0usize;
        for (session_id, path) in entries.into_iter().take(limit) {
            match read_rlm_model_session(&self.config, &session_id, false) {
                Ok(session) => {
                    if sessions_count > 0 {
                        sessions_json.push(',');
                    }
                    let bytes = fs::metadata(&path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                    let last_task = session
                        .turns
                        .last()
                        .map(|turn| turn.task.as_str())
                        .unwrap_or("");
                    sessions_json.push_str(&format!(
                        "{{\"session_id\":\"{}\",\"path\":\"{}\",\"bytes\":{},\"turns\":{},\"updated_at\":\"{}\",\"last_task\":\"{}\"}}",
                        json_escape(&session_id),
                        json_escape(&path.display().to_string()),
                        bytes,
                        session.turns.len(),
                        json_escape(&session.updated_at),
                        json_escape(last_task)
                    ));
                    sessions_count += 1;
                }
                Err(error) => {
                    if errors_count > 0 {
                        errors_json.push(',');
                    }
                    errors_json.push_str(&format!(
                        "{{\"session_id\":\"{}\",\"path\":\"{}\",\"error\":\"{}\"}}",
                        json_escape(&session_id),
                        json_escape(&path.display().to_string()),
                        json_escape(&error.to_string())
                    ));
                    errors_count += 1;
                }
            }
        }

        let mut live_sessions_json = String::new();
        if include_live {
            let mut live_sessions_count = 0usize;
            for (session_id, path) in list_rlm_live_session_manifest_entries(&self.config)?
                .into_iter()
                .take(limit)
            {
                match read_rlm_live_session_manifest(&path, &session_id) {
                    Ok(manifest) => {
                        if live_sessions_count > 0 {
                            live_sessions_json.push(',');
                        }
                        let bytes = fs::metadata(&path)
                            .map(|metadata| metadata.len())
                            .unwrap_or(0);
                        live_sessions_json.push_str(&render_rlm_live_session_list_entry(
                            &session_id,
                            &path,
                            bytes,
                            &manifest,
                            include_turns,
                            limit,
                            &self.config,
                        )?);
                        live_sessions_count += 1;
                    }
                    Err(error) => {
                        if errors_count > 0 {
                            errors_json.push(',');
                        }
                        errors_json.push_str(&format!(
                            "{{\"kind\":\"live\",\"session_id\":\"{}\",\"path\":\"{}\",\"error\":\"{}\"}}",
                            json_escape(&session_id),
                            json_escape(&path.display().to_string()),
                            json_escape(&error.to_string())
                        ));
                        errors_count += 1;
                    }
                }
            }
        }

        let summary = if include_live {
            format!(
                "{{\"sessions\":[{}],\"live_sessions\":[{}],\"errors\":[{}],\"limit\":{},\"include_live\":true,\"include_turns\":{}}}",
                sessions_json, live_sessions_json, errors_json, limit, include_turns
            )
        } else {
            format!(
                "{{\"sessions\":[{}],\"errors\":[{}],\"limit\":{}}}",
                sessions_json, errors_json, limit
            )
        };
        Ok(ToolOutput { summary })
    }
}

impl Tool for RlmLiveStatusTool {
    fn name(&self) -> &str {
        "rlm_process_status"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let limit = parse_rlm_model_sessions_limit(input.get("limit"))?;
        if let Some(session_id) = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            validate_rlm_model_session_id(session_id)?;
            let path = rlm_live_session_manifest_path(&self.config, session_id);
            if !path.exists() {
                return Ok(ToolOutput {
                    summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                        (
                            "session_id".to_string(),
                            JsonValue::String(session_id.to_string()),
                        ),
                        ("exists".to_string(), JsonValue::Bool(false)),
                        ("live".to_string(), JsonValue::Bool(false)),
                        (
                            "path".to_string(),
                            JsonValue::String(path.display().to_string()),
                        ),
                    ]))),
                });
            }
            let manifest = read_rlm_live_session_manifest(&path, session_id)?;
            let status = rlm_live_status_json(&self.config, session_id, &path, &manifest)?;
            return Ok(ToolOutput {
                summary: json_value_to_string(&status),
            });
        }

        let mut sessions = Vec::new();
        let mut errors = Vec::new();
        for (session_id, path) in list_rlm_live_session_manifest_entries(&self.config)?
            .into_iter()
            .take(limit)
        {
            match read_rlm_live_session_manifest(&path, &session_id).and_then(|manifest| {
                rlm_live_status_json(&self.config, &session_id, &path, &manifest)
            }) {
                Ok(status) => sessions.push(status),
                Err(error) => errors.push(JsonValue::Object(BTreeMap::from([
                    ("session_id".to_string(), JsonValue::String(session_id)),
                    (
                        "path".to_string(),
                        JsonValue::String(path.display().to_string()),
                    ),
                    ("error".to_string(), JsonValue::String(error.to_string())),
                ]))),
            }
        }
        let totals = rlm_live_status_totals_json(&sessions);
        Ok(ToolOutput {
            summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                ("sessions".to_string(), JsonValue::Array(sessions)),
                ("totals".to_string(), totals),
                ("errors".to_string(), JsonValue::Array(errors)),
                ("limit".to_string(), JsonValue::Number(limit.to_string())),
            ]))),
        })
    }
}

impl Tool for RlmLiveEventsTool {
    fn name(&self) -> &str {
        "rlm_process_events"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_process_events requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let cursor = parse_rlm_live_event_cursor(
            input
                .get("cursor")
                .or_else(|| input.get("after_seq"))
                .or_else(|| input.get("since_seq")),
        )?;
        let limit = parse_rlm_live_events_limit(input.get("limit"))?;
        let batch = read_rlm_live_event_batch(&self.config, session_id, cursor, limit)?;
        Ok(ToolOutput {
            summary: format!(
                "{{\"session_id\":\"{}\",\"path\":\"{}\",\"exists\":{},\"cursor\":{},\"next_cursor\":{},\"events\":[{}],\"limit\":{}}}",
                json_escape(session_id),
                json_escape(&batch.path.display().to_string()),
                batch.exists,
                cursor,
                batch.next_cursor,
                batch.events_json,
                limit
            ),
        })
    }
}

impl Tool for RlmLiveWaitTool {
    fn name(&self) -> &str {
        "rlm_process_wait"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_process_wait requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let cursor = parse_rlm_live_event_cursor(
            input
                .get("cursor")
                .or_else(|| input.get("after_seq"))
                .or_else(|| input.get("since_seq")),
        )?;
        let limit = parse_rlm_live_events_limit(input.get("limit"))?;
        let timeout = parse_rlm_live_wait_timeout(input.get("timeout_ms"))?;
        let poll_interval = parse_rlm_live_wait_poll_interval(input.get("poll_interval_ms"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let batch = read_rlm_live_event_batch(&self.config, session_id, cursor, limit)?;
            if batch.count > 0 || Instant::now() >= deadline {
                let timed_out = batch.count == 0 && timeout > Duration::ZERO;
                return Ok(ToolOutput {
                    summary: format!(
                        "{{\"session_id\":\"{}\",\"path\":\"{}\",\"exists\":{},\"cursor\":{},\"next_cursor\":{},\"events\":[{}],\"limit\":{},\"timed_out\":{},\"timeout_ms\":{}}}",
                        json_escape(session_id),
                        json_escape(&batch.path.display().to_string()),
                        batch.exists,
                        cursor,
                        batch.next_cursor,
                        batch.events_json,
                        limit,
                        timed_out,
                        timeout.as_millis()
                    ),
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(std::cmp::min(poll_interval, remaining));
        }
    }
}

impl Tool for RlmLiveCancelTool {
    fn name(&self) -> &str {
        "rlm_process_cancel"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_process_cancel requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let target_task_id = input
            .get("task_id")
            .or_else(|| input.get("turn_id"))
            .or_else(|| input.get("id"))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let cancel_all = parse_bool_arg(input.get("all"));
        if target_task_id.is_none() && !cancel_all {
            return Err(tool_failure(
                "rlm_process_cancel requires `task_id`/`turn_id` or `all=true`",
            ));
        }
        let force = parse_bool_arg(input.get("force"));
        let reason = input
            .get("reason")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("cancelled by rlm_process_cancel")
            .to_string();
        let manifest_path = rlm_live_session_manifest_path(&self.config, session_id);
        let manifest = read_rlm_live_session_manifest(&manifest_path, session_id)?;
        let runtime_thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id")
            .ok_or_else(|| {
                tool_failure("rlm_process_cancel live manifest is missing runtime_thread_id")
            })?;
        let store = rlm_runtime_store(&self.config);
        let thread = store.load_thread(&runtime_thread_id)?;
        let candidates = if let Some(task_id) = target_task_id {
            let task = store.load_task(task_id)?;
            ensure_cancelable_live_rlm_task(&task, &runtime_thread_id)?;
            vec![task]
        } else {
            store
                .list_tasks(None, Some(&runtime_thread_id), MAX_RLM_LIVE_TASK_LIST)?
                .into_iter()
                .filter(|task| is_cancelable_live_rlm_task(task, &runtime_thread_id))
                .collect()
        };
        let mut cancelled = Vec::new();
        let mut active_cancelled = false;
        let mut active_owner_cancelled = false;
        let active_turn_id = rlm_live_manifest_string_field(&manifest, "active_turn_id");
        for task in candidates {
            let original_summary = task.summary.clone();
            if active_turn_id.as_deref() == Some(task.id.as_str()) {
                active_owner_cancelled = true;
                active_cancelled = true;
            } else if task.status == "running" {
                active_cancelled = true;
            }
            let (updated, _event) = store.cancel_task(&task.id, reason.clone())?;
            mark_rlm_live_session_turn_payload_cancelled(
                &self.config,
                session_id,
                &updated.id,
                &reason,
            )?;
            append_rlm_live_session_cancel_event(
                &self.config,
                session_id,
                &runtime_thread_id,
                &updated.id,
                &original_summary,
                &reason,
            )?;
            cancelled.push(rlm_live_cancelled_task_json(&updated, &original_summary));
        }
        let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
        let daemon_pid = rlm_live_manifest_u64_field(&manifest, "daemon_pid");
        let daemon_epoch = rlm_live_manifest_string_field(&manifest, "daemon_epoch");
        let mut next_active_turn_id = active_turn_id.clone();
        let (interrupted, interrupt) = if force && active_owner_cancelled {
            let (interrupted, interrupt) = rlm_interrupt_live_daemon_owner(daemon_pid);
            if interrupted {
                next_active_turn_id = None;
                if let Some(active_turn_id) = active_turn_id.as_deref() {
                    let _ = append_rlm_live_session_json_event(
                        &self.config,
                        session_id,
                        "worker_interrupted",
                        &runtime_thread_id,
                        active_turn_id,
                        vec![("interrupt".to_string(), interrupt.clone())],
                    );
                }
            }
            (interrupted, interrupt)
        } else {
            (false, JsonValue::Null)
        };
        let status = if next_active_turn_id.is_some() {
            "running"
        } else {
            "idle"
        };
        let runtime_session_id = rlm_live_manifest_string_field(&manifest, "runtime_session_id")
            .or_else(|| thread.session_id.clone());
        let model =
            rlm_live_manifest_string_field(&manifest, "model").unwrap_or_else(|| thread.model);
        let workspace = rlm_live_manifest_string_field(&manifest, "workspace")
            .unwrap_or_else(|| thread.workspace);
        let created_at = rlm_live_manifest_string_field(&manifest, "created_at");
        if next_active_turn_id.is_some() {
            write_rlm_live_session_manifest_with_daemon(
                &self.config,
                session_id,
                status,
                &runtime_thread_id,
                runtime_session_id.as_deref(),
                next_active_turn_id.as_deref(),
                queued_turns,
                &model,
                &workspace,
                created_at.as_deref(),
                daemon_pid,
                daemon_epoch.as_deref(),
            )?;
        } else {
            write_rlm_live_session_manifest(
                &self.config,
                session_id,
                status,
                &runtime_thread_id,
                runtime_session_id.as_deref(),
                next_active_turn_id.as_deref(),
                queued_turns,
                &model,
                &workspace,
                created_at.as_deref(),
            )?;
        }
        Ok(ToolOutput {
            summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                (
                    "session_id".to_string(),
                    JsonValue::String(session_id.to_string()),
                ),
                (
                    "runtime_thread_id".to_string(),
                    JsonValue::String(runtime_thread_id),
                ),
                (
                    "cancelled_count".to_string(),
                    JsonValue::Number(cancelled.len().to_string()),
                ),
                (
                    "active_cancelled".to_string(),
                    JsonValue::Bool(active_cancelled),
                ),
                (
                    "active_owner_cancelled".to_string(),
                    JsonValue::Bool(active_owner_cancelled),
                ),
                ("force".to_string(), JsonValue::Bool(force)),
                ("interrupted".to_string(), JsonValue::Bool(interrupted)),
                ("interrupt".to_string(), interrupt),
                (
                    "queued_turns".to_string(),
                    JsonValue::Number(queued_turns.to_string()),
                ),
                ("reason".to_string(), JsonValue::String(reason)),
                ("cancelled".to_string(), JsonValue::Array(cancelled)),
            ]))),
        })
    }
}

impl Tool for RlmLiveRunNextTool {
    fn name(&self) -> &str {
        "rlm_process_run_next"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_process_run_next requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let target_task_id = input
            .get("task_id")
            .or_else(|| input.get("turn_id"))
            .or_else(|| input.get("id"))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let dry_run = parse_bool_arg(input.get("dry_run"));
        let manifest_path = rlm_live_session_manifest_path(&self.config, session_id);
        let manifest = read_rlm_live_session_manifest(&manifest_path, session_id)?;
        let runtime_thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id")
            .ok_or_else(|| {
                tool_failure("rlm_process_run_next live manifest is missing runtime_thread_id")
            })?;
        let store = rlm_runtime_store(&self.config);
        let thread = store.load_thread(&runtime_thread_id)?;
        let task = match target_task_id {
            Some(task_id) => {
                let task = store.load_task(task_id)?;
                ensure_pending_live_rlm_task(&task, &runtime_thread_id)?;
                Some(task)
            }
            None => oldest_pending_live_rlm_task(&store, &runtime_thread_id)?,
        };
        let Some(task) = task else {
            return Ok(ToolOutput {
                summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                    (
                        "session_id".to_string(),
                        JsonValue::String(session_id.to_string()),
                    ),
                    (
                        "runtime_thread_id".to_string(),
                        JsonValue::String(runtime_thread_id),
                    ),
                    ("status".to_string(), JsonValue::String("idle".to_string())),
                    (
                        "queued_turns".to_string(),
                        JsonValue::Number("0".to_string()),
                    ),
                ]))),
            });
        };
        let payload = read_rlm_live_session_turn_payload(&self.config, session_id, &task.id)?;
        if payload.runtime_thread_id != runtime_thread_id {
            return Err(tool_failure(format!(
                "rlm_process_run_next payload for task {} points at thread `{}` instead of `{runtime_thread_id}`",
                task.id, payload.runtime_thread_id
            )));
        }
        if payload.status != "queued" {
            return Err(tool_failure(format!(
                "rlm_process_run_next payload for task {} is `{}` instead of `queued`",
                task.id, payload.status
            )));
        }
        let child_task = render_rlm_process_task_with_session(&payload.task, &payload.input, None);
        if dry_run {
            return Ok(ToolOutput {
                summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                    (
                        "session_id".to_string(),
                        JsonValue::String(payload.session_id),
                    ),
                    (
                        "runtime_thread_id".to_string(),
                        JsonValue::String(payload.runtime_thread_id),
                    ),
                    ("task_id".to_string(), JsonValue::String(payload.task_id)),
                    ("dry_run".to_string(), JsonValue::Bool(true)),
                    (
                        "payload_status".to_string(),
                        JsonValue::String(payload.status),
                    ),
                    ("task".to_string(), JsonValue::String(payload.task)),
                    ("steps".to_string(), JsonValue::String(payload.steps)),
                    ("rendered_task".to_string(), JsonValue::String(child_task)),
                ]))),
            });
        }

        let claimed = store.claim_task(&task.id, "rlm_process_run_next".to_string())?;
        update_rlm_live_session_turn_payload_status(
            &self.config,
            session_id,
            &claimed.id,
            "running",
            Vec::new(),
        )?;
        let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
        let daemon_epoch = rlm_epoch_label();
        write_rlm_live_session_manifest_with_daemon(
            &self.config,
            session_id,
            "running",
            &runtime_thread_id,
            thread.session_id.as_deref(),
            Some(&claimed.id),
            queued_turns,
            &payload.model,
            &payload.workspace,
            rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
            Some(std::process::id() as u64),
            Some(&daemon_epoch),
        )?;
        append_rlm_live_session_event(
            &self.config,
            session_id,
            "turn_started",
            &runtime_thread_id,
            &claimed.id,
            &payload.task,
            &payload.input.label,
        )?;
        let child_input = ToolInput::new()
            .with_arg("task", child_task)
            .with_arg("steps", payload.steps.clone());
        let cancel_check: SharedAgentCancelCheck = Rc::new(RefCell::new(RlmLiveTaskCancelCheck {
            store: store.clone(),
            task_id: claimed.id.clone(),
        }));
        let event_target = RlmLiveWorkerEventTarget {
            config: self.config.clone(),
            session_id: session_id.to_string(),
            runtime_thread_id: runtime_thread_id.clone(),
            task_id: claimed.id.clone(),
        };
        let stream_events: Box<dyn StreamEvents> = Box::new(RlmLiveWorkerStreamEvents {
            target: event_target.clone(),
        });
        let run_events: SharedAgentRunEvents = Rc::new(RefCell::new(RlmLiveWorkerRunEvents {
            target: event_target,
        }));
        let output = DispatchSubagentTool {
            config: self.config.clone(),
            parent_depth: self.parent_depth,
        }
        .execute_with_agent_events(
            child_input,
            Some(cancel_check),
            Some(stream_events),
            Some(run_events),
        );
        match output {
            Ok(output) => {
                store.update_task(&claimed.id, "completed".to_string(), output.summary.clone())?;
                update_rlm_live_session_turn_payload_status(
                    &self.config,
                    session_id,
                    &claimed.id,
                    "completed",
                    vec![(
                        "result_summary".to_string(),
                        JsonValue::String(output.summary.clone()),
                    )],
                )?;
                let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
                write_rlm_live_session_manifest(
                    &self.config,
                    session_id,
                    "idle",
                    &runtime_thread_id,
                    thread.session_id.as_deref(),
                    None,
                    queued_turns,
                    &payload.model,
                    &payload.workspace,
                    rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
                )?;
                append_rlm_live_session_event(
                    &self.config,
                    session_id,
                    "turn_completed",
                    &runtime_thread_id,
                    &claimed.id,
                    &payload.task,
                    "completed",
                )?;
                Ok(ToolOutput {
                    summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                        (
                            "session_id".to_string(),
                            JsonValue::String(session_id.to_string()),
                        ),
                        (
                            "runtime_thread_id".to_string(),
                            JsonValue::String(runtime_thread_id),
                        ),
                        ("task_id".to_string(), JsonValue::String(claimed.id)),
                        (
                            "status".to_string(),
                            JsonValue::String("completed".to_string()),
                        ),
                        (
                            "queued_turns".to_string(),
                            JsonValue::Number(queued_turns.to_string()),
                        ),
                        (
                            "result_summary".to_string(),
                            JsonValue::String(output.summary),
                        ),
                    ]))),
                })
            }
            Err(error) => {
                let message = error.to_string();
                if rlm_live_run_next_error_is_cancelled(&store, &claimed.id, &message) {
                    let cancel_reason = store
                        .load_task(&claimed.id)
                        .ok()
                        .filter(|task| task.status == "cancelled")
                        .map(|task| task.summary)
                        .unwrap_or_else(|| message.clone());
                    if store
                        .load_task(&claimed.id)
                        .map(|task| task.status != "cancelled")
                        .unwrap_or(true)
                    {
                        let _ = store.update_task(
                            &claimed.id,
                            "cancelled".to_string(),
                            cancel_reason.clone(),
                        );
                    }
                    let _ = mark_rlm_live_session_turn_payload_cancelled(
                        &self.config,
                        session_id,
                        &claimed.id,
                        &cancel_reason,
                    );
                    let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
                    let _ = write_rlm_live_session_manifest(
                        &self.config,
                        session_id,
                        "idle",
                        &runtime_thread_id,
                        thread.session_id.as_deref(),
                        None,
                        queued_turns,
                        &payload.model,
                        &payload.workspace,
                        rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
                    );
                    let _ = append_rlm_live_session_cancel_event(
                        &self.config,
                        session_id,
                        &runtime_thread_id,
                        &claimed.id,
                        &payload.task,
                        &cancel_reason,
                    );
                    return Ok(ToolOutput {
                        summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                            (
                                "session_id".to_string(),
                                JsonValue::String(session_id.to_string()),
                            ),
                            (
                                "runtime_thread_id".to_string(),
                                JsonValue::String(runtime_thread_id),
                            ),
                            ("task_id".to_string(), JsonValue::String(claimed.id)),
                            (
                                "status".to_string(),
                                JsonValue::String("cancelled".to_string()),
                            ),
                            (
                                "queued_turns".to_string(),
                                JsonValue::Number(queued_turns.to_string()),
                            ),
                            (
                                "cancel_reason".to_string(),
                                JsonValue::String(cancel_reason),
                            ),
                        ]))),
                    });
                }
                let _ = store.update_task(&claimed.id, "failed".to_string(), message.clone());
                let _ = update_rlm_live_session_turn_payload_status(
                    &self.config,
                    session_id,
                    &claimed.id,
                    "failed",
                    vec![("error".to_string(), JsonValue::String(message.clone()))],
                );
                let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
                let _ = write_rlm_live_session_manifest(
                    &self.config,
                    session_id,
                    "error",
                    &runtime_thread_id,
                    thread.session_id.as_deref(),
                    None,
                    queued_turns,
                    &payload.model,
                    &payload.workspace,
                    rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
                );
                let _ = append_rlm_live_session_event(
                    &self.config,
                    session_id,
                    "turn_failed",
                    &runtime_thread_id,
                    &claimed.id,
                    &payload.task,
                    &message,
                );
                Err(tool_failure(format!(
                    "rlm_process_run_next task {} failed: {message}",
                    claimed.id
                )))
            }
        }
    }
}

impl Tool for RlmLiveDrainTool {
    fn name(&self) -> &str {
        "rlm_process_drain"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_process_drain requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let max_turns = parse_rlm_live_drain_max_turns(input.get("max_turns"))?;
        let dry_run = parse_bool_arg(input.get("dry_run"));
        let manifest_path = rlm_live_session_manifest_path(&self.config, session_id);
        let manifest = read_rlm_live_session_manifest(&manifest_path, session_id)?;
        let runtime_thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id")
            .ok_or_else(|| {
                tool_failure("rlm_process_drain live manifest is missing runtime_thread_id")
            })?;
        let store = rlm_runtime_store(&self.config);
        let pending = pending_live_rlm_tasks(&store, &runtime_thread_id)?;
        let selected = pending.into_iter().take(max_turns).collect::<Vec<_>>();
        if dry_run {
            return Ok(ToolOutput {
                summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                    (
                        "session_id".to_string(),
                        JsonValue::String(session_id.to_string()),
                    ),
                    (
                        "runtime_thread_id".to_string(),
                        JsonValue::String(runtime_thread_id),
                    ),
                    ("dry_run".to_string(), JsonValue::Bool(true)),
                    (
                        "selected_count".to_string(),
                        JsonValue::Number(selected.len().to_string()),
                    ),
                    (
                        "max_turns".to_string(),
                        JsonValue::Number(max_turns.to_string()),
                    ),
                    (
                        "turns".to_string(),
                        JsonValue::Array(
                            selected
                                .iter()
                                .map(|task| {
                                    JsonValue::Object(BTreeMap::from([
                                        ("task_id".to_string(), JsonValue::String(task.id.clone())),
                                        (
                                            "status".to_string(),
                                            JsonValue::String(task.status.clone()),
                                        ),
                                        (
                                            "created_at".to_string(),
                                            JsonValue::String(task.created_at.clone()),
                                        ),
                                        (
                                            "summary".to_string(),
                                            JsonValue::String(task.summary.clone()),
                                        ),
                                    ]))
                                })
                                .collect(),
                        ),
                    ),
                ]))),
            });
        }

        let mut results = Vec::new();
        for task in selected {
            let output = RlmLiveRunNextTool {
                config: self.config.clone(),
                parent_depth: self.parent_depth,
            }
            .execute(
                ToolInput::new()
                    .with_arg("session_id", session_id.to_string())
                    .with_arg("task_id", task.id.clone()),
            )?;
            results.push(JsonValue::Object(BTreeMap::from([
                ("task_id".to_string(), JsonValue::String(task.id)),
                ("summary".to_string(), JsonValue::String(output.summary)),
            ])));
        }
        let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
        Ok(ToolOutput {
            summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                (
                    "session_id".to_string(),
                    JsonValue::String(session_id.to_string()),
                ),
                (
                    "runtime_thread_id".to_string(),
                    JsonValue::String(runtime_thread_id),
                ),
                ("dry_run".to_string(), JsonValue::Bool(false)),
                (
                    "ran_count".to_string(),
                    JsonValue::Number(results.len().to_string()),
                ),
                (
                    "queued_turns".to_string(),
                    JsonValue::Number(queued_turns.to_string()),
                ),
                ("results".to_string(), JsonValue::Array(results)),
            ]))),
        })
    }
}

impl Tool for RlmLiveRecoverTool {
    fn name(&self) -> &str {
        "rlm_process_recover"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let mode = parse_rlm_live_recovery_mode(input.get("mode"))?;
        let dry_run = parse_bool_arg(input.get("dry_run"));
        let force = parse_bool_arg(input.get("force"));
        let recover_all = parse_bool_arg(input.get("all"));
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let reason = input
            .get("reason")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("recovered interrupted live RLM worker")
            .to_string();

        if recover_all && session_id.is_none() {
            let limit = parse_rlm_live_recover_limit(input.get("limit"))?;
            let mut sessions = Vec::new();
            let mut errors = Vec::new();
            let mut recovered_total = 0u64;
            let mut scanned_count = 0usize;
            for (session_id, _path) in list_rlm_live_session_manifest_entries(&self.config)?
                .into_iter()
                .take(limit)
            {
                scanned_count += 1;
                let dry_run_value = if dry_run { "true" } else { "false" };
                let force_value = if force { "true" } else { "false" };
                let recover_input = ToolInput::new()
                    .with_arg("session_id", session_id.clone())
                    .with_arg("mode", mode.as_str())
                    .with_arg("dry_run", dry_run_value)
                    .with_arg("force", force_value)
                    .with_arg("reason", reason.clone());
                match self.execute(recover_input) {
                    Ok(output) => match parse_json_value(&output.summary) {
                        Ok(value) => {
                            if let JsonValue::Object(root) = &value {
                                recovered_total += root
                                    .get("recovered_count")
                                    .and_then(json_as_u64)
                                    .unwrap_or(0);
                            }
                            sessions.push(value);
                        }
                        Err(error) => errors.push(JsonValue::Object(BTreeMap::from([
                            (
                                "session_id".to_string(),
                                JsonValue::String(session_id.clone()),
                            ),
                            (
                                "error".to_string(),
                                JsonValue::String(format!("invalid recovery output JSON: {error}")),
                            ),
                        ]))),
                    },
                    Err(error) => errors.push(JsonValue::Object(BTreeMap::from([
                        ("session_id".to_string(), JsonValue::String(session_id)),
                        ("error".to_string(), JsonValue::String(error.to_string())),
                    ]))),
                }
            }
            return Ok(ToolOutput {
                summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                    ("all".to_string(), JsonValue::Bool(true)),
                    ("dry_run".to_string(), JsonValue::Bool(dry_run)),
                    ("force".to_string(), JsonValue::Bool(force)),
                    (
                        "mode".to_string(),
                        JsonValue::String(mode.as_str().to_string()),
                    ),
                    (
                        "scanned_count".to_string(),
                        JsonValue::Number(scanned_count.to_string()),
                    ),
                    (
                        "recovered_count".to_string(),
                        JsonValue::Number(recovered_total.to_string()),
                    ),
                    ("sessions".to_string(), JsonValue::Array(sessions)),
                    ("errors".to_string(), JsonValue::Array(errors)),
                    ("limit".to_string(), JsonValue::Number(limit.to_string())),
                ]))),
            });
        }

        let session_id = session_id
            .ok_or_else(|| tool_failure("rlm_process_recover requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let manifest_path = rlm_live_session_manifest_path(&self.config, session_id);
        let manifest = read_rlm_live_session_manifest(&manifest_path, session_id)?;
        let daemon_owner = rlm_live_daemon_owner_status_from_manifest(&manifest);
        let runtime_thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id")
            .ok_or_else(|| {
                tool_failure("rlm_process_recover live manifest is missing runtime_thread_id")
            })?;
        let active_turn_id = rlm_live_manifest_string_field(&manifest, "active_turn_id");
        let store = rlm_runtime_store(&self.config);
        let thread = store.load_thread(&runtime_thread_id)?;

        let mut candidate_ids = Vec::new();
        if let Some(active_turn_id) = active_turn_id.as_deref() {
            push_unique_string(&mut candidate_ids, active_turn_id);
        }
        for task in store.list_tasks(None, Some(&runtime_thread_id), MAX_RLM_LIVE_TASK_LIST)? {
            if task.kind == "rlm_process" && task.status == "running" {
                push_unique_string(&mut candidate_ids, &task.id);
            }
        }
        for task_id in list_rlm_live_turn_payload_ids(&self.config, session_id)? {
            if let Ok(payload) =
                read_rlm_live_session_turn_payload(&self.config, session_id, &task_id)
            {
                if payload.runtime_thread_id == runtime_thread_id && payload.status == "running" {
                    push_unique_string(&mut candidate_ids, &task_id);
                }
            }
        }

        let mut actions = Vec::new();
        let mut recovered_count = 0usize;
        let mut cleared_active = false;
        for task_id in candidate_ids {
            let task = store.load_task(&task_id).ok();
            let payload =
                read_rlm_live_session_turn_payload(&self.config, session_id, &task_id).ok();
            let task_status = task
                .as_ref()
                .map(|task| task.status.clone())
                .unwrap_or_else(|| "missing".to_string());
            let payload_status = payload
                .as_ref()
                .map(|payload| payload.status.clone())
                .unwrap_or_else(|| "missing".to_string());
            let mut action = "skip";
            let mut action_reason = reason.clone();

            if payload.as_ref().is_some_and(|payload| {
                payload.runtime_thread_id.as_str() != runtime_thread_id.as_str()
            }) {
                action = "skip_mismatched_thread";
                action_reason = "payload belongs to a different runtime thread".to_string();
            } else if !force
                && (task_status == "running" || payload_status == "running")
                && daemon_owner.alive == Some(true)
            {
                action = "skip_live_owner_alive";
                action_reason = format!(
                    "daemon owner pid {} is still alive; pass force=true to recover anyway",
                    daemon_owner
                        .pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
            } else if task_status == "running" || payload_status == "running" {
                if task.is_some() && payload.is_some() && mode == RlmLiveRecoveryMode::Requeue {
                    action = "requeue";
                    if !dry_run {
                        if let Some(task) = &task {
                            store.update_task(
                                &task.id,
                                "pending".to_string(),
                                task.summary.clone(),
                            )?;
                        }
                        update_rlm_live_session_turn_payload_status(
                            &self.config,
                            session_id,
                            &task_id,
                            "queued",
                            vec![
                                (
                                    "recovered_from".to_string(),
                                    JsonValue::String("running".to_string()),
                                ),
                                (
                                    "recovery_reason".to_string(),
                                    JsonValue::String(reason.clone()),
                                ),
                            ],
                        )?;
                        append_rlm_live_session_recovery_event(
                            &self.config,
                            session_id,
                            &runtime_thread_id,
                            &task_id,
                            task.as_ref()
                                .map(|task| task.summary.as_str())
                                .or_else(|| payload.as_ref().map(|payload| payload.task.as_str()))
                                .unwrap_or("interrupted live RLM turn"),
                            mode.as_str(),
                            action,
                            &reason,
                        )?;
                    }
                    recovered_count += 1;
                } else {
                    action = "fail";
                    action_reason = if task.is_none() || payload.is_none() {
                        "cannot requeue interrupted turn because task or payload is missing"
                            .to_string()
                    } else {
                        reason.clone()
                    };
                    if !dry_run {
                        if let Some(task) = &task {
                            store.update_task(
                                &task.id,
                                "failed".to_string(),
                                format!(
                                    "recovery failed interrupted live RLM turn: {action_reason}"
                                ),
                            )?;
                        }
                        if payload.is_some() {
                            update_rlm_live_session_turn_payload_status(
                                &self.config,
                                session_id,
                                &task_id,
                                "failed",
                                vec![(
                                    "recovery_reason".to_string(),
                                    JsonValue::String(action_reason.clone()),
                                )],
                            )?;
                        }
                        append_rlm_live_session_recovery_event(
                            &self.config,
                            session_id,
                            &runtime_thread_id,
                            &task_id,
                            task.as_ref()
                                .map(|task| task.summary.as_str())
                                .or_else(|| payload.as_ref().map(|payload| payload.task.as_str()))
                                .unwrap_or("interrupted live RLM turn"),
                            mode.as_str(),
                            action,
                            &action_reason,
                        )?;
                    }
                    recovered_count += 1;
                }
            } else if active_turn_id.as_deref() == Some(task_id.as_str()) {
                action = "clear_stale_active";
                action_reason = "active_turn_id no longer points at a running turn".to_string();
                recovered_count += 1;
            }

            if active_turn_id.as_deref() == Some(task_id.as_str()) && !action.starts_with("skip") {
                cleared_active = true;
            }
            actions.push(JsonValue::Object(BTreeMap::from([
                ("task_id".to_string(), JsonValue::String(task_id)),
                ("action".to_string(), JsonValue::String(action.to_string())),
                (
                    "task_status".to_string(),
                    JsonValue::String(task_status.clone()),
                ),
                (
                    "payload_status".to_string(),
                    JsonValue::String(payload_status.clone()),
                ),
                ("reason".to_string(), JsonValue::String(action_reason)),
            ])));
        }

        let queued_turns = if dry_run {
            count_pending_live_rlm_tasks(&store, &runtime_thread_id)?
        } else {
            let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
            let runtime_session_id =
                rlm_live_manifest_string_field(&manifest, "runtime_session_id")
                    .or_else(|| thread.session_id.clone());
            let model = rlm_live_manifest_string_field(&manifest, "model").unwrap_or(thread.model);
            let workspace =
                rlm_live_manifest_string_field(&manifest, "workspace").unwrap_or(thread.workspace);
            let created_at = rlm_live_manifest_string_field(&manifest, "created_at");
            let next_active_turn_id = if cleared_active {
                None
            } else {
                active_turn_id.as_deref()
            };
            let status = if next_active_turn_id.is_some() {
                "running"
            } else {
                "idle"
            };
            if next_active_turn_id.is_some() && daemon_owner.alive == Some(true) && !force {
                let daemon_epoch = rlm_live_manifest_string_field(&manifest, "daemon_epoch");
                write_rlm_live_session_manifest_with_daemon(
                    &self.config,
                    session_id,
                    status,
                    &runtime_thread_id,
                    runtime_session_id.as_deref(),
                    next_active_turn_id,
                    queued_turns,
                    &model,
                    &workspace,
                    created_at.as_deref(),
                    daemon_owner.pid,
                    daemon_epoch.as_deref(),
                )?;
            } else {
                write_rlm_live_session_manifest(
                    &self.config,
                    session_id,
                    status,
                    &runtime_thread_id,
                    runtime_session_id.as_deref(),
                    next_active_turn_id,
                    queued_turns,
                    &model,
                    &workspace,
                    created_at.as_deref(),
                )?;
            }
            queued_turns
        };

        Ok(ToolOutput {
            summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                (
                    "session_id".to_string(),
                    JsonValue::String(session_id.to_string()),
                ),
                (
                    "runtime_thread_id".to_string(),
                    JsonValue::String(runtime_thread_id),
                ),
                ("dry_run".to_string(), JsonValue::Bool(dry_run)),
                ("force".to_string(), JsonValue::Bool(force)),
                (
                    "daemon_alive".to_string(),
                    daemon_owner
                        .alive
                        .map(JsonValue::Bool)
                        .unwrap_or(JsonValue::Null),
                ),
                (
                    "daemon_stale".to_string(),
                    JsonValue::Bool(daemon_owner.stale),
                ),
                (
                    "mode".to_string(),
                    JsonValue::String(mode.as_str().to_string()),
                ),
                (
                    "recovered_count".to_string(),
                    JsonValue::Number(recovered_count.to_string()),
                ),
                (
                    "queued_turns".to_string(),
                    JsonValue::Number(queued_turns.to_string()),
                ),
                (
                    "cleared_active".to_string(),
                    JsonValue::Bool(!dry_run && cleared_active),
                ),
                ("actions".to_string(), JsonValue::Array(actions)),
            ]))),
        })
    }
}

impl Tool for RlmLiveStopTool {
    fn name(&self) -> &str {
        "rlm_process_stop"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let session_id = input
            .get("session_id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| tool_failure("rlm_process_stop requires non-empty `session_id`"))?;
        validate_rlm_model_session_id(session_id)?;
        let reason = input
            .get("reason")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("stopped by rlm_process_stop")
            .to_string();
        let manifest_path = rlm_live_session_manifest_path(&self.config, session_id);
        let manifest = read_rlm_live_session_manifest(&manifest_path, session_id)?;
        let runtime_thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id")
            .ok_or_else(|| {
                tool_failure("rlm_process_stop live manifest is missing runtime_thread_id")
            })?;
        let active_turn_id = rlm_live_manifest_string_field(&manifest, "active_turn_id");
        let store = rlm_runtime_store(&self.config);
        let thread = store.load_thread(&runtime_thread_id)?;
        if let Some(active_turn_id) = active_turn_id.as_deref() {
            let active_task = store.load_task(active_turn_id).ok();
            let active_payload =
                read_rlm_live_session_turn_payload(&self.config, session_id, active_turn_id).ok();
            if active_task
                .as_ref()
                .is_some_and(|task| task.status == "running")
                || active_payload
                    .as_ref()
                    .is_some_and(|payload| payload.status == "running")
            {
                return Err(tool_failure(
                    "rlm_process_stop refuses to stop an active running turn; wait for completion or recover it first",
                ));
            }
        }

        let pending = pending_live_rlm_tasks(&store, &runtime_thread_id)?;
        let mut cancelled = Vec::new();
        for task in pending {
            let original_summary = task.summary.clone();
            let (updated, _event) = store.cancel_task(&task.id, reason.clone())?;
            mark_rlm_live_session_turn_payload_cancelled(
                &self.config,
                session_id,
                &updated.id,
                &reason,
            )?;
            append_rlm_live_session_cancel_event(
                &self.config,
                session_id,
                &runtime_thread_id,
                &updated.id,
                &original_summary,
                &reason,
            )?;
            cancelled.push(rlm_live_cancelled_task_json(&updated, &original_summary));
        }

        let queued_turns = count_pending_live_rlm_tasks(&store, &runtime_thread_id)?;
        let runtime_session_id = rlm_live_manifest_string_field(&manifest, "runtime_session_id")
            .or_else(|| thread.session_id.clone());
        let model =
            rlm_live_manifest_string_field(&manifest, "model").unwrap_or_else(|| thread.model);
        let workspace = rlm_live_manifest_string_field(&manifest, "workspace")
            .unwrap_or_else(|| thread.workspace);
        let created_at = rlm_live_manifest_string_field(&manifest, "created_at");
        write_rlm_live_session_manifest(
            &self.config,
            session_id,
            "stopped",
            &runtime_thread_id,
            runtime_session_id.as_deref(),
            None,
            queued_turns,
            &model,
            &workspace,
            created_at.as_deref(),
        )?;
        append_rlm_live_session_stop_event(
            &self.config,
            session_id,
            &runtime_thread_id,
            cancelled.len(),
            &reason,
        )?;
        Ok(ToolOutput {
            summary: json_value_to_string(&JsonValue::Object(BTreeMap::from([
                (
                    "session_id".to_string(),
                    JsonValue::String(session_id.to_string()),
                ),
                (
                    "runtime_thread_id".to_string(),
                    JsonValue::String(runtime_thread_id),
                ),
                (
                    "status".to_string(),
                    JsonValue::String("stopped".to_string()),
                ),
                (
                    "cancelled_count".to_string(),
                    JsonValue::Number(cancelled.len().to_string()),
                ),
                (
                    "queued_turns".to_string(),
                    JsonValue::Number(queued_turns.to_string()),
                ),
                ("reason".to_string(), JsonValue::String(reason)),
                ("cancelled".to_string(), JsonValue::Array(cancelled)),
            ]))),
        })
    }
}

pub(crate) fn render_rlm_task(context: &str, question: &str, strategy: &str) -> String {
    format!(
        "RLM analysis task\n\
         Strategy: {strategy}\n\n\
         Question:\n{question}\n\n\
         Context:\n{context}\n\n\
         Return a concise synthesized answer. Cite concrete evidence from the context when possible. If the context is insufficient, say what is missing."
    )
}

fn input_has_rlm_process_shape(input: &ToolInput) -> bool {
    input
        .get("task")
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
        || input
            .get("file_path")
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        || input
            .get("content")
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
}

fn load_rlm_process_input(input: &ToolInput) -> AppResult<RlmProcessInput> {
    let file_path = input
        .get("file_path")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let content = input
        .get("content")
        .filter(|value| !value.trim().is_empty());
    match (file_path, content) {
        (Some(_), Some(_)) => Err(tool_failure(
            "rlm requires `file_path` or `content`, not both",
        )),
        (None, None) => Err(tool_failure("rlm requires `file_path` or `content`")),
        (Some(path), None) => load_rlm_process_file(path),
        (None, Some(content)) => {
            validate_rlm_process_content_len(content)?;
            Ok(RlmProcessInput {
                label: "inline content".to_string(),
                content: content.to_string(),
                char_count: content.chars().count(),
                line_count: content.lines().count(),
            })
        }
    }
}

fn load_rlm_process_input_or_session_context(
    input: &ToolInput,
    session: Option<&RlmModelSession>,
) -> AppResult<RlmProcessInput> {
    match load_rlm_process_input(input) {
        Ok(input) => Ok(input),
        Err(error) if !rlm_process_has_input_source(input) => {
            let Some(session) = session else {
                return Err(error);
            };
            if session.turns.is_empty() {
                return Err(tool_failure(
                    "rlm_process session-only continuation requires an existing session with prior turns or a new file_path/content input",
                ));
            }
            Ok(RlmProcessInput {
                label: "session context only".to_string(),
                content: "No new long input was provided. Continue from the prior RLM session context above.".to_string(),
                char_count: 0,
                line_count: 0,
            })
        }
        Err(error) => Err(error),
    }
}

fn rlm_process_has_input_source(input: &ToolInput) -> bool {
    input
        .get("file_path")
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
        || input
            .get("content")
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
}

fn load_rlm_process_file(raw_path: &str) -> AppResult<RlmProcessInput> {
    let relative_path = validate_rlm_process_file_path(raw_path)?;
    let content = fs::read_to_string(&relative_path)
        .map_err(|error| tool_failure(format!("rlm failed to read `{raw_path}`: {error}")))?;
    validate_rlm_process_content_len(&content)?;
    Ok(RlmProcessInput {
        label: format!("file_path: {}", relative_path.display()),
        char_count: content.chars().count(),
        line_count: content.lines().count(),
        content,
    })
}

fn validate_rlm_process_file_path(raw_path: &str) -> AppResult<PathBuf> {
    let path = Path::new(raw_path);
    if raw_path.trim().is_empty() || path.is_absolute() {
        return Err(tool_failure(
            "rlm file_path must be a non-empty workspace-relative path",
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => {
                return Err(tool_failure(
                    "rlm file_path must not contain absolute, parent, or prefix components",
                ))
            }
        }
    }
    if path.is_dir() {
        return Err(tool_failure("rlm file_path points to a directory"));
    }
    let cwd = std::env::current_dir()
        .map_err(|error| tool_failure(format!("rlm current_dir failed: {error}")))?;
    let joined = cwd.join(path);
    if let (Ok(canonical_cwd), Ok(canonical_target)) =
        (fs::canonicalize(&cwd), fs::canonicalize(&joined))
    {
        if !canonical_target.starts_with(canonical_cwd) {
            return Err(tool_failure("rlm file_path resolves outside workspace"));
        }
    }
    Ok(path.to_path_buf())
}

fn validate_rlm_process_content_len(content: &str) -> AppResult<()> {
    let chars = content.chars().count();
    if chars > MAX_RLM_PROCESS_CONTENT_CHARS {
        Err(tool_failure(format!(
            "rlm content is {chars} chars; maximum is {MAX_RLM_PROCESS_CONTENT_CHARS}. Use a smaller file or pre-chunk it."
        )))
    } else if content.trim().is_empty() {
        Err(tool_failure("rlm input content is empty"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn render_rlm_process_task(task: &str, input: &RlmProcessInput) -> String {
    render_rlm_process_task_with_session(task, input, None)
}

fn render_rlm_process_task_with_session(
    task: &str,
    input: &RlmProcessInput,
    session: Option<&RlmModelSession>,
) -> String {
    let session_context = session
        .map(render_rlm_model_session_context)
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("\n\nPrior RLM session context:\n{value}"))
        .unwrap_or_default();
    format!(
        "RLM process task\n\
         Objective: {task}\n\
         Input source: {label}\n\
         Input size: {chars} chars, {lines} lines\n\n\
         Process the long input below using a DeepSeek-TUI-style RLM workflow: preview, chunk or sample when useful, compute exact structured facts directly, use map-reduce synthesis when the input is broad, and report coverage or any skipped sections.{session_context}\n\n\
         Long input:\n{content}",
        label = &input.label,
        chars = input.char_count,
        lines = input.line_count,
        content = &input.content
    )
}

fn rlm_model_sessions_dir(config: &AppConfig) -> PathBuf {
    PathBuf::from(&config.workspace.config_dir).join("rlm-model")
}

fn rlm_model_session_path(config: &AppConfig, session_id: &str) -> PathBuf {
    rlm_model_sessions_dir(config).join(format!("{session_id}.json"))
}

fn rlm_live_sessions_dir(config: &AppConfig) -> PathBuf {
    PathBuf::from(&config.workspace.config_dir).join("rlm-daemon")
}

fn rlm_live_session_manifest_path(config: &AppConfig, session_id: &str) -> PathBuf {
    rlm_live_sessions_dir(config)
        .join(session_id)
        .join("manifest.json")
}

fn validate_rlm_model_session_id(session_id: &str) -> AppResult<()> {
    let valid = !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        && !session_id.starts_with('.')
        && !session_id.contains("..");
    if valid {
        Ok(())
    } else {
        Err(tool_failure(
            "rlm_process session_id must use 1-64 chars of [A-Za-z0-9_.-] without leading dot or `..`",
        ))
    }
}

fn read_rlm_model_session(
    config: &AppConfig,
    session_id: &str,
    reset: bool,
) -> AppResult<RlmModelSession> {
    let path = rlm_model_session_path(config, session_id);
    if reset || !path.exists() {
        return Ok(empty_rlm_model_session(session_id));
    }
    let content = fs::read_to_string(&path)
        .map_err(|error| tool_failure(format!("rlm_process session read failed: {error}")))?;
    let value = parse_json_value(&content)
        .map_err(|error| tool_failure(format!("rlm_process session invalid JSON: {error}")))?;
    let JsonValue::Object(root) = value else {
        return Err(tool_failure("rlm_process session must be a JSON object"));
    };
    let stored_id = root
        .get("session_id")
        .and_then(json_as_string)
        .unwrap_or(session_id);
    if stored_id != session_id {
        return Err(tool_failure("rlm_process session_id mismatch"));
    }
    let mut turns = match root.get("turns") {
        Some(JsonValue::Array(items)) => items
            .iter()
            .filter_map(rlm_model_session_turn_from_json)
            .collect::<Vec<_>>(),
        Some(_) => return Err(tool_failure("rlm_process session turns must be an array")),
        None => Vec::new(),
    };
    if turns.len() > MAX_RLM_MODEL_SESSION_TURNS {
        let remove = turns.len() - MAX_RLM_MODEL_SESSION_TURNS;
        turns.drain(0..remove);
    }
    let updated_at = root
        .get("updated_at")
        .and_then(json_as_string)
        .map(str::to_string)
        .or_else(|| turns.last().map(|turn| turn.updated_at.clone()))
        .unwrap_or_else(|| "epoch+0".to_string());
    Ok(RlmModelSession {
        session_id: session_id.to_string(),
        updated_at,
        turns,
    })
}

fn empty_rlm_model_session(session_id: &str) -> RlmModelSession {
    RlmModelSession {
        session_id: session_id.to_string(),
        updated_at: "epoch+0".to_string(),
        turns: Vec::new(),
    }
}

fn write_rlm_model_session(config: &AppConfig, session: &RlmModelSession) -> AppResult<()> {
    let path = rlm_model_session_path(config, &session.session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| tool_failure(format!("rlm_process session mkdir failed: {error}")))?;
    }
    fs::write(
        &path,
        json_value_to_string(&rlm_model_session_to_json(session)),
    )
    .map_err(|error| tool_failure(format!("rlm_process session write failed: {error}")))?;
    Ok(())
}

fn append_rlm_model_session_turn(
    session: &mut RlmModelSession,
    task: &str,
    input: &RlmProcessInput,
    summary: &str,
) {
    let updated_at = rlm_epoch_label();
    session.turns.push(RlmModelSessionTurn {
        task: task.trim().to_string(),
        label: input.label.clone(),
        char_count: input.char_count,
        line_count: input.line_count,
        summary: clip_rlm_model_session_summary(summary.trim()),
        updated_at: updated_at.clone(),
    });
    session.updated_at = updated_at;
    if session.turns.len() > MAX_RLM_MODEL_SESSION_TURNS {
        let remove = session.turns.len() - MAX_RLM_MODEL_SESSION_TURNS;
        session.turns.drain(0..remove);
    }
}

fn render_rlm_model_session_context(session: &RlmModelSession) -> String {
    if session.turns.is_empty() {
        return String::new();
    }
    let start = session
        .turns
        .len()
        .saturating_sub(MAX_RLM_MODEL_SESSION_CONTEXT_TURNS);
    let turns = session.turns[start..]
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            format!(
                "Session turn {}:\nTask: {}\nInput source: {}\nInput size: {} chars, {} lines\nPrior summary:\n{}",
                start + index + 1,
                turn.task,
                turn.label,
                turn.char_count,
                turn.line_count,
                turn.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "Session: {}, prior_turns={}\n\n{}",
        session.session_id,
        session.turns.len(),
        turns
    )
}

fn rlm_model_session_to_json(session: &RlmModelSession) -> JsonValue {
    JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.rlm.model_session.v1".to_string()),
        ),
        (
            "session_id".to_string(),
            JsonValue::String(session.session_id.clone()),
        ),
        (
            "updated_at".to_string(),
            JsonValue::String(session.updated_at.clone()),
        ),
        (
            "turns".to_string(),
            JsonValue::Array(
                session
                    .turns
                    .iter()
                    .map(rlm_model_session_turn_to_json)
                    .collect(),
            ),
        ),
    ]))
}

fn rlm_model_session_turn_to_json(turn: &RlmModelSessionTurn) -> JsonValue {
    JsonValue::Object(BTreeMap::from([
        ("task".to_string(), JsonValue::String(turn.task.clone())),
        ("label".to_string(), JsonValue::String(turn.label.clone())),
        (
            "char_count".to_string(),
            JsonValue::Number(turn.char_count.to_string()),
        ),
        (
            "line_count".to_string(),
            JsonValue::Number(turn.line_count.to_string()),
        ),
        (
            "summary".to_string(),
            JsonValue::String(turn.summary.clone()),
        ),
        (
            "updated_at".to_string(),
            JsonValue::String(turn.updated_at.clone()),
        ),
    ]))
}

fn rlm_model_session_turn_from_json(value: &JsonValue) -> Option<RlmModelSessionTurn> {
    let JsonValue::Object(root) = value else {
        return None;
    };
    Some(RlmModelSessionTurn {
        task: root.get("task").and_then(json_as_string)?.to_string(),
        label: root.get("label").and_then(json_as_string)?.to_string(),
        char_count: root.get("char_count").and_then(json_as_u64).unwrap_or(0) as usize,
        line_count: root.get("line_count").and_then(json_as_u64).unwrap_or(0) as usize,
        summary: root.get("summary").and_then(json_as_string)?.to_string(),
        updated_at: root
            .get("updated_at")
            .and_then(json_as_string)
            .unwrap_or("epoch+0")
            .to_string(),
    })
}

fn read_rlm_live_session_manifest(path: &Path, session_id: &str) -> AppResult<JsonValue> {
    let content = fs::read_to_string(path)
        .map_err(|error| tool_failure(format!("rlm live session read failed: {error}")))?;
    let value = parse_json_value(&content)
        .map_err(|error| tool_failure(format!("rlm live session invalid JSON: {error}")))?;
    let JsonValue::Object(root) = value else {
        return Err(tool_failure(
            "rlm live session manifest must be a JSON object",
        ));
    };
    let stored_id = root
        .get("session_id")
        .and_then(json_as_string)
        .unwrap_or(session_id);
    if stored_id != session_id {
        return Err(tool_failure("rlm live session_id mismatch"));
    }
    let daemon = rlm_live_daemon_owner_status(&root);
    Ok(JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.rlm.live_session.v1".to_string()),
        ),
        (
            "session_id".to_string(),
            JsonValue::String(session_id.to_string()),
        ),
        (
            "status".to_string(),
            JsonValue::String(
                root.get("status")
                    .and_then(json_as_string)
                    .unwrap_or("unknown")
                    .to_string(),
            ),
        ),
        (
            "daemon_pid".to_string(),
            optional_u64_json(&root, "daemon_pid"),
        ),
        (
            "daemon_epoch".to_string(),
            optional_string_json(&root, "daemon_epoch"),
        ),
        (
            "daemon_alive".to_string(),
            daemon.alive.map(JsonValue::Bool).unwrap_or(JsonValue::Null),
        ),
        ("daemon_stale".to_string(), JsonValue::Bool(daemon.stale)),
        (
            "daemon_owner".to_string(),
            JsonValue::String(daemon.owner.to_string()),
        ),
        (
            "runtime_thread_id".to_string(),
            optional_string_json(&root, "runtime_thread_id"),
        ),
        (
            "runtime_session_id".to_string(),
            optional_string_json(&root, "runtime_session_id"),
        ),
        (
            "active_turn_id".to_string(),
            optional_string_json(&root, "active_turn_id"),
        ),
        (
            "queued_turns".to_string(),
            optional_u64_json(&root, "queued_turns"),
        ),
        ("model".to_string(), optional_string_json(&root, "model")),
        (
            "workspace".to_string(),
            optional_string_json(&root, "workspace"),
        ),
        (
            "created_at".to_string(),
            optional_string_json(&root, "created_at"),
        ),
        (
            "updated_at".to_string(),
            optional_string_json(&root, "updated_at"),
        ),
        (
            "last_error".to_string(),
            optional_string_json(&root, "last_error"),
        ),
    ])))
}

#[allow(clippy::too_many_arguments)]
fn write_rlm_live_session_manifest(
    config: &AppConfig,
    session_id: &str,
    status: &str,
    runtime_thread_id: &str,
    runtime_session_id: Option<&str>,
    active_turn_id: Option<&str>,
    queued_turns: u64,
    model: &str,
    workspace: &str,
    created_at: Option<&str>,
) -> AppResult<()> {
    write_rlm_live_session_manifest_with_daemon(
        config,
        session_id,
        status,
        runtime_thread_id,
        runtime_session_id,
        active_turn_id,
        queued_turns,
        model,
        workspace,
        created_at,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_rlm_live_session_manifest_with_daemon(
    config: &AppConfig,
    session_id: &str,
    status: &str,
    runtime_thread_id: &str,
    runtime_session_id: Option<&str>,
    active_turn_id: Option<&str>,
    queued_turns: u64,
    model: &str,
    workspace: &str,
    created_at: Option<&str>,
    daemon_pid: Option<u64>,
    daemon_epoch: Option<&str>,
) -> AppResult<()> {
    let path = rlm_live_session_manifest_path(config, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            tool_failure(format!("rlm live session manifest mkdir failed: {error}"))
        })?;
    }
    let now = rlm_epoch_label();
    let created_at = created_at.unwrap_or(&now);
    let manifest = JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.rlm.live_session.v1".to_string()),
        ),
        (
            "session_id".to_string(),
            JsonValue::String(session_id.to_string()),
        ),
        ("status".to_string(), JsonValue::String(status.to_string())),
        (
            "daemon_pid".to_string(),
            daemon_pid
                .map(|pid| JsonValue::Number(pid.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "daemon_epoch".to_string(),
            daemon_epoch
                .map(|value| JsonValue::String(value.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "runtime_thread_id".to_string(),
            JsonValue::String(runtime_thread_id.to_string()),
        ),
        (
            "runtime_session_id".to_string(),
            runtime_session_id
                .map(|value| JsonValue::String(value.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "active_turn_id".to_string(),
            active_turn_id
                .map(|value| JsonValue::String(value.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "queued_turns".to_string(),
            JsonValue::Number(queued_turns.to_string()),
        ),
        ("model".to_string(), JsonValue::String(model.to_string())),
        (
            "workspace".to_string(),
            JsonValue::String(workspace.to_string()),
        ),
        (
            "created_at".to_string(),
            JsonValue::String(created_at.to_string()),
        ),
        ("updated_at".to_string(), JsonValue::String(now)),
        ("last_error".to_string(), JsonValue::Null),
    ]));
    fs::write(path, json_value_to_string(&manifest)).map_err(|error| {
        tool_failure(format!("rlm live session manifest write failed: {error}"))
    })?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_rlm_live_session_turn_payload(
    config: &AppConfig,
    session_id: &str,
    runtime_thread_id: &str,
    task_id: &str,
    task: &str,
    process_input: &RlmProcessInput,
    steps: &str,
    model: &str,
    workspace: &str,
) -> AppResult<()> {
    let path = rlm_live_session_turn_payload_path(config, session_id, task_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            tool_failure(format!("rlm live turn payload mkdir failed: {error}"))
        })?;
    }
    let now = rlm_epoch_label();
    let payload = JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.rlm.live_turn.v1".to_string()),
        ),
        (
            "session_id".to_string(),
            JsonValue::String(session_id.to_string()),
        ),
        (
            "runtime_thread_id".to_string(),
            JsonValue::String(runtime_thread_id.to_string()),
        ),
        (
            "task_id".to_string(),
            JsonValue::String(task_id.to_string()),
        ),
        (
            "status".to_string(),
            JsonValue::String("queued".to_string()),
        ),
        ("task".to_string(), JsonValue::String(task.to_string())),
        ("steps".to_string(), JsonValue::String(steps.to_string())),
        ("model".to_string(), JsonValue::String(model.to_string())),
        (
            "workspace".to_string(),
            JsonValue::String(workspace.to_string()),
        ),
        (
            "input".to_string(),
            JsonValue::Object(BTreeMap::from([
                (
                    "label".to_string(),
                    JsonValue::String(process_input.label.clone()),
                ),
                (
                    "content".to_string(),
                    JsonValue::String(process_input.content.clone()),
                ),
                (
                    "char_count".to_string(),
                    JsonValue::Number(process_input.char_count.to_string()),
                ),
                (
                    "line_count".to_string(),
                    JsonValue::Number(process_input.line_count.to_string()),
                ),
            ])),
        ),
        ("created_at".to_string(), JsonValue::String(now.clone())),
        ("updated_at".to_string(), JsonValue::String(now)),
    ]));
    fs::write(path, json_value_to_string(&payload))
        .map_err(|error| tool_failure(format!("rlm live turn payload write failed: {error}")))?;
    Ok(())
}

fn read_rlm_live_session_turn_payload(
    config: &AppConfig,
    session_id: &str,
    task_id: &str,
) -> AppResult<RlmLiveTurnPayload> {
    let path = rlm_live_session_turn_payload_path(config, session_id, task_id);
    let content = fs::read_to_string(&path)
        .map_err(|error| tool_failure(format!("rlm live turn payload read failed: {error}")))?;
    let value = parse_json_value(&content)
        .map_err(|error| tool_failure(format!("rlm live turn payload invalid JSON: {error}")))?;
    parse_rlm_live_turn_payload(&value, session_id, task_id)
}

fn parse_rlm_live_turn_payload(
    value: &JsonValue,
    expected_session_id: &str,
    expected_task_id: &str,
) -> AppResult<RlmLiveTurnPayload> {
    let JsonValue::Object(root) = value else {
        return Err(tool_failure("rlm live turn payload must be a JSON object"));
    };
    let session_id = required_json_string(root, "session_id", "rlm live turn payload")?;
    if session_id != expected_session_id {
        return Err(tool_failure(format!(
            "rlm live turn payload session_id `{session_id}` does not match `{expected_session_id}`"
        )));
    }
    let task_id = required_json_string(root, "task_id", "rlm live turn payload")?;
    if task_id != expected_task_id {
        return Err(tool_failure(format!(
            "rlm live turn payload task_id `{task_id}` does not match `{expected_task_id}`"
        )));
    }
    let input_root = match root.get("input") {
        Some(JsonValue::Object(input)) => input,
        _ => {
            return Err(tool_failure(
                "rlm live turn payload requires object field `input`",
            ))
        }
    };
    Ok(RlmLiveTurnPayload {
        session_id: session_id.to_string(),
        runtime_thread_id: required_json_string(
            root,
            "runtime_thread_id",
            "rlm live turn payload",
        )?
        .to_string(),
        task_id: task_id.to_string(),
        status: required_json_string(root, "status", "rlm live turn payload")?.to_string(),
        task: required_json_string(root, "task", "rlm live turn payload")?.to_string(),
        steps: required_json_string(root, "steps", "rlm live turn payload")?.to_string(),
        model: required_json_string(root, "model", "rlm live turn payload")?.to_string(),
        workspace: required_json_string(root, "workspace", "rlm live turn payload")?.to_string(),
        input: RlmProcessInput {
            label: required_json_string(input_root, "label", "rlm live turn payload input")?
                .to_string(),
            content: required_json_string(input_root, "content", "rlm live turn payload input")?
                .to_string(),
            char_count: input_root
                .get("char_count")
                .and_then(json_as_u64)
                .unwrap_or(0) as usize,
            line_count: input_root
                .get("line_count")
                .and_then(json_as_u64)
                .unwrap_or(0) as usize,
        },
    })
}

fn required_json_string<'a>(
    root: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> AppResult<&'a str> {
    root.get(key)
        .and_then(json_as_string)
        .ok_or_else(|| tool_failure(format!("{context} requires string field `{key}`")))
}

fn mark_rlm_live_session_turn_payload_cancelled(
    config: &AppConfig,
    session_id: &str,
    task_id: &str,
    reason: &str,
) -> AppResult<()> {
    update_rlm_live_session_turn_payload_status(
        config,
        session_id,
        task_id,
        "cancelled",
        vec![(
            "cancel_reason".to_string(),
            JsonValue::String(reason.to_string()),
        )],
    )
}

fn update_rlm_live_session_turn_payload_status(
    config: &AppConfig,
    session_id: &str,
    task_id: &str,
    status: &str,
    extra_fields: Vec<(String, JsonValue)>,
) -> AppResult<()> {
    let path = rlm_live_session_turn_payload_path(config, session_id, task_id);
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&path)
        .map_err(|error| tool_failure(format!("rlm live turn payload read failed: {error}")))?;
    let JsonValue::Object(mut root) = parse_json_value(&content)
        .map_err(|error| tool_failure(format!("rlm live turn payload invalid JSON: {error}")))?
    else {
        return Err(tool_failure("rlm live turn payload must be a JSON object"));
    };
    let now = rlm_epoch_label();
    root.insert("status".to_string(), JsonValue::String(status.to_string()));
    root.insert("updated_at".to_string(), JsonValue::String(now.clone()));
    root.insert(format!("{status}_at"), JsonValue::String(now));
    for (key, value) in extra_fields {
        root.insert(key, value);
    }
    fs::write(path, json_value_to_string(&JsonValue::Object(root)))
        .map_err(|error| tool_failure(format!("rlm live turn payload write failed: {error}")))?;
    Ok(())
}

fn append_rlm_live_session_json_event(
    config: &AppConfig,
    session_id: &str,
    kind: &str,
    runtime_thread_id: &str,
    task_id: &str,
    fields: Vec<(String, JsonValue)>,
) -> AppResult<()> {
    let path = rlm_live_session_event_log_path(config, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            tool_failure(format!("rlm live session event mkdir failed: {error}"))
        })?;
    }
    let seq = fs::read_to_string(&path)
        .map(|content| content.lines().count() as u64 + 1)
        .unwrap_or(1);
    let mut root = BTreeMap::from([
        ("seq".to_string(), JsonValue::Number(seq.to_string())),
        (
            "created_at".to_string(),
            JsonValue::String(rlm_epoch_label()),
        ),
        ("kind".to_string(), JsonValue::String(kind.to_string())),
        (
            "runtime_thread_id".to_string(),
            JsonValue::String(runtime_thread_id.to_string()),
        ),
        (
            "task_id".to_string(),
            JsonValue::String(task_id.to_string()),
        ),
    ]);
    for (key, value) in fields {
        root.insert(key, value);
    }
    let mut event = json_value_to_string(&JsonValue::Object(root));
    event.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(event.as_bytes()))
        .map_err(|error| tool_failure(format!("rlm live session event write failed: {error}")))?;
    Ok(())
}

fn rlm_string_map_json(map: &BTreeMap<String, String>) -> JsonValue {
    JsonValue::Object(
        map.iter()
            .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
            .collect(),
    )
}

fn rlm_observation_status_label(status: ObservationStatus) -> &'static str {
    match status {
        ObservationStatus::Ok => "ok",
        ObservationStatus::Failed => "failed",
    }
}

fn append_rlm_live_session_event(
    config: &AppConfig,
    session_id: &str,
    kind: &str,
    runtime_thread_id: &str,
    task_id: &str,
    task: &str,
    input_label: &str,
) -> AppResult<()> {
    let path = rlm_live_session_event_log_path(config, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            tool_failure(format!("rlm live session event mkdir failed: {error}"))
        })?;
    }
    let seq = fs::read_to_string(&path)
        .map(|content| content.lines().count() as u64 + 1)
        .unwrap_or(1);
    let event = format!(
        "{{\"seq\":{},\"created_at\":\"{}\",\"kind\":\"{}\",\"runtime_thread_id\":\"{}\",\"task_id\":\"{}\",\"task\":\"{}\",\"input\":\"{}\"}}\n",
        seq,
        json_escape(&rlm_epoch_label()),
        json_escape(kind),
        json_escape(runtime_thread_id),
        json_escape(task_id),
        json_escape(task),
        json_escape(input_label)
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(event.as_bytes()))
        .map_err(|error| tool_failure(format!("rlm live session event write failed: {error}")))?;
    Ok(())
}

fn append_rlm_live_session_cancel_event(
    config: &AppConfig,
    session_id: &str,
    runtime_thread_id: &str,
    task_id: &str,
    task_summary: &str,
    reason: &str,
) -> AppResult<()> {
    let path = rlm_live_session_event_log_path(config, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            tool_failure(format!("rlm live session event mkdir failed: {error}"))
        })?;
    }
    let seq = fs::read_to_string(&path)
        .map(|content| content.lines().count() as u64 + 1)
        .unwrap_or(1);
    let event = format!(
        "{{\"seq\":{},\"created_at\":\"{}\",\"kind\":\"turn_cancelled\",\"runtime_thread_id\":\"{}\",\"task_id\":\"{}\",\"task\":\"{}\",\"input\":\"queued turn\",\"reason\":\"{}\"}}\n",
        seq,
        json_escape(&rlm_epoch_label()),
        json_escape(runtime_thread_id),
        json_escape(task_id),
        json_escape(task_summary),
        json_escape(reason)
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(event.as_bytes()))
        .map_err(|error| tool_failure(format!("rlm live session event write failed: {error}")))?;
    Ok(())
}

fn append_rlm_live_session_recovery_event(
    config: &AppConfig,
    session_id: &str,
    runtime_thread_id: &str,
    task_id: &str,
    task_summary: &str,
    mode: &str,
    action: &str,
    reason: &str,
) -> AppResult<()> {
    let path = rlm_live_session_event_log_path(config, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            tool_failure(format!("rlm live session event mkdir failed: {error}"))
        })?;
    }
    let seq = fs::read_to_string(&path)
        .map(|content| content.lines().count() as u64 + 1)
        .unwrap_or(1);
    let event = format!(
        "{{\"seq\":{},\"created_at\":\"{}\",\"kind\":\"turn_recovered\",\"runtime_thread_id\":\"{}\",\"task_id\":\"{}\",\"task\":\"{}\",\"mode\":\"{}\",\"action\":\"{}\",\"reason\":\"{}\"}}\n",
        seq,
        json_escape(&rlm_epoch_label()),
        json_escape(runtime_thread_id),
        json_escape(task_id),
        json_escape(task_summary),
        json_escape(mode),
        json_escape(action),
        json_escape(reason)
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(event.as_bytes()))
        .map_err(|error| tool_failure(format!("rlm live session event write failed: {error}")))?;
    Ok(())
}

fn append_rlm_live_session_stop_event(
    config: &AppConfig,
    session_id: &str,
    runtime_thread_id: &str,
    cancelled_count: usize,
    reason: &str,
) -> AppResult<()> {
    let path = rlm_live_session_event_log_path(config, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            tool_failure(format!("rlm live session event mkdir failed: {error}"))
        })?;
    }
    let seq = fs::read_to_string(&path)
        .map(|content| content.lines().count() as u64 + 1)
        .unwrap_or(1);
    let event = format!(
        "{{\"seq\":{},\"created_at\":\"{}\",\"kind\":\"session_stopped\",\"runtime_thread_id\":\"{}\",\"cancelled_count\":{},\"reason\":\"{}\"}}\n",
        seq,
        json_escape(&rlm_epoch_label()),
        json_escape(runtime_thread_id),
        cancelled_count,
        json_escape(reason)
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(event.as_bytes()))
        .map_err(|error| tool_failure(format!("rlm live session event write failed: {error}")))?;
    Ok(())
}

fn ensure_pending_live_rlm_task(task: &TaskRecord, runtime_thread_id: &str) -> AppResult<()> {
    if task.thread_id.as_deref() != Some(runtime_thread_id) {
        return Err(tool_failure(format!(
            "live RLM task {} does not belong to live session thread {runtime_thread_id}",
            task.id
        )));
    }
    if task.kind != "rlm_process" {
        return Err(tool_failure(format!(
            "live RLM task {} has kind `{}` instead of `rlm_process`",
            task.id, task.kind
        )));
    }
    if task.status != "pending" {
        return Err(tool_failure(format!(
            "live RLM controls only target queued pending turns; task {} is `{}`",
            task.id, task.status
        )));
    }
    Ok(())
}

fn ensure_cancelable_live_rlm_task(task: &TaskRecord, runtime_thread_id: &str) -> AppResult<()> {
    if task.thread_id.as_deref() != Some(runtime_thread_id) {
        return Err(tool_failure(format!(
            "live RLM task {} does not belong to live session thread {runtime_thread_id}",
            task.id
        )));
    }
    if task.kind != "rlm_process" {
        return Err(tool_failure(format!(
            "live RLM task {} has kind `{}` instead of `rlm_process`",
            task.id, task.kind
        )));
    }
    if !matches!(task.status.as_str(), "pending" | "running") {
        return Err(tool_failure(format!(
            "live RLM cancel only targets queued pending or active running turns; task {} is `{}`",
            task.id, task.status
        )));
    }
    Ok(())
}

fn is_pending_live_rlm_task(task: &TaskRecord, runtime_thread_id: &str) -> bool {
    task.kind == "rlm_process"
        && task.status == "pending"
        && task.thread_id.as_deref() == Some(runtime_thread_id)
}

fn is_cancelable_live_rlm_task(task: &TaskRecord, runtime_thread_id: &str) -> bool {
    task.kind == "rlm_process"
        && matches!(task.status.as_str(), "pending" | "running")
        && task.thread_id.as_deref() == Some(runtime_thread_id)
}

fn rlm_live_run_next_error_is_cancelled(
    store: &RuntimeStore,
    task_id: &str,
    message: &str,
) -> bool {
    store
        .load_task(task_id)
        .map(|task| task.status == "cancelled")
        .unwrap_or(false)
        || message.contains("agent run cancelled")
}

fn count_pending_live_rlm_tasks(store: &RuntimeStore, runtime_thread_id: &str) -> AppResult<u64> {
    let count = store
        .list_tasks(None, Some(runtime_thread_id), MAX_RLM_LIVE_TASK_LIST)?
        .into_iter()
        .filter(|task| is_pending_live_rlm_task(task, runtime_thread_id))
        .count();
    Ok(count as u64)
}

fn oldest_pending_live_rlm_task(
    store: &RuntimeStore,
    runtime_thread_id: &str,
) -> AppResult<Option<TaskRecord>> {
    Ok(pending_live_rlm_tasks(store, runtime_thread_id)?
        .into_iter()
        .next())
}

fn pending_live_rlm_tasks(
    store: &RuntimeStore,
    runtime_thread_id: &str,
) -> AppResult<Vec<TaskRecord>> {
    let mut tasks = store
        .list_tasks(None, Some(runtime_thread_id), MAX_RLM_LIVE_TASK_LIST)?
        .into_iter()
        .filter(|task| is_pending_live_rlm_task(task, runtime_thread_id))
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(tasks)
}

fn rlm_live_cancelled_task_json(task: &TaskRecord, original_summary: &str) -> JsonValue {
    JsonValue::Object(BTreeMap::from([
        ("task_id".to_string(), JsonValue::String(task.id.clone())),
        ("status".to_string(), JsonValue::String(task.status.clone())),
        (
            "original_summary".to_string(),
            JsonValue::String(original_summary.to_string()),
        ),
        (
            "summary".to_string(),
            JsonValue::String(task.summary.clone()),
        ),
    ]))
}

fn render_rlm_live_session_list_entry(
    session_id: &str,
    path: &Path,
    bytes: u64,
    manifest: &JsonValue,
    include_turns: bool,
    turn_limit: usize,
    config: &AppConfig,
) -> AppResult<String> {
    let JsonValue::Object(root) = manifest else {
        return Err(tool_failure(
            "rlm live session manifest must be a JSON object",
        ));
    };
    let status = json_field_string(root, "status");
    let updated_at = json_field_string(root, "updated_at");
    let daemon_pid = json_field_value(root, "daemon_pid");
    let daemon = rlm_live_daemon_owner_status(root);
    let daemon_alive = daemon
        .alive
        .map(|alive| alive.to_string())
        .unwrap_or_else(|| "null".to_string());
    let runtime_thread_id = json_field_value(root, "runtime_thread_id");
    let active_turn_id = json_field_value(root, "active_turn_id");
    let queued_turns = json_field_value(root, "queued_turns");
    let last_error = json_field_value(root, "last_error");
    let mut rendered = format!(
        "{{\"session_id\":\"{}\",\"path\":\"{}\",\"bytes\":{},\"status\":\"{}\",\"daemon_pid\":{},\"daemon_alive\":{},\"daemon_stale\":{},\"daemon_owner\":\"{}\",\"runtime_thread_id\":{},\"active_turn_id\":{},\"queued_turns\":{},\"updated_at\":\"{}\",\"last_error\":{}}}",
        json_escape(session_id),
        json_escape(&path.display().to_string()),
        bytes,
        json_escape(&status),
        daemon_pid,
        daemon_alive,
        daemon.stale,
        daemon.owner,
        runtime_thread_id,
        active_turn_id,
        queued_turns,
        json_escape(&updated_at),
        last_error
    );
    if include_turns {
        let turns = render_rlm_live_turn_entries(config, session_id, turn_limit)?;
        rendered.pop();
        rendered.push_str(&format!(",\"turns\":[{}]}}", turns));
    }
    Ok(rendered)
}

fn rlm_live_status_json(
    config: &AppConfig,
    session_id: &str,
    path: &Path,
    manifest: &JsonValue,
) -> AppResult<JsonValue> {
    let JsonValue::Object(root) = manifest else {
        return Err(tool_failure(
            "rlm live session manifest must be a JSON object",
        ));
    };
    let store = rlm_runtime_store(config);
    let runtime_thread_id = rlm_live_manifest_string_field(manifest, "runtime_thread_id");
    let active_turn_id = rlm_live_manifest_string_field(manifest, "active_turn_id");
    let daemon = rlm_live_daemon_owner_status(root);
    let status =
        rlm_live_manifest_string_field(manifest, "status").unwrap_or_else(|| "unknown".to_string());

    let mut runtime_task_counts = BTreeMap::new();
    let mut pending_tasks = Vec::new();
    let mut active_runtime_status = None;
    if let Some(thread_id) = runtime_thread_id.as_deref() {
        for task in store.list_tasks(None, Some(thread_id), MAX_RLM_LIVE_TASK_LIST)? {
            if task.kind == "rlm_process" {
                increment_rlm_status_count(&mut runtime_task_counts, &task.status);
                if task.status == "pending" {
                    pending_tasks.push(task);
                }
            }
        }
        pending_tasks.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        if let Some(active_id) = active_turn_id.as_deref() {
            active_runtime_status = store.load_task(active_id).ok().map(|task| task.status);
        }
    }

    let mut turn_counts = BTreeMap::new();
    let mut turn_payload_errors = 0usize;
    for task_id in list_rlm_live_turn_payload_ids(config, session_id)? {
        match read_rlm_live_session_turn_payload(config, session_id, &task_id) {
            Ok(payload) => increment_rlm_status_count(&mut turn_counts, &payload.status),
            Err(_) => turn_payload_errors += 1,
        }
    }

    let queued_turns_runtime = pending_tasks.len() as u64;
    let next_pending_turn_id = pending_tasks.first().map(|task| task.id.clone());
    let recommended_actions = rlm_live_status_recommended_actions(
        session_id,
        &status,
        &daemon,
        active_turn_id.as_deref(),
        queued_turns_runtime,
        &runtime_task_counts,
        &turn_counts,
    );
    Ok(JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.rlm.live_status.v1".to_string()),
        ),
        ("exists".to_string(), JsonValue::Bool(true)),
        ("live".to_string(), JsonValue::Bool(true)),
        (
            "session_id".to_string(),
            JsonValue::String(session_id.to_string()),
        ),
        (
            "path".to_string(),
            JsonValue::String(path.display().to_string()),
        ),
        ("status".to_string(), JsonValue::String(status)),
        (
            "runtime_thread_id".to_string(),
            runtime_thread_id
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "runtime_session_id".to_string(),
            rlm_live_manifest_string_field(manifest, "runtime_session_id")
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "active_turn_id".to_string(),
            active_turn_id
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "active_runtime_status".to_string(),
            active_runtime_status
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "queued_turns_manifest".to_string(),
            rlm_live_manifest_u64_field(manifest, "queued_turns")
                .map(|count| JsonValue::Number(count.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "queued_turns_runtime".to_string(),
            JsonValue::Number(queued_turns_runtime.to_string()),
        ),
        (
            "next_pending_turn_id".to_string(),
            next_pending_turn_id
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "daemon_pid".to_string(),
            daemon
                .pid
                .map(|pid| JsonValue::Number(pid.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "daemon_epoch".to_string(),
            rlm_live_manifest_string_field(manifest, "daemon_epoch")
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "daemon_alive".to_string(),
            daemon.alive.map(JsonValue::Bool).unwrap_or(JsonValue::Null),
        ),
        ("daemon_stale".to_string(), JsonValue::Bool(daemon.stale)),
        (
            "daemon_owner".to_string(),
            JsonValue::String(daemon.owner.to_string()),
        ),
        (
            "turn_counts".to_string(),
            rlm_status_counts_json(&turn_counts),
        ),
        (
            "runtime_task_counts".to_string(),
            rlm_status_counts_json(&runtime_task_counts),
        ),
        (
            "turn_payload_errors".to_string(),
            JsonValue::Number(turn_payload_errors.to_string()),
        ),
        (
            "recommended_actions".to_string(),
            JsonValue::Array(
                recommended_actions
                    .into_iter()
                    .map(JsonValue::String)
                    .collect(),
            ),
        ),
        (
            "updated_at".to_string(),
            rlm_live_manifest_string_field(manifest, "updated_at")
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "last_error".to_string(),
            root.get("last_error").cloned().unwrap_or(JsonValue::Null),
        ),
    ])))
}

fn rlm_live_status_recommended_actions(
    session_id: &str,
    status: &str,
    daemon: &RlmLiveDaemonOwner,
    active_turn_id: Option<&str>,
    queued_turns_runtime: u64,
    runtime_task_counts: &BTreeMap<String, usize>,
    turn_counts: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut actions = Vec::new();
    if status == "stopped" {
        actions.push(format!(
            "rlm_process live=true session_id={session_id} reset=true to restart"
        ));
        return actions;
    }
    if daemon.stale
        || status_count(runtime_task_counts, "running") > 0 && daemon.alive == Some(false)
        || status_count(turn_counts, "running") > 0 && daemon.alive == Some(false)
    {
        actions.push(format!("rlm_process_recover session_id={session_id}"));
    }
    if active_turn_id.is_some() && daemon.alive == Some(true) {
        actions.push(format!("rlm_process_wait session_id={session_id}"));
    }
    if queued_turns_runtime > 0 {
        actions.push(format!("rlm_process_run_next session_id={session_id}"));
        actions.push("deepseek agents daemon".to_string());
    }
    if queued_turns_runtime > 1 {
        actions.push(format!("rlm_process_drain session_id={session_id}"));
    }
    if active_turn_id.is_none() && queued_turns_runtime == 0 && status != "failed" {
        actions.push(format!(
            "rlm_process live=true session_id={session_id} to enqueue a turn"
        ));
    }
    actions
}

fn rlm_live_status_totals_json(sessions: &[JsonValue]) -> JsonValue {
    let mut live_sessions = 0u64;
    let mut running_sessions = 0u64;
    let mut stopped_sessions = 0u64;
    let mut stale_daemon_sessions = 0u64;
    let mut queued_turns = 0u64;
    let mut running_turns = 0u64;
    for session in sessions {
        let JsonValue::Object(root) = session else {
            continue;
        };
        live_sessions += 1;
        if root
            .get("status")
            .and_then(json_as_string)
            .is_some_and(|value| value == "running")
        {
            running_sessions += 1;
        }
        if root
            .get("status")
            .and_then(json_as_string)
            .is_some_and(|value| value == "stopped")
        {
            stopped_sessions += 1;
        }
        if matches!(root.get("daemon_stale"), Some(JsonValue::Bool(true))) {
            stale_daemon_sessions += 1;
        }
        queued_turns += root
            .get("queued_turns_runtime")
            .and_then(json_as_u64)
            .unwrap_or(0);
        running_turns += root
            .get("runtime_task_counts")
            .and_then(|value| rlm_count_from_json(value, "running"))
            .unwrap_or(0);
    }
    JsonValue::Object(BTreeMap::from([
        (
            "live_sessions".to_string(),
            JsonValue::Number(live_sessions.to_string()),
        ),
        (
            "running_sessions".to_string(),
            JsonValue::Number(running_sessions.to_string()),
        ),
        (
            "stopped_sessions".to_string(),
            JsonValue::Number(stopped_sessions.to_string()),
        ),
        (
            "stale_daemon_sessions".to_string(),
            JsonValue::Number(stale_daemon_sessions.to_string()),
        ),
        (
            "queued_turns".to_string(),
            JsonValue::Number(queued_turns.to_string()),
        ),
        (
            "running_turns".to_string(),
            JsonValue::Number(running_turns.to_string()),
        ),
    ]))
}

fn increment_rlm_status_count(counts: &mut BTreeMap<String, usize>, status: &str) {
    *counts.entry(status.to_string()).or_insert(0) += 1;
}

fn rlm_status_counts_json(counts: &BTreeMap<String, usize>) -> JsonValue {
    let mut root = BTreeMap::new();
    let total = counts.values().sum::<usize>();
    root.insert("total".to_string(), JsonValue::Number(total.to_string()));
    for (status, count) in counts {
        root.insert(status.clone(), JsonValue::Number(count.to_string()));
    }
    JsonValue::Object(root)
}

fn status_count(counts: &BTreeMap<String, usize>, status: &str) -> usize {
    counts.get(status).copied().unwrap_or(0)
}

fn rlm_count_from_json(value: &JsonValue, status: &str) -> Option<u64> {
    let JsonValue::Object(root) = value else {
        return None;
    };
    root.get(status).and_then(json_as_u64)
}

fn render_rlm_live_turn_entries(
    config: &AppConfig,
    session_id: &str,
    limit: usize,
) -> AppResult<String> {
    let turns_dir = rlm_live_session_turns_dir(config, session_id);
    if !turns_dir.exists() {
        return Ok(String::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&turns_dir)
        .map_err(|error| tool_failure(format!("rlm live turns read_dir failed: {error}")))?
    {
        let entry = entry
            .map_err(|error| tool_failure(format!("rlm live turns read entry failed: {error}")))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)
            .map_err(|error| tool_failure(format!("rlm live turn payload read failed: {error}")))?;
        let value = parse_json_value(&content).map_err(|error| {
            tool_failure(format!("rlm live turn payload invalid JSON: {error}"))
        })?;
        let JsonValue::Object(root) = &value else {
            return Err(tool_failure("rlm live turn payload must be a JSON object"));
        };
        let created_at = root
            .get("created_at")
            .and_then(json_as_string)
            .unwrap_or("")
            .to_string();
        let task_id = root
            .get("task_id")
            .and_then(json_as_string)
            .unwrap_or("")
            .to_string();
        entries.push((created_at, task_id, path, value));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut out = String::new();
    for (_created_at, _task_id, path, value) in entries.into_iter().take(limit) {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(&json_value_to_string(&rlm_live_turn_inventory_json(
            config, session_id, &path, &value,
        )?));
    }
    Ok(out)
}

fn rlm_live_turn_inventory_json(
    config: &AppConfig,
    session_id: &str,
    path: &Path,
    value: &JsonValue,
) -> AppResult<JsonValue> {
    let JsonValue::Object(root) = value else {
        return Err(tool_failure("rlm live turn payload must be a JSON object"));
    };
    let task_id = required_json_string(root, "task_id", "rlm live turn payload")?;
    let payload_session_id = required_json_string(root, "session_id", "rlm live turn payload")?;
    if payload_session_id != session_id {
        return Err(tool_failure(format!(
            "rlm live turn payload session_id `{payload_session_id}` does not match `{session_id}`"
        )));
    }
    let runtime_task = rlm_runtime_store(config).load_task(task_id).ok();
    let input_root = match root.get("input") {
        Some(JsonValue::Object(input)) => Some(input),
        _ => None,
    };
    let (result_preview, result_chars, result_truncated) =
        rlm_preview_json(root.get("result_summary").and_then(json_as_string));
    let (error_preview, error_chars, error_truncated) =
        rlm_preview_json(root.get("error").and_then(json_as_string));
    Ok(JsonValue::Object(BTreeMap::from([
        (
            "task_id".to_string(),
            JsonValue::String(task_id.to_string()),
        ),
        (
            "path".to_string(),
            JsonValue::String(path.display().to_string()),
        ),
        (
            "bytes".to_string(),
            JsonValue::Number(
                fs::metadata(path)
                    .map(|meta| meta.len())
                    .unwrap_or(0)
                    .to_string(),
            ),
        ),
        (
            "status".to_string(),
            root.get("status").cloned().unwrap_or(JsonValue::Null),
        ),
        (
            "runtime_status".to_string(),
            runtime_task
                .as_ref()
                .map(|task| JsonValue::String(task.status.clone()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "runtime_updated_at".to_string(),
            runtime_task
                .as_ref()
                .map(|task| JsonValue::String(task.updated_at.clone()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "task".to_string(),
            root.get("task").cloned().unwrap_or(JsonValue::Null),
        ),
        (
            "steps".to_string(),
            root.get("steps").cloned().unwrap_or(JsonValue::Null),
        ),
        (
            "model".to_string(),
            root.get("model").cloned().unwrap_or(JsonValue::Null),
        ),
        (
            "workspace".to_string(),
            root.get("workspace").cloned().unwrap_or(JsonValue::Null),
        ),
        (
            "created_at".to_string(),
            root.get("created_at").cloned().unwrap_or(JsonValue::Null),
        ),
        (
            "updated_at".to_string(),
            root.get("updated_at").cloned().unwrap_or(JsonValue::Null),
        ),
        (
            "input_label".to_string(),
            input_root
                .and_then(|input| input.get("label").cloned())
                .unwrap_or(JsonValue::Null),
        ),
        (
            "input_chars".to_string(),
            input_root
                .and_then(|input| input.get("char_count").cloned())
                .unwrap_or(JsonValue::Null),
        ),
        (
            "input_lines".to_string(),
            input_root
                .and_then(|input| input.get("line_count").cloned())
                .unwrap_or(JsonValue::Null),
        ),
        (
            "cancel_reason".to_string(),
            root.get("cancel_reason")
                .cloned()
                .unwrap_or(JsonValue::Null),
        ),
        ("result_summary_preview".to_string(), result_preview),
        ("result_summary_chars".to_string(), result_chars),
        ("result_summary_truncated".to_string(), result_truncated),
        ("error_preview".to_string(), error_preview),
        ("error_chars".to_string(), error_chars),
        ("error_truncated".to_string(), error_truncated),
    ])))
}

fn rlm_preview_json(raw: Option<&str>) -> (JsonValue, JsonValue, JsonValue) {
    let Some(raw) = raw else {
        return (JsonValue::Null, JsonValue::Null, JsonValue::Bool(false));
    };
    let chars = raw.chars().count();
    let truncated = chars > MAX_RLM_LIVE_TURN_PREVIEW_CHARS;
    let preview = if truncated {
        let mut out = raw
            .chars()
            .take(MAX_RLM_LIVE_TURN_PREVIEW_CHARS)
            .collect::<String>();
        out.push_str("\n...[truncated]");
        out
    } else {
        raw.to_string()
    };
    (
        JsonValue::String(preview),
        JsonValue::Number(chars.to_string()),
        JsonValue::Bool(truncated),
    )
}

fn list_rlm_live_session_manifest_entries(config: &AppConfig) -> AppResult<Vec<(String, PathBuf)>> {
    let sessions_dir = rlm_live_sessions_dir(config);
    let mut entries = Vec::new();
    if !sessions_dir.exists() {
        return Ok(entries);
    }
    for entry in fs::read_dir(&sessions_dir)
        .map_err(|error| tool_failure(format!("rlm live sessions read_dir failed: {error}")))?
    {
        let entry = entry.map_err(|error| {
            tool_failure(format!("rlm live sessions read entry failed: {error}"))
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(session_id) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if validate_rlm_model_session_id(session_id).is_err() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if manifest_path.is_file() {
            entries.push((session_id.to_string(), manifest_path));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

pub(crate) fn rlm_live_session_ids_by_runtime_thread(
    config: &AppConfig,
) -> AppResult<BTreeMap<String, String>> {
    let mut sessions = BTreeMap::new();
    for (session_id, path) in list_rlm_live_session_manifest_entries(config)? {
        let Ok(manifest) = read_rlm_live_session_manifest(&path, &session_id) else {
            continue;
        };
        let Some(runtime_thread_id) =
            rlm_live_manifest_string_field(&manifest, "runtime_thread_id")
        else {
            continue;
        };
        sessions.insert(runtime_thread_id, session_id);
    }
    Ok(sessions)
}

fn rlm_live_session_event_log_path(config: &AppConfig, session_id: &str) -> PathBuf {
    rlm_live_sessions_dir(config)
        .join(session_id)
        .join("events.jsonl")
}

fn rlm_live_session_turn_payload_path(
    config: &AppConfig,
    session_id: &str,
    task_id: &str,
) -> PathBuf {
    rlm_live_session_turns_dir(config, session_id).join(format!("{task_id}.json"))
}

fn rlm_live_session_turns_dir(config: &AppConfig, session_id: &str) -> PathBuf {
    rlm_live_sessions_dir(config).join(session_id).join("turns")
}

fn list_rlm_live_turn_payload_ids(config: &AppConfig, session_id: &str) -> AppResult<Vec<String>> {
    let turns_dir = rlm_live_session_turns_dir(config, session_id);
    if !turns_dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&turns_dir)
        .map_err(|error| tool_failure(format!("rlm live turns read_dir failed: {error}")))?
    {
        let entry = entry
            .map_err(|error| tool_failure(format!("rlm live turns read entry failed: {error}")))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        ids.push(stem.to_string());
    }
    ids.sort();
    Ok(ids)
}

fn rlm_live_manifest_string_field(manifest: &JsonValue, key: &str) -> Option<String> {
    let JsonValue::Object(root) = manifest else {
        return None;
    };
    root.get(key).and_then(json_as_string).map(str::to_string)
}

fn rlm_live_manifest_u64_field(manifest: &JsonValue, key: &str) -> Option<u64> {
    let JsonValue::Object(root) = manifest else {
        return None;
    };
    root.get(key).and_then(json_as_u64)
}

fn rlm_live_daemon_owner_status(root: &BTreeMap<String, JsonValue>) -> RlmLiveDaemonOwner {
    let pid = root.get("daemon_pid").and_then(json_as_u64);
    let status = root.get("status").and_then(json_as_string).unwrap_or("");
    let alive = pid.map(rlm_process_is_alive);
    let stale = status == "running" && alive == Some(false);
    let owner = match (pid, alive) {
        (None, _) => "none",
        (Some(pid), Some(true)) if pid == std::process::id() as u64 => "current",
        (Some(_), Some(true)) => "external",
        (Some(_), Some(false)) => "stale",
        (Some(_), None) => "unknown",
    };
    RlmLiveDaemonOwner {
        pid,
        alive,
        stale,
        owner,
    }
}

fn rlm_live_daemon_owner_status_from_manifest(manifest: &JsonValue) -> RlmLiveDaemonOwner {
    let JsonValue::Object(root) = manifest else {
        return RlmLiveDaemonOwner {
            pid: None,
            alive: None,
            stale: false,
            owner: "none",
        };
    };
    rlm_live_daemon_owner_status(root)
}

fn rlm_process_is_alive(pid: u64) -> bool {
    if pid <= 1 || pid > i32::MAX as u64 {
        return false;
    }
    #[cfg(unix)]
    {
        if rlm_process_is_zombie(pid) {
            return false;
        }
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let result = unsafe { kill(pid as i32, 0) };
        if result == 0 {
            return true;
        }
        matches!(std::io::Error::last_os_error().raw_os_error(), Some(1))
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn rlm_interrupt_live_daemon_owner(pid: Option<u64>) -> (bool, JsonValue) {
    let Some(pid) = pid else {
        return (
            false,
            JsonValue::Object(BTreeMap::from([
                ("attempted".to_string(), JsonValue::Bool(false)),
                ("interrupted".to_string(), JsonValue::Bool(false)),
                (
                    "error".to_string(),
                    JsonValue::String("live manifest has no daemon_pid".to_string()),
                ),
            ])),
        );
    };
    if pid <= 1 || pid == std::process::id() as u64 || pid > i32::MAX as u64 {
        return (
            false,
            JsonValue::Object(BTreeMap::from([
                ("attempted".to_string(), JsonValue::Bool(false)),
                ("interrupted".to_string(), JsonValue::Bool(false)),
                ("pid".to_string(), JsonValue::Number(pid.to_string())),
                (
                    "error".to_string(),
                    JsonValue::String(format!("refusing to interrupt unsafe daemon pid {pid}")),
                ),
            ])),
        );
    }
    if !rlm_process_is_alive(pid) {
        return (
            false,
            JsonValue::Object(BTreeMap::from([
                ("attempted".to_string(), JsonValue::Bool(false)),
                ("interrupted".to_string(), JsonValue::Bool(false)),
                ("pid".to_string(), JsonValue::Number(pid.to_string())),
                (
                    "error".to_string(),
                    JsonValue::String(format!("daemon pid {pid} is not alive")),
                ),
            ])),
        );
    }
    #[cfg(unix)]
    {
        const SIGTERM: i32 = 15;
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let result = unsafe { kill(pid as i32, SIGTERM) };
        if result == 0 {
            return (
                true,
                JsonValue::Object(BTreeMap::from([
                    ("attempted".to_string(), JsonValue::Bool(true)),
                    ("interrupted".to_string(), JsonValue::Bool(true)),
                    ("pid".to_string(), JsonValue::Number(pid.to_string())),
                    (
                        "signal".to_string(),
                        JsonValue::String("SIGTERM".to_string()),
                    ),
                ])),
            );
        }
        (
            false,
            JsonValue::Object(BTreeMap::from([
                ("attempted".to_string(), JsonValue::Bool(true)),
                ("interrupted".to_string(), JsonValue::Bool(false)),
                ("pid".to_string(), JsonValue::Number(pid.to_string())),
                (
                    "signal".to_string(),
                    JsonValue::String("SIGTERM".to_string()),
                ),
                (
                    "error".to_string(),
                    JsonValue::String(format!("{}", std::io::Error::last_os_error())),
                ),
            ])),
        )
    }
    #[cfg(not(unix))]
    {
        (
            false,
            JsonValue::Object(BTreeMap::from([
                ("attempted".to_string(), JsonValue::Bool(false)),
                ("interrupted".to_string(), JsonValue::Bool(false)),
                ("pid".to_string(), JsonValue::Number(pid.to_string())),
                (
                    "error".to_string(),
                    JsonValue::String(
                        "forced live RLM worker interruption is only supported on Unix".to_string(),
                    ),
                ),
            ])),
        )
    }
}

#[cfg(unix)]
fn rlm_process_is_zombie(pid: u64) -> bool {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let Some(after_name) = stat.rsplit_once(") ") else {
        return false;
    };
    after_name.1.starts_with("Z ")
}

fn rlm_runtime_store(config: &AppConfig) -> RuntimeStore {
    RuntimeStore::new(PathBuf::from(&config.workspace.config_dir).join("runtime"))
}

fn default_rlm_workspace() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn optional_string_json(root: &BTreeMap<String, JsonValue>, key: &str) -> JsonValue {
    root.get(key)
        .and_then(json_as_string)
        .map(|value| JsonValue::String(value.to_string()))
        .unwrap_or(JsonValue::Null)
}

fn optional_u64_json(root: &BTreeMap<String, JsonValue>, key: &str) -> JsonValue {
    root.get(key)
        .and_then(json_as_u64)
        .map(|value| JsonValue::Number(value.to_string()))
        .unwrap_or(JsonValue::Null)
}

fn json_field_value(root: &BTreeMap<String, JsonValue>, key: &str) -> String {
    root.get(key)
        .map(json_value_to_string)
        .unwrap_or_else(|| "null".to_string())
}

fn json_field_string(root: &BTreeMap<String, JsonValue>, key: &str) -> String {
    root.get(key)
        .and_then(json_as_string)
        .unwrap_or("null")
        .to_string()
}

fn rlm_epoch_label() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("epoch+{secs}")
}

fn clip_rlm_model_session_summary(value: &str) -> String {
    if value.chars().count() <= MAX_RLM_MODEL_SESSION_SUMMARY_CHARS {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(MAX_RLM_MODEL_SESSION_SUMMARY_CHARS)
        .collect::<String>();
    out.push_str("\n...[truncated]");
    out
}

fn parse_rlm_chunk_chars(raw: Option<&str>) -> AppResult<usize> {
    let chars = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_chunk_plan max_chars must be a positive integer"))?,
        None => DEFAULT_RLM_CHUNK_CHARS,
    };
    Ok(chars.clamp(MIN_RLM_CHUNK_CHARS, MAX_RLM_CHUNK_CHARS))
}

fn parse_rlm_chunk_overlap(raw: Option<&str>, max_chars: usize) -> AppResult<usize> {
    let overlap = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_chunk_plan overlap must be a non-negative integer"))?,
        None => 0,
    };
    if overlap >= max_chars {
        return Err(tool_failure(
            "rlm_chunk_plan overlap must be smaller than max_chars",
        ));
    }
    Ok(overlap)
}

fn parse_rlm_chunk_include_text(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

fn parse_rlm_map_limit(raw: Option<&str>) -> AppResult<usize> {
    let limit = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.parse::<usize>().map_err(|_| {
            tool_failure("rlm_map_reduce_plan map_limit must be a positive integer")
        })?,
        None => MAX_RLM_BATCH_QUESTIONS,
    };
    Ok(limit.clamp(1, MAX_RLM_BATCH_QUESTIONS))
}

fn parse_rlm_recursive_fan_in(raw: Option<&str>) -> AppResult<usize> {
    let fan_in = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_recursive_plan fan_in must be a positive integer"))?,
        None => DEFAULT_RLM_RECURSIVE_FAN_IN,
    };
    Ok(fan_in.clamp(2, MAX_RLM_RECURSIVE_FAN_IN))
}

fn render_rlm_chunk_plan(
    input: &RlmProcessInput,
    max_chars: usize,
    overlap: usize,
    include_text: bool,
) -> AppResult<String> {
    let chunks = build_rlm_chunks(input, max_chars, overlap)?;
    Ok(format!(
        "{{\"source\":{},\"settings\":{{\"max_chars\":{},\"overlap\":{},\"include_text\":{}}},\"coverage\":{},\"chunks\":[{}]}}",
        render_rlm_source_json(input),
        max_chars,
        overlap,
        include_text,
        render_rlm_coverage_json(input.char_count, chunks.len()),
        render_rlm_chunks_json(&chunks, include_text)
    ))
}

#[allow(clippy::too_many_arguments)]
fn render_rlm_recursive_plan(
    input: &RlmProcessInput,
    task: &str,
    max_chars: usize,
    overlap: usize,
    include_text: bool,
    map_limit: usize,
    fan_in: usize,
    steps: &str,
) -> AppResult<String> {
    let chunks = build_rlm_chunks(input, max_chars, overlap)?;
    let selected = chunks.len().min(map_limit);
    let omitted = chunks.len().saturating_sub(selected);
    let mut map_tasks = String::new();
    for (offset, chunk) in chunks.iter().take(map_limit).enumerate() {
        if offset > 0 {
            map_tasks.push(',');
        }
        map_tasks.push_str(&format!(
            "{{\"ref\":\"map:{}\",\"chunk_index\":{},\"start\":{},\"end\":{},\"steps\":\"{}\",\"task\":\"{}\"}}",
            chunk.index,
            chunk.index,
            chunk.start,
            chunk.end,
            json_escape(steps),
            json_escape(&render_rlm_map_task(
                task,
                chunk,
                chunks.len(),
                include_text
            ))
        ));
    }

    let mut current_refs = chunks
        .iter()
        .map(|chunk| format!("map:{}", chunk.index))
        .collect::<Vec<_>>();
    let mut rounds_json = String::new();
    let mut round_index = 1usize;
    while current_refs.len() > 1 {
        let input_count = current_refs.len();
        let group_count = input_count.div_ceil(fan_in);
        let mut groups_json = String::new();
        let mut next_refs = Vec::with_capacity(group_count);
        for group_index in 0..group_count {
            let start = group_index * fan_in;
            let end = input_count.min(start + fan_in);
            let inputs = &current_refs[start..end];
            let output_ref = format!("round{round_index}:group{group_index}");
            let final_round = group_count == 1;
            if group_index > 0 {
                groups_json.push(',');
            }
            groups_json.push_str(&format!(
                "{{\"group_index\":{},\"input_refs\":[{}],\"output_ref\":\"{}\",\"steps\":\"{}\",\"task\":\"{}\"}}",
                group_index,
                render_json_string_array(inputs),
                json_escape(&output_ref),
                json_escape(steps),
                json_escape(&render_rlm_recursive_reduce_task(
                    task,
                    round_index,
                    group_index,
                    inputs,
                    final_round
                ))
            ));
            next_refs.push(output_ref);
        }
        if round_index > 1 {
            rounds_json.push(',');
        }
        rounds_json.push_str(&format!(
            "{{\"round\":{},\"input_count\":{},\"fan_in\":{},\"group_count\":{},\"groups\":[{}]}}",
            round_index, input_count, fan_in, group_count, groups_json
        ));
        current_refs = next_refs;
        round_index += 1;
    }
    let final_output_ref = current_refs
        .first()
        .cloned()
        .unwrap_or_else(|| "none".to_string());
    Ok(format!(
        "{{\"source\":{},\"task\":\"{}\",\"settings\":{{\"max_chars\":{},\"overlap\":{},\"include_text\":{},\"map_limit\":{},\"fan_in\":{},\"steps\":\"{}\"}},\"coverage\":{},\"chunks\":[{}],\"map_tasks\":[{}],\"map_tasks_omitted\":{},\"rounds\":[{}],\"final_output_ref\":\"{}\"}}",
        render_rlm_source_json(input),
        json_escape(task),
        max_chars,
        overlap,
        include_text,
        map_limit,
        fan_in,
        json_escape(steps),
        render_rlm_coverage_json(input.char_count, chunks.len()),
        render_rlm_chunks_json(&chunks, include_text),
        map_tasks,
        omitted,
        rounds_json,
        json_escape(&final_output_ref)
    ))
}

fn render_rlm_map_reduce_plan(
    input: &RlmProcessInput,
    task: &str,
    max_chars: usize,
    overlap: usize,
    include_text: bool,
    map_limit: usize,
    steps: &str,
) -> AppResult<String> {
    let chunks = build_rlm_chunks(input, max_chars, overlap)?;
    let selected = chunks.len().min(map_limit);
    let mut map_tasks = String::new();
    for (offset, chunk) in chunks.iter().take(map_limit).enumerate() {
        if offset > 0 {
            map_tasks.push(',');
        }
        map_tasks.push_str(&format!(
            "{{\"chunk_index\":{},\"start\":{},\"end\":{},\"steps\":\"{}\",\"task\":\"{}\"}}",
            chunk.index,
            chunk.start,
            chunk.end,
            json_escape(steps),
            json_escape(&render_rlm_map_task(
                task,
                chunk,
                chunks.len(),
                include_text
            ))
        ));
    }
    let omitted = chunks.len().saturating_sub(selected);
    Ok(format!(
        "{{\"source\":{},\"task\":\"{}\",\"settings\":{{\"max_chars\":{},\"overlap\":{},\"include_text\":{},\"map_limit\":{},\"steps\":\"{}\"}},\"coverage\":{},\"chunks\":[{}],\"map_tasks\":[{}],\"map_tasks_omitted\":{},\"reduce_task\":\"{}\"}}",
        render_rlm_source_json(input),
        json_escape(task),
        max_chars,
        overlap,
        include_text,
        map_limit,
        json_escape(steps),
        render_rlm_coverage_json(input.char_count, chunks.len()),
        render_rlm_chunks_json(&chunks, include_text),
        map_tasks,
        omitted,
        json_escape(&render_rlm_reduce_task(task, chunks.len(), omitted))
    ))
}

fn build_rlm_chunks(
    input: &RlmProcessInput,
    max_chars: usize,
    overlap: usize,
) -> AppResult<Vec<RlmChunk>> {
    let byte_offsets = char_byte_offsets(&input.content);
    let total_chars = byte_offsets.len().saturating_sub(1);
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;

    while start < total_chars || (total_chars == 0 && index == 0) {
        if index >= MAX_RLM_CHUNKS {
            return Err(tool_failure(format!(
                "rlm_chunk_plan would create more than {MAX_RLM_CHUNKS} chunks; increase max_chars or reduce overlap"
            )));
        }
        let end = total_chars.min(start + max_chars);
        let byte_start = byte_offsets[start];
        let byte_end = byte_offsets[end];
        chunks.push(RlmChunk {
            index,
            start,
            end,
            text: input.content[byte_start..byte_end].to_string(),
        });
        index += 1;
        if end >= total_chars {
            break;
        }
        start = end - overlap;
    }
    Ok(chunks)
}

fn render_rlm_source_json(input: &RlmProcessInput) -> String {
    format!(
        "{{\"label\":\"{}\",\"chars\":{},\"lines\":{}}}",
        json_escape(&input.label),
        input.char_count,
        input.line_count
    )
}

fn render_rlm_coverage_json(context_chars: usize, chunks: usize) -> String {
    format!(
        "{{\"chunks\":{},\"context_chars\":{},\"covered_chars\":{},\"gaps\":[],\"complete\":true}}",
        chunks, context_chars, context_chars
    )
}

fn render_rlm_chunks_json(chunks: &[RlmChunk], include_text: bool) -> String {
    let mut chunks_json = String::new();
    for (offset, chunk) in chunks.iter().enumerate() {
        if offset > 0 {
            chunks_json.push(',');
        }
        chunks_json.push_str(&format!(
            "{{\"index\":{},\"start\":{},\"end\":{},\"chars\":{}",
            chunk.index,
            chunk.start,
            chunk.end,
            chunk.end.saturating_sub(chunk.start)
        ));
        if include_text {
            chunks_json.push_str(&format!(",\"text\":\"{}\"", json_escape(&chunk.text)));
        } else {
            chunks_json.push_str(",\"text\":null");
        }
        chunks_json.push('}');
    }
    chunks_json
}

fn render_rlm_map_task(
    task: &str,
    chunk: &RlmChunk,
    total_chunks: usize,
    include_text: bool,
) -> String {
    let chunk_text = if include_text {
        format!("\nChunk text:\n{}", chunk.text)
    } else {
        "\nChunk text omitted by include_text=false; use the chunk offsets to fetch this slice before running the map step.".to_string()
    };
    format!(
        "RLM map step\n\
Objective: {task}\n\
Chunk: {} of {total_chunks}\n\
Offsets: chars {}..{}\n\n\
Analyze only this chunk. Extract facts, evidence, partial answers, and any unresolved questions relevant to the objective. Preserve source-specific details for reduction.{chunk_text}",
        chunk.index + 1,
        chunk.start,
        chunk.end
    )
}

fn render_rlm_reduce_task(task: &str, chunk_count: usize, omitted: usize) -> String {
    let omitted_note = if omitted == 0 {
        "All planned chunk map tasks are included.".to_string()
    } else {
        format!("{omitted} chunk map task(s) were omitted by map_limit; create additional map batches before final reduction.")
    };
    format!(
        "RLM reduce step\n\
Objective: {task}\n\
Expected map outputs: {chunk_count}\n\
{omitted_note}\n\n\
Combine the chunk-level map outputs into one answer. Reconcile duplicate or conflicting facts, cite chunk indexes or offsets for key evidence, call out uncovered/omitted chunks, and state residual uncertainty."
    )
}

fn render_rlm_recursive_reduce_task(
    task: &str,
    round_index: usize,
    group_index: usize,
    inputs: &[String],
    final_round: bool,
) -> String {
    let final_note = if final_round {
        "This reduce group produces the final answer."
    } else {
        "This reduce group produces an intermediate summary for a later recursive reduce round."
    };
    format!(
        "RLM recursive reduce step\n\
Objective: {task}\n\
Round: {round_index}\n\
Group: {}\n\
Inputs: {}\n\
{final_note}\n\n\
Combine only the referenced input summaries. Preserve concrete evidence, source offsets, contradictions, and unresolved questions. If an input is missing, mark it as uncovered instead of guessing.",
        group_index + 1,
        inputs.join(", ")
    )
}

fn render_json_string_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect::<Vec<_>>()
        .join(",")
}

fn char_byte_offsets(content: &str) -> Vec<usize> {
    let mut offsets: Vec<usize> = content.char_indices().map(|(index, _)| index).collect();
    offsets.push(content.len());
    offsets
}

fn validate_rlm_python_code(code: &str) -> AppResult<()> {
    if code.len() > MAX_RLM_PYTHON_CODE_BYTES {
        return Err(tool_failure(format!(
            "rlm_python code must be at most {MAX_RLM_PYTHON_CODE_BYTES} bytes"
        )));
    }
    let lower = code.to_ascii_lowercase();
    for token in [
        "__",
        "import ",
        "from ",
        "open(",
        "exec(",
        "eval(",
        "compile(",
        "globals(",
        "locals(",
        "input(",
        "breakpoint(",
        "help(",
        "dir(",
        "getattr(",
        "setattr(",
        "delattr(",
        "type(",
        "super(",
        "object",
        "class ",
        "subprocess",
        "socket",
        "pathlib",
        "requests",
        "os.",
        "sys.",
    ] {
        if lower.contains(token) {
            return Err(tool_failure(format!(
                "rlm_python rejects blocked token `{token}`"
            )));
        }
    }
    Ok(())
}

fn validate_rlm_python_session_id(session_id: &str) -> AppResult<()> {
    let valid = session_id.len() <= 64
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        && !session_id.starts_with('.')
        && !session_id.contains("..");
    if valid {
        Ok(())
    } else {
        Err(tool_failure(
            "rlm_python_session session_id must use 1-64 chars of [A-Za-z0-9_.-] without leading dot or `..`",
        ))
    }
}

fn parse_rlm_python_timeout(raw: Option<&str>) -> AppResult<Duration> {
    let millis = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.parse::<u64>().map_err(|_| {
            tool_failure("rlm_python timeout_ms must be a positive integer milliseconds value")
        })?,
        None => DEFAULT_RLM_PYTHON_TIMEOUT_MS,
    };
    Ok(Duration::from_millis(
        millis.clamp(100, MAX_RLM_PYTHON_TIMEOUT_MS),
    ))
}

fn parse_rlm_python_sessions_limit(raw: Option<&str>) -> AppResult<usize> {
    let limit = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_python_sessions limit must be a positive integer"))?,
        None => 20,
    };
    Ok(limit.clamp(1, 100))
}

fn parse_rlm_model_sessions_limit(raw: Option<&str>) -> AppResult<usize> {
    let limit = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_process_sessions limit must be a positive integer"))?,
        None => 20,
    };
    Ok(limit.clamp(1, 100))
}

fn parse_rlm_live_events_limit(raw: Option<&str>) -> AppResult<usize> {
    let limit = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_process_events limit must be a positive integer"))?,
        None => 50,
    };
    Ok(limit.clamp(1, 500))
}

fn parse_rlm_live_event_cursor(raw: Option<&str>) -> AppResult<u64> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| tool_failure("rlm_process_events cursor must be a non-negative integer")),
        None => Ok(0),
    }
}

fn parse_rlm_live_wait_timeout(raw: Option<&str>) -> AppResult<Duration> {
    let millis = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.parse::<u64>().map_err(|_| {
            tool_failure("rlm_process_wait timeout_ms must be a non-negative integer")
        })?,
        None => 1_000,
    };
    Ok(Duration::from_millis(millis.min(30_000)))
}

fn parse_rlm_live_wait_poll_interval(raw: Option<&str>) -> AppResult<Duration> {
    let millis = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.parse::<u64>().map_err(|_| {
            tool_failure("rlm_process_wait poll_interval_ms must be a positive integer")
        })?,
        None => 100,
    };
    Ok(Duration::from_millis(millis.clamp(25, 1_000)))
}

fn parse_rlm_live_drain_max_turns(raw: Option<&str>) -> AppResult<usize> {
    let value = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_process_drain max_turns must be a positive integer"))?,
        None => 10,
    };
    Ok(value.clamp(1, 100))
}

fn parse_rlm_live_recovery_mode(raw: Option<&str>) -> AppResult<RlmLiveRecoveryMode> {
    match raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("requeue")
    {
        "requeue" | "queue" | "pending" => Ok(RlmLiveRecoveryMode::Requeue),
        "fail" | "failed" => Ok(RlmLiveRecoveryMode::Fail),
        other => Err(tool_failure(format!(
            "rlm_process_recover mode must be `requeue` or `fail`, got `{other}`"
        ))),
    }
}

fn parse_rlm_live_recover_limit(raw: Option<&str>) -> AppResult<usize> {
    let limit = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| tool_failure("rlm_process_recover limit must be a positive integer"))?,
        None => 20,
    };
    Ok(limit.clamp(1, 100))
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn read_rlm_live_event_batch(
    config: &AppConfig,
    session_id: &str,
    cursor: u64,
    limit: usize,
) -> AppResult<RlmLiveEventBatch> {
    let path = rlm_live_session_event_log_path(config, session_id);
    if !path.exists() {
        return Ok(RlmLiveEventBatch {
            path,
            exists: false,
            next_cursor: cursor,
            events_json: String::new(),
            count: 0,
        });
    }
    let content = fs::read_to_string(&path)
        .map_err(|error| tool_failure(format!("rlm_process_events read failed: {error}")))?;
    let mut events_json = String::new();
    let mut count = 0usize;
    let mut next_cursor = cursor;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let value = parse_json_value(line)
            .map_err(|error| tool_failure(format!("rlm_process_events invalid JSON: {error}")))?;
        let seq = match &value {
            JsonValue::Object(root) => root.get("seq").and_then(json_as_u64).unwrap_or(0),
            _ => 0,
        };
        if seq <= cursor {
            continue;
        }
        if count >= limit {
            break;
        }
        if count > 0 {
            events_json.push(',');
        }
        events_json.push_str(&json_value_to_string(&value));
        next_cursor = seq;
        count += 1;
    }
    Ok(RlmLiveEventBatch {
        path,
        exists: true,
        next_cursor,
        events_json,
        count,
    })
}

pub fn rlm_live_event_values(
    config: &AppConfig,
    session_id: &str,
    cursor: u64,
    limit: usize,
) -> AppResult<(bool, u64, Vec<JsonValue>)> {
    validate_rlm_model_session_id(session_id)?;
    let batch = read_rlm_live_event_batch(config, session_id, cursor, limit)?;
    if batch.events_json.is_empty() {
        return Ok((batch.exists, batch.next_cursor, Vec::new()));
    }
    let value = parse_json_value(&format!("[{}]", batch.events_json))
        .map_err(|error| tool_failure(format!("rlm_process_events invalid JSON: {error}")))?;
    let JsonValue::Array(events) = value else {
        return Err(tool_failure("rlm_process_events expected JSON array"));
    };
    Ok((batch.exists, batch.next_cursor, events))
}

fn parse_bool_arg(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn rlm_python_payload(code: &str, context: &str, question: &str, state: Option<&str>) -> String {
    rlm_python_payload_with_options(code, context, question, state, None, false)
}

fn rlm_python_session_payload(
    code: &str,
    context: &str,
    question: &str,
    state: &str,
    timeout: Duration,
    reset: bool,
) -> String {
    rlm_python_payload_with_options(
        code,
        context,
        question,
        Some(state),
        Some(timeout.as_millis()),
        reset,
    )
}

fn rlm_python_payload_with_options(
    code: &str,
    context: &str,
    question: &str,
    state: Option<&str>,
    timeout_ms: Option<u128>,
    reset: bool,
) -> String {
    let mut payload = format!(
        "{{\"code\":\"{}\",\"context\":\"{}\",\"question\":\"{}\"",
        json_escape(code),
        json_escape(context),
        json_escape(question)
    );
    if let Some(state) = state {
        payload.push_str(",\"state\":");
        payload.push_str(state);
    }
    if let Some(timeout_ms) = timeout_ms {
        payload.push_str(",\"timeout_ms\":");
        payload.push_str(&timeout_ms.to_string());
    }
    if reset {
        payload.push_str(",\"reset\":true");
    }
    payload.push('}');
    payload
}

fn rlm_python_sessions_dir(config: &AppConfig) -> PathBuf {
    PathBuf::from(&config.workspace.config_dir).join("rlm-python")
}

fn rlm_python_session_process_key(config: &AppConfig, session_id: &str) -> String {
    format!(
        "{}::{}",
        rlm_python_sessions_dir(config).display(),
        session_id
    )
}

fn rlm_python_session_process_json(config: &AppConfig, session_id: &str) -> AppResult<String> {
    let Some(repls) = RLM_PYTHON_REPLS.get() else {
        return Ok("{\"active\":false}".to_string());
    };
    let key = rlm_python_session_process_key(config, session_id);
    let mut repls = repls
        .lock()
        .map_err(|_| tool_failure("rlm_python_session repl cache lock poisoned"))?;
    let status = match repls.get_mut(&key) {
        Some(process) => match process.child.try_wait() {
            Ok(None) => Some(process.child.id()),
            Ok(Some(_)) | Err(_) => {
                repls.remove(&key);
                None
            }
        },
        None => None,
    };
    Ok(match status {
        Some(pid) => format!("{{\"active\":true,\"pid\":{pid}}}"),
        None => "{\"active\":false}".to_string(),
    })
}

fn rlm_python_session_path(config: &AppConfig, session_id: &str) -> PathBuf {
    rlm_python_sessions_dir(config).join(format!("{session_id}.json"))
}

fn read_rlm_python_session_state(path: &Path) -> AppResult<String> {
    if !path.exists() {
        return Ok("{}".to_string());
    }
    let state = fs::read_to_string(path)
        .map_err(|error| tool_failure(format!("rlm_python_session read failed: {error}")))?;
    match parse_json_value(&state) {
        Ok(JsonValue::Object(_)) => Ok(state),
        Ok(_) => Err(tool_failure(
            "rlm_python_session stored state must be a JSON object",
        )),
        Err(error) => Err(tool_failure(format!(
            "rlm_python_session stored state is invalid JSON: {error}"
        ))),
    }
}

fn extract_rlm_python_state(summary: &str) -> AppResult<String> {
    let value = parse_json_value(summary)
        .map_err(|error| tool_failure(format!("rlm_python_session invalid output: {error}")))?;
    let object = match value {
        JsonValue::Object(object) => object,
        _ => {
            return Err(tool_failure(
                "rlm_python_session output must be a JSON object",
            ))
        }
    };
    let Some(state) = object.get("state") else {
        return Ok("{}".to_string());
    };
    match state {
        JsonValue::Object(_) => Ok(json_value_to_string(state)),
        JsonValue::Null => Ok("{}".to_string()),
        _ => Err(tool_failure(
            "rlm_python_session state must remain a JSON object",
        )),
    }
}

fn run_rlm_python_sandbox(payload: &str, timeout: Duration) -> AppResult<String> {
    let interpreter = resolve_rlm_python_interpreter().ok_or_else(|| {
        tool_failure("rlm_python requires Python; tried python3, python, and py -3")
    })?;
    let mut child = Command::new(interpreter.program)
        .args(interpreter.args)
        .arg("-I")
        .arg("-S")
        .arg("-c")
        .arg(RLM_PYTHON_SANDBOX)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            tool_failure(format!(
                "rlm_python failed to spawn {}: {error}",
                render_rlm_python_interpreter(interpreter)
            ))
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload.as_bytes())
            .map_err(|error| tool_failure(format!("rlm_python failed to write stdin: {error}")))?;
    }

    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .map_err(|error| tool_failure(format!("rlm_python wait failed: {error}")))?
            .is_some()
        {
            let output = child
                .wait_with_output()
                .map_err(|error| tool_failure(format!("rlm_python output failed: {error}")))?;
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if output.status.success() {
                return Ok(stdout);
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stdout.is_empty() { stderr } else { stdout };
            return Err(tool_failure(format!("rlm_python failed: {detail}")));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(tool_failure(format!(
                "rlm_python timed out after {} ms",
                timeout.as_millis()
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_rlm_python_repl_sandbox(
    session_key: &str,
    payload: &str,
    reset_process: bool,
) -> AppResult<String> {
    let repls = RLM_PYTHON_REPLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut repls = repls
        .lock()
        .map_err(|_| tool_failure("rlm_python_session repl cache lock poisoned"))?;
    if reset_process {
        repls.remove(session_key);
    }
    let is_stale = match repls.get_mut(session_key) {
        Some(process) => process
            .child
            .try_wait()
            .map(|status| status.is_some())
            .unwrap_or(true),
        None => false,
    };
    if is_stale {
        repls.remove(session_key);
    }
    if !repls.contains_key(session_key) {
        repls.insert(session_key.to_string(), spawn_rlm_python_repl()?);
    }

    let result = {
        let process = repls
            .get_mut(session_key)
            .ok_or_else(|| tool_failure("rlm_python_session repl process missing"))?;
        if let Err(error) = writeln!(process.stdin, "{payload}").and_then(|_| process.stdin.flush())
        {
            Err(tool_failure(format!(
                "rlm_python_session repl write failed: {error}"
            )))
        } else {
            let mut line = String::new();
            match process.stdout.read_line(&mut line) {
                Ok(0) => Err(tool_failure("rlm_python_session repl exited unexpectedly")),
                Ok(_) => Ok(line.trim().to_string()),
                Err(error) => Err(tool_failure(format!(
                    "rlm_python_session repl read failed: {error}"
                ))),
            }
        }
    };

    if result.is_err() {
        repls.remove(session_key);
    }
    let summary = result?;
    ensure_rlm_python_success(&summary, "rlm_python_session")?;
    Ok(summary)
}

fn spawn_rlm_python_repl() -> AppResult<RlmPythonReplProcess> {
    let interpreter = resolve_rlm_python_interpreter().ok_or_else(|| {
        tool_failure("rlm_python requires Python; tried python3, python, and py -3")
    })?;
    let mut child = Command::new(interpreter.program)
        .args(interpreter.args)
        .arg("-I")
        .arg("-S")
        .arg("-u")
        .arg("-c")
        .arg(RLM_PYTHON_REPL_SANDBOX)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            tool_failure(format!(
                "rlm_python_session failed to spawn {} repl: {error}",
                render_rlm_python_interpreter(interpreter)
            ))
        })?;
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(tool_failure("rlm_python_session repl stdin unavailable"));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => BufReader::new(stdout),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(tool_failure("rlm_python_session repl stdout unavailable"));
        }
    };
    Ok(RlmPythonReplProcess {
        child,
        stdin,
        stdout,
    })
}

fn ensure_rlm_python_success(summary: &str, tool_name: &str) -> AppResult<()> {
    let value = parse_json_value(summary)
        .map_err(|error| tool_failure(format!("{tool_name} invalid output: {error}")))?;
    let object = match value {
        JsonValue::Object(object) => object,
        _ => {
            return Err(tool_failure(format!(
                "{tool_name} output must be a JSON object"
            )))
        }
    };
    if matches!(object.get("ok"), Some(JsonValue::Bool(true))) {
        return Ok(());
    }
    let detail = object
        .get("error")
        .and_then(json_as_string)
        .unwrap_or("unknown error");
    Err(tool_failure(format!("{tool_name} failed: {detail}")))
}

fn resolve_rlm_python_interpreter() -> Option<RlmPythonInterpreter> {
    RLM_PYTHON_INTERPRETERS.iter().copied().find(|interpreter| {
        Command::new(interpreter.program)
            .args(interpreter.args)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    })
}

fn render_rlm_python_interpreter(interpreter: RlmPythonInterpreter) -> String {
    let mut command = interpreter.program.to_string();
    for arg in interpreter.args {
        command.push(' ');
        command.push_str(arg);
    }
    command
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RlmBatchQuestion {
    question: String,
    context: Option<String>,
    strategy: Option<String>,
}

fn parse_rlm_batch_questions(raw: &str) -> AppResult<Vec<RlmBatchQuestion>> {
    let value = parse_json_value(raw)
        .map_err(|error| tool_failure(format!("rlm_batch questions must be JSON: {error}")))?;
    let array = match value {
        JsonValue::Array(array) => array,
        _ => return Err(tool_failure("rlm_batch `questions` must be a JSON array")),
    };
    if array.is_empty() {
        return Err(tool_failure("rlm_batch requires at least one question"));
    }
    if array.len() > MAX_RLM_BATCH_QUESTIONS {
        return Err(tool_failure(format!(
            "rlm_batch accepts at most {MAX_RLM_BATCH_QUESTIONS} questions"
        )));
    }

    let mut questions = Vec::with_capacity(array.len());
    for (index, item) in array.iter().enumerate() {
        match item {
            JsonValue::String(question) => {
                let question = question.trim();
                if question.is_empty() {
                    return Err(tool_failure(format!(
                        "rlm_batch question {} must be non-empty",
                        index + 1
                    )));
                }
                questions.push(RlmBatchQuestion {
                    question: question.to_string(),
                    context: None,
                    strategy: None,
                });
            }
            JsonValue::Object(object) => {
                let question = json_as_string(object.get("question").ok_or_else(|| {
                    tool_failure(format!(
                        "rlm_batch question {} requires `question`",
                        index + 1
                    ))
                })?)
                .map(str::trim)
                .filter(|question| !question.is_empty())
                .ok_or_else(|| {
                    tool_failure(format!(
                        "rlm_batch question {} requires non-empty `question`",
                        index + 1
                    ))
                })?;
                let context = object
                    .get("context")
                    .and_then(json_as_string)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let strategy = object
                    .get("strategy")
                    .and_then(json_as_string)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                questions.push(RlmBatchQuestion {
                    question: question.to_string(),
                    context,
                    strategy,
                });
            }
            _ => {
                return Err(tool_failure(format!(
                    "rlm_batch question {} must be a string or object",
                    index + 1
                )))
            }
        }
    }
    Ok(questions)
}

fn render_rlm_batch_tasks(
    context: &str,
    questions: &[RlmBatchQuestion],
    strategy: &str,
    steps: &str,
) -> String {
    let mut out = String::from("[");
    for (index, question) in questions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let task = render_rlm_task(
            question.context.as_deref().unwrap_or(context),
            &question.question,
            question.strategy.as_deref().unwrap_or(strategy),
        );
        out.push_str(&format!(
            "{{\"task\":\"{}\",\"steps\":\"{}\"}}",
            json_escape(&task),
            json_escape(steps)
        ));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_value(summary: &str, key: &str) -> Option<String> {
        let prefix = format!("{key}=");
        summary.lines().find_map(|line| {
            line.strip_prefix(&prefix)
                .map(|value| value.trim().to_string())
        })
    }

    #[test]
    fn render_rlm_task_includes_context_question_and_strategy() {
        let task = render_rlm_task("alpha\nbeta", "What changed?", "classify");

        assert!(task.contains("RLM analysis task"));
        assert!(task.contains("Strategy: classify"));
        assert!(task.contains("What changed?"));
        assert!(task.contains("alpha\nbeta"));
        assert!(task.contains("concise synthesized answer"));
    }

    #[test]
    fn rlm_process_task_loads_inline_content_and_renders_metadata() {
        let input = ToolInput::new()
            .with_arg("content", "alpha\nbeta")
            .with_arg("task", "summarize");
        let loaded = load_rlm_process_input(&input).unwrap();
        let task = render_rlm_process_task("summarize", &loaded);

        assert_eq!(loaded.label, "inline content");
        assert_eq!(loaded.char_count, 10);
        assert_eq!(loaded.line_count, 2);
        assert!(task.contains("RLM process task"));
        assert!(task.contains("Objective: summarize"));
        assert!(task.contains("Input size: 10 chars, 2 lines"));
        assert!(task.contains("alpha\nbeta"));
    }

    #[test]
    fn rlm_process_alias_reports_its_tool_name_for_missing_task() {
        let tool = RlmTool {
            tool_name: "rlm_process",
            config: AppConfig::default(),
            parent_depth: 0,
        };

        assert!(tool
            .execute(ToolInput::new().with_arg("content", "alpha"))
            .unwrap_err()
            .to_string()
            .contains("rlm_process requires non-empty `task`"));
    }

    #[test]
    fn rlm_process_input_reads_safe_workspace_relative_file() {
        let _cwd_lock = crate::util::cwd::lock_cwd().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-process-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("input.txt");
        fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let relative = file.strip_prefix(&cwd).unwrap().display().to_string();

        let loaded = load_rlm_process_input(&ToolInput::new().with_arg("file_path", relative))
            .expect("load relative file");

        assert!(loaded.label.contains("file_path: target/"));
        assert_eq!(loaded.char_count, 14);
        assert_eq!(loaded.line_count, 3);
        assert!(loaded.content.contains("two"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_input_rejects_ambiguous_or_unsafe_sources() {
        let missing = load_rlm_process_input(&ToolInput::new())
            .unwrap_err()
            .to_string();
        assert!(missing.contains("file_path"));
        assert!(missing.contains("content"));
        assert!(load_rlm_process_input(
            &ToolInput::new()
                .with_arg("file_path", "README.md")
                .with_arg("content", "inline")
        )
        .unwrap_err()
        .to_string()
        .contains("not both"));
        assert!(validate_rlm_process_file_path("/tmp/outside")
            .unwrap_err()
            .to_string()
            .contains("workspace-relative"));
        assert!(validate_rlm_process_file_path("../outside")
            .unwrap_err()
            .to_string()
            .contains("must not contain"));
        assert!(
            validate_rlm_process_content_len(&"x".repeat(MAX_RLM_PROCESS_CONTENT_CHARS + 1))
                .unwrap_err()
                .to_string()
                .contains("maximum")
        );
    }

    #[test]
    fn rlm_model_session_round_trips_and_renders_prior_context() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-model-session-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let mut session = empty_rlm_model_session("analysis.1");
        let input = RlmProcessInput {
            label: "inline content".to_string(),
            content: "alpha\nbeta".to_string(),
            char_count: 10,
            line_count: 2,
        };

        append_rlm_model_session_turn(&mut session, "summarize alpha", &input, "alpha result");
        write_rlm_model_session(&config, &session).unwrap();

        let loaded = read_rlm_model_session(&config, "analysis.1", false).unwrap();
        assert_eq!(loaded.turns.len(), 1);
        let next_input = RlmProcessInput {
            label: "inline content".to_string(),
            content: "gamma".to_string(),
            char_count: 5,
            line_count: 1,
        };
        let task = render_rlm_process_task_with_session("continue", &next_input, Some(&loaded));
        assert!(task.contains("Prior RLM session context"));
        assert!(task.contains("Session: analysis.1, prior_turns=1"));
        assert!(task.contains("summarize alpha"));
        assert!(task.contains("alpha result"));
        assert!(task.contains("Long input:\ngamma"));

        let reset = read_rlm_model_session(&config, "analysis.1", true).unwrap();
        assert!(reset.turns.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_session_only_continuation_uses_prior_context() {
        let input = RlmProcessInput {
            label: "inline content".to_string(),
            content: "alpha\nbeta".to_string(),
            char_count: 10,
            line_count: 2,
        };
        let mut session = empty_rlm_model_session("analysis.1");
        append_rlm_model_session_turn(&mut session, "summarize alpha", &input, "alpha result");

        let continuation = load_rlm_process_input_or_session_context(
            &ToolInput::new().with_arg("task", "continue analysis"),
            Some(&session),
        )
        .unwrap();
        assert_eq!(continuation.label, "session context only");
        assert_eq!(continuation.char_count, 0);
        assert!(continuation.content.contains("No new long input"));

        let task = render_rlm_process_task_with_session(
            "continue analysis",
            &continuation,
            Some(&session),
        );
        assert!(task.contains("Prior RLM session context"));
        assert!(task.contains("summarize alpha"));
        assert!(task.contains("alpha result"));
        assert!(task.contains("Input source: session context only"));

        let empty_session = empty_rlm_model_session("analysis.2");
        assert!(load_rlm_process_input_or_session_context(
            &ToolInput::new().with_arg("task", "continue analysis"),
            Some(&empty_session)
        )
        .unwrap_err()
        .to_string()
        .contains("session-only continuation requires an existing session"));
        assert!(load_rlm_process_input_or_session_context(
            &ToolInput::new().with_arg("task", "continue analysis"),
            None
        )
        .unwrap_err()
        .to_string()
        .contains("file_path"));
    }

    #[test]
    fn rlm_model_session_rejects_bad_ids_and_clips_turns() {
        for bad_id in ["", ".hidden", "a..b", "slash/id"] {
            assert!(validate_rlm_model_session_id(bad_id)
                .unwrap_err()
                .to_string()
                .contains("session_id"));
        }
        assert!(validate_rlm_model_session_id(&"a".repeat(65)).is_err());

        let input = RlmProcessInput {
            label: "inline content".to_string(),
            content: "alpha".to_string(),
            char_count: 5,
            line_count: 1,
        };
        let mut clipped = empty_rlm_model_session("valid-id");
        append_rlm_model_session_turn(
            &mut clipped,
            "clip",
            &input,
            &"x".repeat(MAX_RLM_MODEL_SESSION_SUMMARY_CHARS + 10),
        );
        assert!(clipped.turns[0].summary.contains("[truncated]"));

        let mut capped = empty_rlm_model_session("valid-id");
        for index in 0..(MAX_RLM_MODEL_SESSION_TURNS + 5) {
            append_rlm_model_session_turn(&mut capped, &format!("task {index}"), &input, "summary");
        }
        assert_eq!(capped.turns.len(), MAX_RLM_MODEL_SESSION_TURNS);
        assert_eq!(capped.turns[0].task, "task 5");
    }

    #[test]
    fn rlm_process_sessions_lists_and_reads_model_session_files() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-process-sessions-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let mut session = empty_rlm_model_session("analysis.1");
        let input = RlmProcessInput {
            label: "inline content".to_string(),
            content: "alpha".to_string(),
            char_count: 5,
            line_count: 1,
        };
        append_rlm_model_session_turn(&mut session, "summarize alpha", &input, "alpha result");
        write_rlm_model_session(&config, &session).unwrap();

        let tool = RlmModelSessionsTool {
            config: config.clone(),
        };
        let listed = tool.execute(ToolInput::new()).unwrap();
        assert!(listed.summary.contains(r#""session_id":"analysis.1""#));
        assert!(listed.summary.contains(r#""turns":1"#));
        assert!(listed.summary.contains(r#""last_task":"summarize alpha""#));
        assert!(!listed.summary.contains("alpha result"));

        let shown = tool
            .execute(ToolInput::new().with_arg("session_id", "analysis.1"))
            .unwrap();
        assert!(shown.summary.contains(r#""exists":true"#));
        assert!(shown.summary.contains("alpha result"));

        assert_eq!(parse_rlm_model_sessions_limit(None).unwrap(), 20);
        assert_eq!(parse_rlm_model_sessions_limit(Some("0")).unwrap(), 1);
        assert_eq!(parse_rlm_model_sessions_limit(Some("1000")).unwrap(), 100);
        assert!(parse_rlm_model_sessions_limit(Some("bad"))
            .unwrap_err()
            .to_string()
            .contains("limit"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_sessions_can_include_live_daemon_manifests() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-sessions-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let manifest_path = rlm_live_session_manifest_path(&config, "live.1");
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        fs::write(
            &manifest_path,
            r#"{"session_id":"live.1","status":"idle","daemon_pid":12345,"daemon_epoch":"epoch+1","runtime_thread_id":"thread-live","runtime_session_id":"session-live","active_turn_id":null,"queued_turns":2,"model":"deepseek-coder","workspace":"/tmp/ws","created_at":"epoch+1","updated_at":"epoch+2","last_error":null}"#,
        )
        .unwrap();

        let tool = RlmModelSessionsTool {
            config: config.clone(),
        };
        let default_list = tool.execute(ToolInput::new()).unwrap();
        assert!(!default_list.summary.contains("live.1"));

        let live_list = tool
            .execute(ToolInput::new().with_arg("include_live", "true"))
            .unwrap();
        assert!(live_list.summary.contains(r#""include_live":true"#));
        assert!(live_list.summary.contains(r#""live_sessions":["#));
        assert!(live_list.summary.contains(r#""session_id":"live.1""#));
        assert!(live_list.summary.contains(r#""status":"idle""#));
        assert!(live_list
            .summary
            .contains(r#""runtime_thread_id":"thread-live""#));
        assert!(live_list.summary.contains(r#""queued_turns":2"#));

        let shown = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "live.1")
                    .with_arg("include_live", "true"),
            )
            .unwrap();
        assert!(shown.summary.contains(r#""exists":false"#));
        assert!(shown.summary.contains(r#""live_exists":true"#));
        assert!(shown.summary.contains(r#""live_session":{"#));
        assert!(shown
            .summary
            .contains(r#""kind":"deepseek.rlm.live_session.v1""#));
        assert!(shown.summary.contains(r#""model":"deepseek-coder""#));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_live_sessions_report_daemon_owner_liveness() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-daemon-owner-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        write_rlm_live_session_manifest_with_daemon(
            &config,
            "owner.current",
            "running",
            "thread-current",
            Some("session-current"),
            Some("turn-current"),
            0,
            "deepseek-coder",
            "/tmp/ws",
            None,
            Some(std::process::id() as u64),
            Some("epoch+owner"),
        )
        .unwrap();
        write_rlm_live_session_manifest_with_daemon(
            &config,
            "owner.stale",
            "running",
            "thread-stale",
            Some("session-stale"),
            Some("turn-stale"),
            0,
            "deepseek-coder",
            "/tmp/ws",
            None,
            Some(i32::MAX as u64 + 1),
            Some("epoch+stale"),
        )
        .unwrap();

        let current_manifest = read_rlm_live_session_manifest(
            &rlm_live_session_manifest_path(&config, "owner.current"),
            "owner.current",
        )
        .unwrap();
        assert_eq!(
            rlm_live_manifest_u64_field(&current_manifest, "daemon_pid"),
            Some(std::process::id() as u64)
        );
        let current_json = json_value_to_string(&current_manifest);
        assert!(current_json.contains(r#""daemon_alive":true"#));
        assert!(current_json.contains(r#""daemon_stale":false"#));
        assert!(current_json.contains(r#""daemon_owner":"current""#));

        let listed = RlmModelSessionsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("include_live", "true"))
        .unwrap();
        assert!(listed.summary.contains(r#""session_id":"owner.current""#));
        assert!(listed.summary.contains(r#""daemon_alive":true"#));
        assert!(listed.summary.contains(r#""daemon_owner":"current""#));
        assert!(listed.summary.contains(r#""session_id":"owner.stale""#));
        assert!(listed.summary.contains(r#""daemon_alive":false"#));
        assert!(listed.summary.contains(r#""daemon_stale":true"#));
        assert!(listed.summary.contains(r#""daemon_owner":"stale""#));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_status_summarizes_live_queue_and_stale_owner() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-status-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let first = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "first status turn")
                    .with_arg("content", "alpha status payload")
                    .with_arg("session_id", "status.1")
                    .with_arg("live", "true")
                    .with_arg("cwd", root.display().to_string()),
            )
            .unwrap();
        let first_turn = meta_value(&first.summary, "meta.rlm_turn_id").unwrap();
        rlm.execute(
            ToolInput::new()
                .with_arg("task", "second status turn")
                .with_arg("content", "beta status payload")
                .with_arg("session_id", "status.1")
                .with_arg("live", "true"),
        )
        .unwrap();

        let status_tool = RlmLiveStatusTool {
            config: config.clone(),
        };
        let queued = status_tool
            .execute(ToolInput::new().with_arg("session_id", "status.1"))
            .unwrap();
        assert!(queued
            .summary
            .contains(r#""kind":"deepseek.rlm.live_status.v1""#));
        assert!(queued.summary.contains(r#""queued_turns_runtime":2"#));
        assert!(queued.summary.contains(r#""pending":2"#));
        assert!(queued
            .summary
            .contains(&format!(r#""next_pending_turn_id":"{}""#, first_turn)));
        assert!(queued
            .summary
            .contains("rlm_process_run_next session_id=status.1"));

        let manifest_path = rlm_live_session_manifest_path(&config, "status.1");
        let manifest = read_rlm_live_session_manifest(&manifest_path, "status.1").unwrap();
        let runtime_thread_id =
            rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        let runtime_session_id = rlm_live_manifest_string_field(&manifest, "runtime_session_id");
        let store = rlm_runtime_store(&config);
        store
            .claim_task(&first_turn, "status-test".to_string())
            .unwrap();
        update_rlm_live_session_turn_payload_status(
            &config,
            "status.1",
            &first_turn,
            "running",
            Vec::new(),
        )
        .unwrap();
        let payload = read_rlm_live_session_turn_payload(&config, "status.1", &first_turn).unwrap();
        write_rlm_live_session_manifest_with_daemon(
            &config,
            "status.1",
            "running",
            &runtime_thread_id,
            runtime_session_id.as_deref(),
            Some(&first_turn),
            1,
            &payload.model,
            &payload.workspace,
            rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
            Some(i32::MAX as u64 + 1),
            Some("epoch+stale"),
        )
        .unwrap();

        let stale = status_tool
            .execute(ToolInput::new().with_arg("session_id", "status.1"))
            .unwrap();
        assert!(stale.summary.contains(r#""daemon_stale":true"#));
        assert!(stale.summary.contains(r#""daemon_owner":"stale""#));
        assert!(stale.summary.contains(r#""running":1"#));
        assert!(stale
            .summary
            .contains("rlm_process_recover session_id=status.1"));

        let listed = status_tool.execute(ToolInput::new()).unwrap();
        assert!(listed.summary.contains(r#""live_sessions":1"#));
        assert!(listed.summary.contains(r#""stale_daemon_sessions":1"#));
        assert!(listed.summary.contains(r#""queued_turns":1"#));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_live_enqueues_runtime_turn_and_manifest() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-queue-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let tool = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };

        let first = tool
            .execute(
                ToolInput::new()
                    .with_arg("task", "summarize alpha")
                    .with_arg("content", "alpha")
                    .with_arg("session_id", "live.queue")
                    .with_arg("live", "true")
                    .with_arg("cwd", root.display().to_string()),
            )
            .unwrap();
        assert!(first.summary.contains("meta.rlm_live=true"));
        assert!(first.summary.contains("meta.rlm_status=queued"));
        assert!(first.summary.contains("meta.rlm_queued_turns=1"));
        let first_turn = meta_value(&first.summary, "meta.rlm_turn_id").unwrap();
        let first_payload = fs::read_to_string(rlm_live_session_turn_payload_path(
            &config,
            "live.queue",
            &first_turn,
        ))
        .unwrap();
        assert!(first_payload.contains(r#""task":"summarize alpha""#));
        assert!(first_payload.contains(r#""content":"alpha""#));
        assert!(first_payload.contains(r#""steps":"6""#));

        let manifest_path = rlm_live_session_manifest_path(&config, "live.queue");
        let manifest = read_rlm_live_session_manifest(&manifest_path, "live.queue").unwrap();
        let thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        assert_eq!(
            rlm_live_manifest_u64_field(&manifest, "queued_turns"),
            Some(1)
        );
        assert_eq!(
            rlm_live_manifest_string_field(&manifest, "workspace").as_deref(),
            Some(root.display().to_string().as_str())
        );

        let second = tool
            .execute(
                ToolInput::new()
                    .with_arg("task", "continue from live context")
                    .with_arg("session_id", "live.queue")
                    .with_arg("live", "true"),
            )
            .unwrap();
        assert!(second.summary.contains("meta.rlm_queued_turns=2"));
        assert!(second.summary.contains("live session context only"));
        let second_turn = meta_value(&second.summary, "meta.rlm_turn_id").unwrap();
        let second_payload = fs::read_to_string(rlm_live_session_turn_payload_path(
            &config,
            "live.queue",
            &second_turn,
        ))
        .unwrap();
        assert!(second_payload.contains(r#""label":"live session context only""#));
        assert!(second_payload.contains(r#""content":"""#));

        let updated = read_rlm_live_session_manifest(&manifest_path, "live.queue").unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "runtime_thread_id").as_deref(),
            Some(thread_id.as_str())
        );
        assert_eq!(
            rlm_live_manifest_u64_field(&updated, "queued_turns"),
            Some(2)
        );
        let store = rlm_runtime_store(&config);
        let tasks = store.list_tasks(None, Some(&thread_id), 10).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().all(|task| task.kind == "rlm_process"));
        assert!(tasks.iter().all(|task| task.status == "pending"));
        let events =
            fs::read_to_string(rlm_live_session_event_log_path(&config, "live.queue")).unwrap();
        assert!(events.contains(r#""kind":"turn_queued""#));
        assert!(events.contains(r#""seq":1"#));
        assert!(events.contains(r#""seq":2"#));

        let sessions = RlmModelSessionsTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "live.queue")
                .with_arg("include_turns", "true"),
        )
        .unwrap();
        assert!(sessions.summary.contains(r#""include_live":true"#));
        assert!(sessions.summary.contains(r#""include_turns":true"#));
        assert!(sessions.summary.contains(r#""live_turns":["#));
        assert!(sessions.summary.contains(&first_turn));
        assert!(sessions.summary.contains(&second_turn));
        assert!(sessions.summary.contains(r#""runtime_status":"pending""#));
        assert!(sessions
            .summary
            .contains(r#""input_label":"inline content""#));
        assert!(sessions
            .summary
            .contains(r#""input_label":"live session context only""#));

        let listed = RlmModelSessionsTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("include_live", "true")
                .with_arg("include_turns", "true"),
        )
        .unwrap();
        assert!(listed.summary.contains(r#""include_turns":true"#));
        assert!(listed.summary.contains(r#""turns":["#));
        assert!(listed.summary.contains(&first_turn));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_events_reads_live_event_cursor() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-events-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        rlm.execute(
            ToolInput::new()
                .with_arg("task", "first live turn")
                .with_arg("content", "alpha")
                .with_arg("session_id", "events.1")
                .with_arg("live", "true"),
        )
        .unwrap();
        rlm.execute(
            ToolInput::new()
                .with_arg("task", "second live turn")
                .with_arg("session_id", "events.1")
                .with_arg("live", "true"),
        )
        .unwrap();

        let events_tool = RlmLiveEventsTool {
            config: config.clone(),
        };
        let first = events_tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "events.1")
                    .with_arg("limit", "1"),
            )
            .unwrap();
        assert!(first.summary.contains(r#""exists":true"#));
        assert!(first.summary.contains(r#""cursor":0"#));
        assert!(first.summary.contains(r#""next_cursor":1"#));
        assert!(first.summary.contains(r#""seq":1"#));
        assert!(!first.summary.contains(r#""seq":2"#));

        let second = events_tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "events.1")
                    .with_arg("cursor", "1"),
            )
            .unwrap();
        assert!(second.summary.contains(r#""cursor":1"#));
        assert!(second.summary.contains(r#""next_cursor":2"#));
        assert!(second.summary.contains(r#""seq":2"#));
        assert!(second.summary.contains("second live turn"));

        let wait = RlmLiveWaitTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "events.1")
                .with_arg("cursor", "1")
                .with_arg("timeout_ms", "0"),
        )
        .unwrap();
        assert!(wait.summary.contains(r#""timed_out":false"#));
        assert!(wait.summary.contains(r#""seq":2"#));

        let quiet = RlmLiveWaitTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "events.1")
                .with_arg("cursor", "2")
                .with_arg("timeout_ms", "0"),
        )
        .unwrap();
        assert!(quiet.summary.contains(r#""timed_out":false"#));
        assert!(quiet.summary.contains(r#""events":[]"#));

        let missing = events_tool
            .execute(ToolInput::new().with_arg("session_id", "missing"))
            .unwrap();
        assert!(missing.summary.contains(r#""exists":false"#));
        assert_eq!(parse_rlm_live_events_limit(None).unwrap(), 50);
        assert_eq!(parse_rlm_live_events_limit(Some("0")).unwrap(), 1);
        assert_eq!(parse_rlm_live_events_limit(Some("1000")).unwrap(), 500);
        assert_eq!(
            parse_rlm_live_wait_timeout(Some("40000"))
                .unwrap()
                .as_millis(),
            30000
        );
        assert_eq!(
            parse_rlm_live_wait_poll_interval(Some("1"))
                .unwrap()
                .as_millis(),
            25
        );
        assert!(parse_rlm_live_event_cursor(Some("bad"))
            .unwrap_err()
            .to_string()
            .contains("cursor"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_live_worker_events_append_stream_and_tool_events() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-worker-events-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let target = RlmLiveWorkerEventTarget {
            config: config.clone(),
            session_id: "worker.events".to_string(),
            runtime_thread_id: "thread-worker-events".to_string(),
            task_id: "task-worker-events".to_string(),
        };
        let mut stream = RlmLiveWorkerStreamEvents {
            target: target.clone(),
        };
        stream.on_reasoning_delta("thinking");
        stream.on_text_delta("hello");
        stream.on_assistant_done("hello world");
        let input = BTreeMap::from([("path".to_string(), "src/main.rs".to_string())]);
        stream.on_tool_call("read_file", &input);

        let mut run = RlmLiveWorkerRunEvents { target };
        run.on_tool_call("read_file", &input);
        run.on_permission_request("write_file", &input, "write", "src/main.rs");
        run.on_tool_result(&ToolEvent {
            tool_name: "read_file".to_string(),
            input,
            output: "file excerpt".to_string(),
            status: ObservationStatus::Ok,
        });

        let events = RlmLiveEventsTool { config }
            .execute(ToolInput::new().with_arg("session_id", "worker.events"))
            .unwrap();
        assert!(events
            .summary
            .contains(r#""kind":"worker_reasoning_delta""#));
        assert!(events.summary.contains(r#""kind":"worker_text_delta""#));
        assert!(events.summary.contains(r#""kind":"worker_assistant_done""#));
        assert!(events
            .summary
            .contains(r#""kind":"worker_model_tool_call""#));
        assert!(events.summary.contains(r#""kind":"worker_tool_call""#));
        assert!(events
            .summary
            .contains(r#""kind":"worker_permission_request""#));
        assert!(events.summary.contains(r#""kind":"worker_tool_result""#));
        assert!(events.summary.contains(r#""status":"ok""#));
        assert!(events.summary.contains(r#""next_cursor":7"#));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_cancel_cancels_queued_live_turns() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-cancel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let first = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "first queued turn")
                    .with_arg("content", "alpha")
                    .with_arg("session_id", "cancel.1")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let first_turn = meta_value(&first.summary, "meta.rlm_turn_id").unwrap();
        let second = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "second queued turn")
                    .with_arg("session_id", "cancel.1")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let second_turn = meta_value(&second.summary, "meta.rlm_turn_id").unwrap();

        let cancel = RlmLiveCancelTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "cancel.1")
                .with_arg("turn_id", first_turn.clone())
                .with_arg("reason", "superseded"),
        )
        .unwrap();
        assert!(cancel.summary.contains(r#""cancelled_count":1"#));
        assert!(cancel.summary.contains(r#""queued_turns":1"#));
        assert!(cancel.summary.contains(&first_turn));

        let manifest_path = rlm_live_session_manifest_path(&config, "cancel.1");
        let manifest = read_rlm_live_session_manifest(&manifest_path, "cancel.1").unwrap();
        let thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        assert_eq!(
            rlm_live_manifest_u64_field(&manifest, "queued_turns"),
            Some(1)
        );
        let store = rlm_runtime_store(&config);
        assert_eq!(store.load_task(&first_turn).unwrap().status, "cancelled");
        assert_eq!(store.load_task(&second_turn).unwrap().status, "pending");

        let events_tool = RlmLiveEventsTool {
            config: config.clone(),
        };
        let events = events_tool
            .execute(ToolInput::new().with_arg("session_id", "cancel.1"))
            .unwrap();
        assert!(events.summary.contains(r#""kind":"turn_cancelled""#));
        assert!(events.summary.contains(r#""reason":"superseded""#));
        assert!(events.summary.contains(&first_turn));
        let cancelled_payload = fs::read_to_string(rlm_live_session_turn_payload_path(
            &config,
            "cancel.1",
            &first_turn,
        ))
        .unwrap();
        assert!(cancelled_payload.contains(r#""status":"cancelled""#));
        assert!(cancelled_payload.contains(r#""cancel_reason":"superseded""#));

        let all = RlmLiveCancelTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "cancel.1")
                .with_arg("all", "true"),
        )
        .unwrap();
        assert!(all.summary.contains(r#""cancelled_count":1"#));
        assert!(all.summary.contains(r#""queued_turns":0"#));
        assert_eq!(store.load_task(&second_turn).unwrap().status, "cancelled");
        let updated = read_rlm_live_session_manifest(&manifest_path, "cancel.1").unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "runtime_thread_id").as_deref(),
            Some(thread_id.as_str())
        );
        assert_eq!(
            rlm_live_manifest_u64_field(&updated, "queued_turns"),
            Some(0)
        );

        let err = RlmLiveCancelTool { config }
            .execute(ToolInput::new().with_arg("session_id", "cancel.1"))
            .unwrap_err();
        assert!(err.to_string().contains("task_id"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_cancel_marks_active_turn_cancel_requested() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-active-cancel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let queued = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "active queued turn")
                    .with_arg("content", "alpha")
                    .with_arg("session_id", "cancel.active")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let turn_id = meta_value(&queued.summary, "meta.rlm_turn_id").unwrap();
        let manifest_path = rlm_live_session_manifest_path(&config, "cancel.active");
        let manifest = read_rlm_live_session_manifest(&manifest_path, "cancel.active").unwrap();
        let thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        let created_at = rlm_live_manifest_string_field(&manifest, "created_at");
        let store = rlm_runtime_store(&config);
        let thread = store.load_thread(&thread_id).unwrap();
        let claimed = store
            .claim_task(&turn_id, "active-cancel-test".to_string())
            .unwrap();
        assert_eq!(claimed.status, "running");
        update_rlm_live_session_turn_payload_status(
            &config,
            "cancel.active",
            &turn_id,
            "running",
            Vec::new(),
        )
        .unwrap();
        write_rlm_live_session_manifest_with_daemon(
            &config,
            "cancel.active",
            "running",
            &thread_id,
            thread.session_id.as_deref(),
            Some(&turn_id),
            0,
            &thread.model,
            &thread.workspace,
            created_at.as_deref(),
            Some(4242),
            Some("test-epoch"),
        )
        .unwrap();

        let cancel = RlmLiveCancelTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "cancel.active")
                .with_arg("task_id", turn_id.clone())
                .with_arg("reason", "active stop"),
        )
        .unwrap();
        assert!(cancel.summary.contains(r#""cancelled_count":1"#));
        assert!(cancel.summary.contains(r#""active_cancelled":true"#));
        assert_eq!(store.load_task(&turn_id).unwrap().status, "cancelled");
        assert_eq!(store.load_task(&turn_id).unwrap().summary, "active stop");

        let payload = fs::read_to_string(rlm_live_session_turn_payload_path(
            &config,
            "cancel.active",
            &turn_id,
        ))
        .unwrap();
        assert!(payload.contains(r#""status":"cancelled""#));
        assert!(payload.contains(r#""cancel_reason":"active stop""#));

        let updated = read_rlm_live_session_manifest(&manifest_path, "cancel.active").unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "status").as_deref(),
            Some("running")
        );
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "active_turn_id").as_deref(),
            Some(turn_id.as_str())
        );
        assert_eq!(
            rlm_live_manifest_u64_field(&updated, "daemon_pid"),
            Some(4242)
        );
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "daemon_epoch").as_deref(),
            Some("test-epoch")
        );
        assert_eq!(
            rlm_live_manifest_u64_field(&updated, "queued_turns"),
            Some(0)
        );

        let events = RlmLiveEventsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "cancel.active"))
        .unwrap();
        assert!(events.summary.contains(r#""kind":"turn_cancelled""#));
        assert!(events.summary.contains(r#""reason":"active stop""#));
        assert!(events.summary.contains(&turn_id));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rlm_process_cancel_force_interrupts_external_owner() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-force-cancel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut owner = Command::new("sleep").arg("30").spawn().unwrap();
        let owner_pid = owner.id() as u64;
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let queued = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "force active turn")
                    .with_arg("content", "alpha")
                    .with_arg("session_id", "cancel.force")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let turn_id = meta_value(&queued.summary, "meta.rlm_turn_id").unwrap();
        let manifest_path = rlm_live_session_manifest_path(&config, "cancel.force");
        let manifest = read_rlm_live_session_manifest(&manifest_path, "cancel.force").unwrap();
        let thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        let created_at = rlm_live_manifest_string_field(&manifest, "created_at");
        let store = rlm_runtime_store(&config);
        let thread = store.load_thread(&thread_id).unwrap();
        store
            .claim_task(&turn_id, "force-cancel-test".to_string())
            .unwrap();
        update_rlm_live_session_turn_payload_status(
            &config,
            "cancel.force",
            &turn_id,
            "running",
            Vec::new(),
        )
        .unwrap();
        write_rlm_live_session_manifest_with_daemon(
            &config,
            "cancel.force",
            "running",
            &thread_id,
            thread.session_id.as_deref(),
            Some(&turn_id),
            0,
            &thread.model,
            &thread.workspace,
            created_at.as_deref(),
            Some(owner_pid),
            Some("force-epoch"),
        )
        .unwrap();

        let cancel = RlmLiveCancelTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "cancel.force")
                .with_arg("task_id", turn_id.clone())
                .with_arg("reason", "force stop")
                .with_arg("force", "true"),
        )
        .unwrap();
        assert!(cancel.summary.contains(r#""active_cancelled":true"#));
        assert!(cancel.summary.contains(r#""active_owner_cancelled":true"#));
        assert!(cancel.summary.contains(r#""force":true"#));
        assert!(cancel.summary.contains(r#""interrupted":true"#));
        assert!(cancel.summary.contains(r#""signal":"SIGTERM""#));
        let _ = owner.wait();

        let updated = read_rlm_live_session_manifest(&manifest_path, "cancel.force").unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "status").as_deref(),
            Some("idle")
        );
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "active_turn_id"),
            None
        );
        assert_eq!(rlm_live_manifest_u64_field(&updated, "daemon_pid"), None);
        assert_eq!(store.load_task(&turn_id).unwrap().status, "cancelled");

        let events = RlmLiveEventsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "cancel.force"))
        .unwrap();
        assert!(events.summary.contains(r#""kind":"worker_interrupted""#));
        assert!(events.summary.contains(r#""interrupted":true"#));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rlm_process_cancel_force_does_not_interrupt_non_active_running_task() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-force-non-active-cancel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut owner = Command::new("sleep").arg("30").spawn().unwrap();
        let owner_pid = owner.id() as u64;
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let first = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "active force guard turn")
                    .with_arg("content", "alpha")
                    .with_arg("session_id", "cancel.force.guard")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let active_turn_id = meta_value(&first.summary, "meta.rlm_turn_id").unwrap();
        let second = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "non active force guard turn")
                    .with_arg("content", "beta")
                    .with_arg("session_id", "cancel.force.guard")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let non_active_turn_id = meta_value(&second.summary, "meta.rlm_turn_id").unwrap();
        let manifest_path = rlm_live_session_manifest_path(&config, "cancel.force.guard");
        let manifest =
            read_rlm_live_session_manifest(&manifest_path, "cancel.force.guard").unwrap();
        let thread_id = rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        let created_at = rlm_live_manifest_string_field(&manifest, "created_at");
        let store = rlm_runtime_store(&config);
        let thread = store.load_thread(&thread_id).unwrap();
        store
            .claim_task(&active_turn_id, "force-guard-active".to_string())
            .unwrap();
        store
            .claim_task(&non_active_turn_id, "force-guard-non-active".to_string())
            .unwrap();
        update_rlm_live_session_turn_payload_status(
            &config,
            "cancel.force.guard",
            &active_turn_id,
            "running",
            Vec::new(),
        )
        .unwrap();
        update_rlm_live_session_turn_payload_status(
            &config,
            "cancel.force.guard",
            &non_active_turn_id,
            "running",
            Vec::new(),
        )
        .unwrap();
        write_rlm_live_session_manifest_with_daemon(
            &config,
            "cancel.force.guard",
            "running",
            &thread_id,
            thread.session_id.as_deref(),
            Some(&active_turn_id),
            0,
            &thread.model,
            &thread.workspace,
            created_at.as_deref(),
            Some(owner_pid),
            Some("force-guard-epoch"),
        )
        .unwrap();

        let cancel = RlmLiveCancelTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "cancel.force.guard")
                .with_arg("task_id", non_active_turn_id.clone())
                .with_arg("reason", "stop non active")
                .with_arg("force", "true"),
        )
        .unwrap();
        assert!(cancel.summary.contains(r#""active_cancelled":true"#));
        assert!(cancel.summary.contains(r#""active_owner_cancelled":false"#));
        assert!(cancel.summary.contains(r#""force":true"#));
        assert!(cancel.summary.contains(r#""interrupted":false"#));
        assert!(cancel.summary.contains(r#""interrupt":null"#));
        assert!(owner.try_wait().unwrap().is_none());

        let updated = read_rlm_live_session_manifest(&manifest_path, "cancel.force.guard").unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "status").as_deref(),
            Some("running")
        );
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "active_turn_id").as_deref(),
            Some(active_turn_id.as_str())
        );
        assert_eq!(
            rlm_live_manifest_u64_field(&updated, "daemon_pid"),
            Some(owner_pid)
        );
        assert_eq!(
            store.load_task(&non_active_turn_id).unwrap().status,
            "cancelled"
        );

        let events = RlmLiveEventsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "cancel.force.guard"))
        .unwrap();
        assert!(!events.summary.contains(r#""kind":"worker_interrupted""#));
        let _ = owner.kill();
        let _ = owner.wait();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_live_task_cancel_check_reads_runtime_cancelled_status() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-cancel-check-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode").join("runtime"));
        let thread = store
            .create_thread(
                "Cancel check".to_string(),
                ".".to_string(),
                "deepseek-chat".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                thread.session_id.as_deref(),
                Some(&thread.id),
                None,
                "rlm_process".to_string(),
                "pending".to_string(),
                "pending check".to_string(),
            )
            .unwrap();
        let mut cancel_check = RlmLiveTaskCancelCheck {
            store: store.clone(),
            task_id: task.id.clone(),
        };
        assert!(!cancel_check.is_cancelled().unwrap());
        store
            .cancel_task(&task.id, "cancel check requested".to_string())
            .unwrap();
        assert!(cancel_check.is_cancelled().unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_run_next_dry_run_loads_oldest_payload() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-run-next-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let first = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "first worker turn")
                    .with_arg("content", "alpha worker payload")
                    .with_arg("session_id", "run.next")
                    .with_arg("steps", "3")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let first_turn = meta_value(&first.summary, "meta.rlm_turn_id").unwrap();
        let second = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "second worker turn")
                    .with_arg("content", "beta worker payload")
                    .with_arg("session_id", "run.next")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let second_turn = meta_value(&second.summary, "meta.rlm_turn_id").unwrap();

        let output = RlmLiveRunNextTool {
            config: config.clone(),
            parent_depth: 0,
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "run.next")
                .with_arg("dry_run", "true"),
        )
        .unwrap();

        assert!(output.summary.contains(r#""dry_run":true"#));
        assert!(output.summary.contains(&first_turn));
        assert!(!output.summary.contains(&second_turn));
        assert!(output.summary.contains("first worker turn"));
        assert!(output.summary.contains("alpha worker payload"));
        assert!(output.summary.contains(r#""steps":"3""#));
        let store = rlm_runtime_store(&config);
        assert_eq!(store.load_task(&first_turn).unwrap().status, "pending");
        let payload = read_rlm_live_session_turn_payload(&config, "run.next", &first_turn).unwrap();
        assert_eq!(payload.status, "queued");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_drain_dry_run_lists_fifo_batch() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-drain-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let first = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "first drain turn")
                    .with_arg("content", "alpha drain payload")
                    .with_arg("session_id", "drain.1")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let first_turn = meta_value(&first.summary, "meta.rlm_turn_id").unwrap();
        let second = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "second drain turn")
                    .with_arg("content", "beta drain payload")
                    .with_arg("session_id", "drain.1")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let second_turn = meta_value(&second.summary, "meta.rlm_turn_id").unwrap();

        let output = RlmLiveDrainTool {
            config: config.clone(),
            parent_depth: 0,
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "drain.1")
                .with_arg("max_turns", "1")
                .with_arg("dry_run", "true"),
        )
        .unwrap();
        assert!(output.summary.contains(r#""dry_run":true"#));
        assert!(output.summary.contains(r#""selected_count":1"#));
        assert!(output.summary.contains(&first_turn));
        assert!(!output.summary.contains(&second_turn));
        let store = rlm_runtime_store(&config);
        assert_eq!(store.load_task(&first_turn).unwrap().status, "pending");
        assert_eq!(store.load_task(&second_turn).unwrap().status, "pending");
        assert_eq!(parse_rlm_live_drain_max_turns(Some("0")).unwrap(), 1);
        assert_eq!(parse_rlm_live_drain_max_turns(Some("1000")).unwrap(), 100);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_recover_requeues_interrupted_active_turn() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-recover-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let queued = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "recover active turn")
                    .with_arg("content", "recover payload")
                    .with_arg("session_id", "recover.1")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let turn_id = meta_value(&queued.summary, "meta.rlm_turn_id").unwrap();
        let manifest_path = rlm_live_session_manifest_path(&config, "recover.1");
        let manifest = read_rlm_live_session_manifest(&manifest_path, "recover.1").unwrap();
        let runtime_thread_id =
            rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        let runtime_session_id = rlm_live_manifest_string_field(&manifest, "runtime_session_id");
        let store = rlm_runtime_store(&config);
        store
            .claim_task(&turn_id, "test-recovery-worker".to_string())
            .unwrap();
        update_rlm_live_session_turn_payload_status(
            &config,
            "recover.1",
            &turn_id,
            "running",
            Vec::new(),
        )
        .unwrap();
        let payload = read_rlm_live_session_turn_payload(&config, "recover.1", &turn_id).unwrap();
        write_rlm_live_session_manifest(
            &config,
            "recover.1",
            "running",
            &runtime_thread_id,
            runtime_session_id.as_deref(),
            Some(&turn_id),
            0,
            &payload.model,
            &payload.workspace,
            rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
        )
        .unwrap();

        let dry = RlmLiveRecoverTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "recover.1")
                .with_arg("dry_run", "true"),
        )
        .unwrap();
        assert!(dry.summary.contains(r#""dry_run":true"#));
        assert!(dry.summary.contains(r#""action":"requeue""#));
        assert_eq!(store.load_task(&turn_id).unwrap().status, "running");

        let output = RlmLiveRecoverTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "recover.1"))
        .unwrap();
        assert!(output.summary.contains(r#""mode":"requeue""#));
        assert!(output.summary.contains(r#""recovered_count":1"#));
        assert!(output.summary.contains(r#""queued_turns":1"#));
        assert!(output.summary.contains(r#""cleared_active":true"#));
        assert_eq!(store.load_task(&turn_id).unwrap().status, "pending");
        let recovered_payload =
            read_rlm_live_session_turn_payload(&config, "recover.1", &turn_id).unwrap();
        assert_eq!(recovered_payload.status, "queued");
        let updated = read_rlm_live_session_manifest(&manifest_path, "recover.1").unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&updated, "active_turn_id"),
            None
        );
        assert_eq!(
            rlm_live_manifest_u64_field(&updated, "queued_turns"),
            Some(1)
        );
        let events = RlmLiveEventsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "recover.1"))
        .unwrap();
        assert!(events.summary.contains(r#""kind":"turn_recovered""#));
        assert!(events.summary.contains(r#""action":"requeue""#));
        assert_eq!(
            parse_rlm_live_recovery_mode(Some("fail")).unwrap(),
            RlmLiveRecoveryMode::Fail
        );
        assert!(parse_rlm_live_recovery_mode(Some("unknown")).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_recover_skips_live_daemon_owner_unless_forced() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-recover-owner-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let queued = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "recover owned turn")
                    .with_arg("content", "owned payload")
                    .with_arg("session_id", "recover.owner")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let turn_id = meta_value(&queued.summary, "meta.rlm_turn_id").unwrap();
        let manifest_path = rlm_live_session_manifest_path(&config, "recover.owner");
        let manifest = read_rlm_live_session_manifest(&manifest_path, "recover.owner").unwrap();
        let runtime_thread_id =
            rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
        let runtime_session_id = rlm_live_manifest_string_field(&manifest, "runtime_session_id");
        let store = rlm_runtime_store(&config);
        store
            .claim_task(&turn_id, "test-owned-worker".to_string())
            .unwrap();
        update_rlm_live_session_turn_payload_status(
            &config,
            "recover.owner",
            &turn_id,
            "running",
            Vec::new(),
        )
        .unwrap();
        let payload =
            read_rlm_live_session_turn_payload(&config, "recover.owner", &turn_id).unwrap();
        write_rlm_live_session_manifest_with_daemon(
            &config,
            "recover.owner",
            "running",
            &runtime_thread_id,
            runtime_session_id.as_deref(),
            Some(&turn_id),
            0,
            &payload.model,
            &payload.workspace,
            rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
            Some(std::process::id() as u64),
            Some("epoch+owned"),
        )
        .unwrap();

        let skipped = RlmLiveRecoverTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "recover.owner"))
        .unwrap();
        assert!(skipped.summary.contains(r#""force":false"#));
        assert!(skipped.summary.contains(r#""daemon_alive":true"#));
        assert!(skipped.summary.contains(r#""recovered_count":0"#));
        assert!(skipped
            .summary
            .contains(r#""action":"skip_live_owner_alive""#));
        assert_eq!(store.load_task(&turn_id).unwrap().status, "running");
        let still_running =
            read_rlm_live_session_manifest(&manifest_path, "recover.owner").unwrap();
        assert_eq!(
            rlm_live_manifest_u64_field(&still_running, "daemon_pid"),
            Some(std::process::id() as u64)
        );
        assert_eq!(
            rlm_live_manifest_string_field(&still_running, "active_turn_id").as_deref(),
            Some(turn_id.as_str())
        );

        let forced = RlmLiveRecoverTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "recover.owner")
                .with_arg("force", "true"),
        )
        .unwrap();
        assert!(forced.summary.contains(r#""force":true"#));
        assert!(forced.summary.contains(r#""recovered_count":1"#));
        assert!(forced.summary.contains(r#""action":"requeue""#));
        assert_eq!(store.load_task(&turn_id).unwrap().status, "pending");
        let forced_manifest =
            read_rlm_live_session_manifest(&manifest_path, "recover.owner").unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&forced_manifest, "active_turn_id"),
            None
        );
        assert_eq!(
            rlm_live_manifest_u64_field(&forced_manifest, "daemon_pid"),
            None
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_recover_all_scans_live_sessions() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-recover-all-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let make_running_turn = |session_id: &str, task: &str| -> String {
            let queued = rlm
                .execute(
                    ToolInput::new()
                        .with_arg("task", task)
                        .with_arg("content", format!("payload for {task}"))
                        .with_arg("session_id", session_id)
                        .with_arg("live", "true"),
                )
                .unwrap();
            let turn_id = meta_value(&queued.summary, "meta.rlm_turn_id").unwrap();
            let manifest_path = rlm_live_session_manifest_path(&config, session_id);
            let manifest = read_rlm_live_session_manifest(&manifest_path, session_id).unwrap();
            let runtime_thread_id =
                rlm_live_manifest_string_field(&manifest, "runtime_thread_id").unwrap();
            let runtime_session_id =
                rlm_live_manifest_string_field(&manifest, "runtime_session_id");
            let store = rlm_runtime_store(&config);
            store
                .claim_task(&turn_id, format!("test-recovery-worker-{session_id}"))
                .unwrap();
            update_rlm_live_session_turn_payload_status(
                &config,
                session_id,
                &turn_id,
                "running",
                Vec::new(),
            )
            .unwrap();
            let payload =
                read_rlm_live_session_turn_payload(&config, session_id, &turn_id).unwrap();
            write_rlm_live_session_manifest(
                &config,
                session_id,
                "running",
                &runtime_thread_id,
                runtime_session_id.as_deref(),
                Some(&turn_id),
                0,
                &payload.model,
                &payload.workspace,
                rlm_live_manifest_string_field(&manifest, "created_at").as_deref(),
            )
            .unwrap();
            turn_id
        };
        let first_turn = make_running_turn("recover.all.1", "first recover all turn");
        let second_turn = make_running_turn("recover.all.2", "second recover all turn");

        let output = RlmLiveRecoverTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("all", "true")
                .with_arg("limit", "10")
                .with_arg("reason", "daemon restart"),
        )
        .unwrap();
        assert!(output.summary.contains(r#""all":true"#));
        assert!(output.summary.contains(r#""scanned_count":2"#));
        assert!(output.summary.contains(r#""recovered_count":2"#));
        assert!(output.summary.contains("recover.all.1"));
        assert!(output.summary.contains("recover.all.2"));
        let store = rlm_runtime_store(&config);
        assert_eq!(store.load_task(&first_turn).unwrap().status, "pending");
        assert_eq!(store.load_task(&second_turn).unwrap().status, "pending");
        assert_eq!(parse_rlm_live_recover_limit(Some("0")).unwrap(), 1);
        assert_eq!(parse_rlm_live_recover_limit(Some("1000")).unwrap(), 100);
        assert!(parse_rlm_live_recover_limit(Some("bad")).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_process_stop_cancels_queue_and_blocks_reuse_until_reset() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join("target").join(format!(
            "dscode-rlm-live-stop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let rlm = RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        };
        let first = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "first stopped turn")
                    .with_arg("content", "alpha stop payload")
                    .with_arg("session_id", "stop.1")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let first_turn = meta_value(&first.summary, "meta.rlm_turn_id").unwrap();
        let second = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "second stopped turn")
                    .with_arg("content", "beta stop payload")
                    .with_arg("session_id", "stop.1")
                    .with_arg("live", "true"),
            )
            .unwrap();
        let second_turn = meta_value(&second.summary, "meta.rlm_turn_id").unwrap();

        let stopped = RlmLiveStopTool {
            config: config.clone(),
        }
        .execute(
            ToolInput::new()
                .with_arg("session_id", "stop.1")
                .with_arg("reason", "done"),
        )
        .unwrap();
        assert!(stopped.summary.contains(r#""status":"stopped""#));
        assert!(stopped.summary.contains(r#""cancelled_count":2"#));
        assert!(stopped.summary.contains(r#""queued_turns":0"#));
        let manifest = read_rlm_live_session_manifest(
            &rlm_live_session_manifest_path(&config, "stop.1"),
            "stop.1",
        )
        .unwrap();
        assert_eq!(
            rlm_live_manifest_string_field(&manifest, "status").as_deref(),
            Some("stopped")
        );
        let store = rlm_runtime_store(&config);
        assert_eq!(store.load_task(&first_turn).unwrap().status, "cancelled");
        assert_eq!(store.load_task(&second_turn).unwrap().status, "cancelled");
        let events = RlmLiveEventsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "stop.1"))
        .unwrap();
        assert!(events.summary.contains(r#""kind":"session_stopped""#));
        assert!(events.summary.contains(r#""reason":"done""#));

        let reuse = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "reuse stopped session")
                    .with_arg("content", "reuse")
                    .with_arg("session_id", "stop.1")
                    .with_arg("live", "true"),
            )
            .unwrap_err();
        assert!(reuse.to_string().contains("reset=true"));
        let restarted = rlm
            .execute(
                ToolInput::new()
                    .with_arg("task", "restart stopped session")
                    .with_arg("content", "restart")
                    .with_arg("session_id", "stop.1")
                    .with_arg("reset", "true")
                    .with_arg("live", "true"),
            )
            .unwrap();
        assert!(restarted.summary.contains("meta.rlm_status=queued"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rlm_chunk_plan_splits_inline_content_with_overlap() {
        let output = RlmChunkPlanTool
            .execute(
                ToolInput::new()
                    .with_arg("content", "abcdefghij")
                    .with_arg("max_chars", "4")
                    .with_arg("overlap", "1"),
            )
            .unwrap();

        assert!(output.summary.contains(r#""source":{"#));
        assert!(output.summary.contains(r#""chunks":3"#));
        assert!(output.summary.contains(r#""context_chars":10"#));
        assert!(output.summary.contains(r#""complete":true"#));
        assert!(output
            .summary
            .contains(r#""index":0,"start":0,"end":4,"chars":4,"text":"abcd""#));
        assert!(output
            .summary
            .contains(r#""index":1,"start":3,"end":7,"chars":4,"text":"defg""#));
        assert!(output
            .summary
            .contains(r#""index":2,"start":6,"end":10,"chars":4,"text":"ghij""#));
    }

    #[test]
    fn rlm_chunk_plan_can_hide_text_and_rejects_bad_overlap() {
        let hidden = RlmChunkPlanTool
            .execute(
                ToolInput::new()
                    .with_arg("content", "abcdefghij")
                    .with_arg("max_chars", "4")
                    .with_arg("include_text", "false"),
            )
            .unwrap();
        assert!(hidden.summary.contains(r#""include_text":false"#));
        assert!(hidden.summary.contains(r#""text":null"#));
        assert!(!hidden.summary.contains("abcd"));

        let error = RlmChunkPlanTool
            .execute(
                ToolInput::new()
                    .with_arg("content", "abcdefghij")
                    .with_arg("max_chars", "4")
                    .with_arg("overlap", "4"),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("overlap must be smaller"));
    }

    #[test]
    fn rlm_map_reduce_plan_builds_map_tasks_and_reduce_prompt() {
        let output = RlmMapReducePlanTool
            .execute(
                ToolInput::new()
                    .with_arg("task", "Summarize repeated motifs")
                    .with_arg("content", "abcdefghij")
                    .with_arg("max_chars", "4")
                    .with_arg("overlap", "1")
                    .with_arg("steps", "3"),
            )
            .unwrap();

        assert!(output
            .summary
            .contains(r#""task":"Summarize repeated motifs""#));
        assert!(output.summary.contains(r#""chunks":3"#));
        assert!(output.summary.contains(r#""map_tasks":["#));
        assert!(output.summary.contains(r#""chunk_index":0"#));
        assert!(output.summary.contains("RLM map step"));
        assert!(output.summary.contains("Chunk: 1 of 3"));
        assert!(output.summary.contains("Chunk text:\\nabcd"));
        assert!(output.summary.contains(r#""steps":"3""#));
        assert!(output.summary.contains(r#""map_tasks_omitted":0"#));
        assert!(output.summary.contains("RLM reduce step"));
        assert!(output.summary.contains("Expected map outputs: 3"));
    }

    #[test]
    fn rlm_map_reduce_plan_honors_map_limit_and_hidden_text() {
        let output = RlmMapReducePlanTool
            .execute(
                ToolInput::new()
                    .with_arg("task", "Classify")
                    .with_arg("content", "abcdefghij")
                    .with_arg("max_chars", "3")
                    .with_arg("map_limit", "2")
                    .with_arg("include_text", "false"),
            )
            .unwrap();

        assert!(output.summary.contains(r#""include_text":false"#));
        assert!(output.summary.contains(r#""map_limit":2"#));
        assert!(output.summary.contains(r#""map_tasks_omitted":2"#));
        assert!(output.summary.contains(r#""text":null"#));
        assert!(!output.summary.contains("Chunk text:\\nabc"));
        assert!(output
            .summary
            .contains("Chunk text omitted by include_text=false"));
    }

    #[test]
    fn rlm_recursive_plan_builds_multi_round_reduce_tree() {
        let output = RlmRecursivePlanTool
            .execute(
                ToolInput::new()
                    .with_arg("task", "Extract decisions")
                    .with_arg("content", "abcdefghij")
                    .with_arg("max_chars", "2")
                    .with_arg("fan_in", "2")
                    .with_arg("map_limit", "3")
                    .with_arg("include_text", "false")
                    .with_arg("steps", "5"),
            )
            .unwrap();

        assert!(output.summary.contains(r#""task":"Extract decisions""#));
        assert!(output.summary.contains(r#""chunks":5"#));
        assert!(output.summary.contains(r#""fan_in":2"#));
        assert!(output.summary.contains(r#""map_tasks_omitted":2"#));
        assert!(output.summary.contains(r#""ref":"map:0""#));
        assert!(output.summary.contains(r#""input_refs":["map:0","map:1"]"#));
        assert!(output.summary.contains(r#""round":1"#));
        assert!(output.summary.contains(r#""round":2"#));
        assert!(output.summary.contains(r#""round":3"#));
        assert!(output
            .summary
            .contains(r#""final_output_ref":"round3:group0""#));
        assert!(output.summary.contains("RLM recursive reduce step"));
        assert!(output
            .summary
            .contains("intermediate summary for a later recursive reduce round"));
        assert!(output.summary.contains("produces the final answer"));
        assert!(output.summary.contains(r#""steps":"5""#));
    }

    #[test]
    fn rlm_recursive_plan_clamps_fan_in() {
        assert_eq!(parse_rlm_recursive_fan_in(Some("1")).unwrap(), 2);
        assert_eq!(
            parse_rlm_recursive_fan_in(Some("99")).unwrap(),
            MAX_RLM_RECURSIVE_FAN_IN
        );
        assert!(parse_rlm_recursive_fan_in(Some("nope"))
            .unwrap_err()
            .to_string()
            .contains("fan_in"));
    }

    #[test]
    fn rlm_alias_tools_report_alias_name_in_errors() {
        let rlm = RlmTool {
            tool_name: "llm_query",
            config: AppConfig::default(),
            parent_depth: 0,
        };
        assert_eq!(rlm.name(), "llm_query");
        assert!(rlm
            .execute(ToolInput::new())
            .unwrap_err()
            .to_string()
            .contains("llm_query requires non-empty `context`"));

        let batch = RlmBatchTool {
            tool_name: "llm_query_batched",
            config: AppConfig::default(),
            parent_depth: 0,
        };
        assert_eq!(batch.name(), "llm_query_batched");
        assert!(batch
            .execute(ToolInput::new().with_arg("context", "shared"))
            .unwrap_err()
            .to_string()
            .contains("llm_query_batched requires non-empty `questions`"));
    }

    #[test]
    fn rlm_python_rejects_dangerous_tokens() {
        let error = validate_rlm_python_code("import os\nprint(os.getcwd())")
            .unwrap_err()
            .to_string();

        assert!(error.contains("blocked token"));
        assert!(RlmPythonTool
            .execute(ToolInput::new())
            .unwrap_err()
            .to_string()
            .contains("requires non-empty `code`"));
    }

    #[test]
    fn rlm_python_interpreter_candidates_match_deepseek_tui_fallback_order() {
        assert_eq!(
            RLM_PYTHON_INTERPRETERS,
            &[
                RlmPythonInterpreter {
                    program: "python3",
                    args: &[],
                },
                RlmPythonInterpreter {
                    program: "python",
                    args: &[],
                },
                RlmPythonInterpreter {
                    program: "py",
                    args: &["-3"],
                },
            ]
        );
        assert_eq!(
            render_rlm_python_interpreter(RlmPythonInterpreter {
                program: "py",
                args: &["-3"],
            }),
            "py -3"
        );
    }

    #[test]
    fn rlm_python_executes_small_snippet_when_python_is_available() {
        if resolve_rlm_python_interpreter().is_none() {
            return;
        }
        let output = RlmPythonTool
            .execute(
                ToolInput::new()
                    .with_arg("context", "alpha beta alpha")
                    .with_arg("question", "count alpha")
                    .with_arg(
                        "code",
                        "words = context.split()\ncounts = Counter(words)\nanswer = counts['alpha']\nprint(answer)",
                    ),
            )
            .unwrap();

        assert!(output.summary.contains(r#""ok": true"#));
        assert!(output.summary.contains(r#""answer": 2"#));
        assert!(output.summary.contains(r#""stdout": "2\n""#));
    }

    #[test]
    fn rlm_python_chunk_helpers_cover_context_when_python_is_available() {
        if resolve_rlm_python_interpreter().is_none() {
            return;
        }
        let output = RlmPythonTool
            .execute(
                ToolInput::new()
                    .with_arg("context", "abcdefghij")
                    .with_arg(
                        "code",
                        "chunks = chunk_context(max_chars=5, overlap=1)\ncoverage = chunk_coverage(chunks)\nprint(coverage['complete'])",
                    ),
            )
            .unwrap();

        assert!(output.summary.contains(r#""chunks": 3"#));
        assert!(output.summary.contains(r#""context_chars": 10"#));
        assert!(output.summary.contains(r#""complete": true"#));
        assert!(output.summary.contains(r#""stdout": "True\n""#));

        let error = RlmPythonTool
            .execute(
                ToolInput::new()
                    .with_arg("context", "abcdef")
                    .with_arg("code", "chunk_context(max_chars=2, overlap=2)"),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("overlap must be smaller than max_chars"));

        let root = std::env::temp_dir().join(format!(
            "dscode-rlm-python-chunks-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let session_output = RlmPythonSessionTool { config }
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "chunk-helper")
                    .with_arg("context", "abcdef")
                    .with_arg(
                        "code",
                        "state['coverage'] = chunk_coverage(chunk_context(max_chars=3, overlap=1))\nprint(state['coverage']['complete'])",
                    ),
            )
            .unwrap();
        assert!(session_output.summary.contains(r#""complete": true"#));
    }

    #[test]
    fn rlm_python_ctx_alias_matches_context_when_python_is_available() {
        if resolve_rlm_python_interpreter().is_none() {
            return;
        }
        let output = RlmPythonTool
            .execute(
                ToolInput::new()
                    .with_arg("context", "alias check")
                    .with_arg(
                        "code",
                        "same = ctx == context\nvars_seen = SHOW_VARS()\nprint(ctx)",
                    ),
            )
            .unwrap();

        assert!(output.summary.contains(r#""same": true"#));
        assert!(output.summary.contains(r#""stdout": "alias check\n""#));
        assert!(!output.summary.contains(r#""ctx":"#));

        let root = std::env::temp_dir().join(format!(
            "dscode-rlm-python-ctx-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let session_output = RlmPythonSessionTool { config }
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "ctx")
                    .with_arg("context", "session alias")
                    .with_arg("code", "state['same'] = ctx == context\nprint(ctx)"),
            )
            .unwrap();
        assert!(session_output.summary.contains(r#""same": true"#));
        assert!(session_output
            .summary
            .contains(r#""stdout": "session alias\n""#));
    }

    #[test]
    fn rlm_python_repl_helpers_surface_vars_final_and_state_when_python_is_available() {
        if resolve_rlm_python_interpreter().is_none() {
            return;
        }
        let output = RlmPythonTool
            .execute(ToolInput::new().with_arg(
                "code",
                "repl_set('answer', 42)\nseen = repl_get('answer')\nvars_seen = SHOW_VARS()\nFINAL_VAR('seen')",
            ))
            .unwrap();

        assert!(output.summary.contains(r#""seen": 42"#));
        assert!(output.summary.contains(r#""final": "42""#));
        assert!(output.summary.contains(r#""answer": 42"#));
        assert!(output.summary.contains(r#""stdout": "42\n""#));

        let root = std::env::temp_dir().join(format!(
            "dscode-rlm-python-repl-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let tool = RlmPythonSessionTool {
            config: config.clone(),
        };
        tool.execute(
            ToolInput::new()
                .with_arg("session_id", "repl")
                .with_arg("code", "repl_set('cached', 7)"),
        )
        .unwrap();
        let persisted = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "repl")
                    .with_arg("code", "value = repl_get('cached')\nFINAL_VAR('value')"),
            )
            .unwrap();
        assert!(persisted.summary.contains(r#""value": 7"#));
        assert!(persisted.summary.contains(r#""final": "7""#));
    }

    #[test]
    fn rlm_python_timeout_is_clamped() {
        assert_eq!(
            parse_rlm_python_timeout(Some("1")).unwrap(),
            Duration::from_millis(100)
        );
        assert_eq!(
            parse_rlm_python_timeout(Some("9000")).unwrap(),
            Duration::from_millis(MAX_RLM_PYTHON_TIMEOUT_MS)
        );
        assert!(parse_rlm_python_timeout(Some("nope"))
            .unwrap_err()
            .to_string()
            .contains("timeout_ms"));
    }

    #[test]
    fn rlm_python_session_rejects_unsafe_session_id() {
        assert!(validate_rlm_python_session_id("analysis-1.ok").is_ok());
        assert!(validate_rlm_python_session_id("../escape")
            .unwrap_err()
            .to_string()
            .contains("session_id"));
    }

    #[test]
    fn rlm_python_session_persists_and_resets_state_when_python_is_available() {
        if resolve_rlm_python_interpreter().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "dscode-rlm-python-session-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let tool = RlmPythonSessionTool {
            config: config.clone(),
        };
        let code =
            "state['total'] = state.get('total', 0) + len(context.split())\nprint(state['total'])";

        let first = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "chunk-counts")
                    .with_arg("context", "alpha beta")
                    .with_arg("code", code),
            )
            .unwrap();
        let second = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "chunk-counts")
                    .with_arg("context", "gamma")
                    .with_arg("code", code),
            )
            .unwrap();
        let reset = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "chunk-counts")
                    .with_arg("context", "delta")
                    .with_arg("reset", "true")
                    .with_arg("code", code),
            )
            .unwrap();

        assert!(first.summary.contains(r#""total": 2"#));
        assert!(second.summary.contains(r#""total": 3"#));
        assert!(reset.summary.contains(r#""total": 1"#));
        assert!(fs::read_to_string(
            root.join(".dscode")
                .join("rlm-python")
                .join("chunk-counts.json")
        )
        .unwrap()
        .contains(r#""total":1"#));
    }

    #[test]
    fn rlm_python_session_auto_persists_safe_locals_when_python_is_available() {
        if resolve_rlm_python_interpreter().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "dscode-rlm-python-session-locals-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let tool = RlmPythonSessionTool {
            config: config.clone(),
        };
        let sessions_dir = root.join(".dscode").join("rlm-python");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(
            sessions_dir.join("locals.json"),
            r#"{"count":4,"_hidden":8,"state":"bad"}"#,
        )
        .unwrap();

        let preloaded = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "locals")
                    .with_arg(
                        "code",
                        "count += 1\nvars_seen = SHOW_VARS()\nhidden_local = '_hidden' in vars_seen\nstate_value = state['state']\nFINAL_VAR('count')",
                    ),
            )
            .unwrap();
        let first = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "locals")
                    .with_arg("reset", "true")
                    .with_arg("code", "count = 2\nprivate = 9\nprint(count)"),
            )
            .unwrap();
        let second = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "locals")
                    .with_arg("code", "count += 3\nFINAL_VAR('count')"),
            )
            .unwrap();
        let reset = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "locals")
                    .with_arg("reset", "true")
                    .with_arg("code", "count = state.get('count', 0)\nFINAL_VAR('count')"),
            )
            .unwrap();

        assert!(preloaded.summary.contains(r#""count": 5"#));
        assert!(preloaded.summary.contains(r#""hidden_local": false"#));
        assert!(preloaded.summary.contains(r#""state_value": "bad""#));
        assert!(first.summary.contains(r#""count": 2"#));
        assert!(second.summary.contains(r#""count": 5"#));
        assert!(second.summary.contains(r#""final": "5""#));
        assert!(reset.summary.contains(r#""count": 0"#));
        let stored =
            fs::read_to_string(root.join(".dscode").join("rlm-python").join("locals.json"))
                .unwrap();
        assert!(stored.contains(r#""count":0"#));
        assert!(!stored.contains("__builtins__"));
    }

    #[test]
    fn rlm_python_session_persistent_process_reuses_python_pid_when_available() {
        if resolve_rlm_python_interpreter().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "dscode-rlm-python-persistent-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let tool = RlmPythonSessionTool {
            config: config.clone(),
        };

        let first = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "proc")
                    .with_arg("persistent", "true")
                    .with_arg("reset", "true")
                    .with_arg("code", "counter = 1\nFINAL_VAR('counter')"),
            )
            .unwrap();
        let second = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "proc")
                    .with_arg("persistent", "true")
                    .with_arg("code", "counter += 1\nFINAL_VAR('counter')"),
            )
            .unwrap();
        let reset = tool
            .execute(
                ToolInput::new()
                    .with_arg("session_id", "proc")
                    .with_arg("persistent", "true")
                    .with_arg("reset", "true")
                    .with_arg(
                        "code",
                        "counter = state.get('counter', 9)\nFINAL_VAR('counter')",
                    ),
            )
            .unwrap();

        assert!(first.summary.contains(r#""persistent": true"#));
        assert!(second.summary.contains(r#""counter": 2"#));
        assert_eq!(
            json_number_field(&first.summary, "pid"),
            json_number_field(&second.summary, "pid")
        );
        assert!(reset.summary.contains(r#""counter": 9"#));

        let inventory = RlmPythonSessionsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "proc"))
        .unwrap();
        assert!(inventory
            .summary
            .contains(r#""process":{"active":true,"pid":"#));
        assert!(inventory.summary.contains(&format!(
            r#""pid":{}"#,
            json_number_field(&reset.summary, "pid")
        )));

        let listed = RlmPythonSessionsTool { config }
            .execute(ToolInput::new())
            .unwrap();
        assert!(listed.summary.contains(r#""session_id":"proc""#));
        assert!(listed.summary.contains(r#""process":{"active":true"#));
    }

    fn json_number_field(summary: &str, field: &str) -> String {
        let value = parse_json_value(summary).unwrap();
        let JsonValue::Object(object) = value else {
            panic!("expected object");
        };
        match object.get(field) {
            Some(JsonValue::Number(value)) => value.clone(),
            other => panic!("expected numeric field {field}, got {other:?}"),
        }
    }

    #[test]
    fn rlm_python_sessions_lists_and_reads_state_files() {
        let root = std::env::temp_dir().join(format!(
            "dscode-rlm-python-sessions-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        let sessions_dir = root.join(".dscode").join("rlm-python");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("alpha.json"), r#"{"count":2}"#).unwrap();
        fs::write(sessions_dir.join("broken.json"), r#"["not","object"]"#).unwrap();
        fs::write(sessions_dir.join("notes.txt"), r#"{"ignored":true}"#).unwrap();

        let tool = RlmPythonSessionsTool {
            config: config.clone(),
        };
        let listed = tool.execute(ToolInput::new()).unwrap();

        assert!(listed.summary.contains(r#""session_id":"alpha""#));
        assert!(listed.summary.contains(r#""count":2"#));
        assert!(listed.summary.contains(r#""session_id":"broken""#));
        assert!(listed.summary.contains(r#""errors":["#));
        assert!(!listed.summary.contains("notes.txt"));

        let read = tool
            .execute(ToolInput::new().with_arg("session_id", "alpha"))
            .unwrap();
        assert!(read.summary.contains(r#""exists":true"#));
        assert!(read.summary.contains(r#""state":{"count":2}"#));
        assert!(read.summary.contains(r#""process":{"active":false}"#));

        let missing = tool
            .execute(ToolInput::new().with_arg("session_id", "missing"))
            .unwrap();
        assert!(missing.summary.contains(r#""exists":false"#));
        assert!(missing.summary.contains(r#""state":{}"#));
        assert!(missing.summary.contains(r#""process":{"active":false}"#));
    }

    #[test]
    fn rlm_python_sessions_rejects_bad_inputs_and_clamps_limit() {
        assert_eq!(parse_rlm_python_sessions_limit(None).unwrap(), 20);
        assert_eq!(parse_rlm_python_sessions_limit(Some("0")).unwrap(), 1);
        assert_eq!(parse_rlm_python_sessions_limit(Some("1000")).unwrap(), 100);
        assert!(parse_rlm_python_sessions_limit(Some("nope"))
            .unwrap_err()
            .to_string()
            .contains("limit"));

        let tool = RlmPythonSessionsTool {
            config: AppConfig::default(),
        };
        assert!(tool
            .execute(ToolInput::new().with_arg("session_id", "../escape"))
            .unwrap_err()
            .to_string()
            .contains("session_id"));
    }

    #[test]
    fn render_rlm_batch_tasks_builds_parallel_json_tasks() {
        let questions = parse_rlm_batch_questions(
            r#"["Classify alpha",{"question":"Compare beta","strategy":"compare"}]"#,
        )
        .unwrap();

        let tasks = render_rlm_batch_tasks("shared context", &questions, "classify", "3");

        assert!(tasks.contains("Classify alpha"));
        assert!(tasks.contains("Compare beta"));
        assert!(tasks.contains("Strategy: classify"));
        assert!(tasks.contains("Strategy: compare"));
        assert!(tasks.contains("\"steps\":\"3\""));
    }

    #[test]
    fn parse_rlm_batch_questions_rejects_too_many_questions() {
        let raw = r#"["a","b","c","d","e","f","g","h","i","j","k","l","m","n","o","p","q"]"#;

        let error = parse_rlm_batch_questions(raw).unwrap_err().to_string();

        assert!(error.contains("at most 16"));
    }

    #[test]
    fn parse_rlm_batch_questions_accepts_sixteen_questions() {
        let raw = r#"["a","b","c","d","e","f","g","h","i","j","k","l","m","n","o","p"]"#;
        let questions = parse_rlm_batch_questions(raw).unwrap();

        assert_eq!(questions.len(), 16);
    }
}
