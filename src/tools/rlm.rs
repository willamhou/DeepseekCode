use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::types::AppConfig;
use crate::error::{tool_failure, AppResult};
use crate::tools::dispatch_subagent::{DispatchSubagentTool, DispatchSubagentsTool};
use crate::tools::types::{Tool, ToolInput, ToolOutput};
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
        let process_input = load_rlm_process_input(&input)?;
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
            let summary = format!(
                "{{\"session_id\":\"{}\",\"path\":\"{}\",\"exists\":{},\"bytes\":{},\"session\":{}}}",
                json_escape(session_id),
                json_escape(&path.display().to_string()),
                exists,
                bytes,
                json_value_to_string(&rlm_model_session_to_json(&session))
            );
            return Ok(ToolOutput { summary });
        }

        let limit = parse_rlm_model_sessions_limit(input.get("limit"))?;
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

        Ok(ToolOutput {
            summary: format!(
                "{{\"sessions\":[{}],\"errors\":[{}],\"limit\":{}}}",
                sessions_json, errors_json, limit
            ),
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
