use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::cli::app::McpAction;
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::error::{app_error, AppResult};
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_value_to_string, parse_json_value,
    parse_root_object, JsonValue,
};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(action: McpAction) -> AppResult<()> {
    let config = load_or_default()?;
    match action {
        McpAction::List => list_servers(&config),
        McpAction::Doctor => doctor(&config),
        McpAction::Tools { server } => list_remote_tools(&config, server.as_deref()),
        McpAction::Call {
            server,
            tool,
            arguments_json,
        } => call_remote_tool(&config, &server, &tool, arguments_json.as_deref()),
        McpAction::Init { force } => {
            let path = init_mcp_config_at(&std::env::current_dir()?, &config, force)?;
            println!("initialized MCP config: {}", path.display());
            Ok(())
        }
    }
}

fn list_servers(config: &AppConfig) -> AppResult<()> {
    if !config.mcp.enabled {
        println!("MCP is disabled by config: mcp.enabled = false");
        return Ok(());
    }

    let inventory = load_inventory(config)?;
    print_sources(&inventory);

    if inventory.servers.is_empty() {
        println!("No MCP servers configured. Run `deepseek mcp init` to create .dscode/mcp.json.");
        return Ok(());
    }

    println!("MCP servers:");
    for server in &inventory.servers {
        let status = if server.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let detail = match server.transport.as_str() {
            "stdio" => server
                .command
                .as_deref()
                .map(|command| {
                    if server.args.is_empty() {
                        command.to_string()
                    } else {
                        format!("{command} {}", server.args.join(" "))
                    }
                })
                .unwrap_or_else(|| "(missing command)".to_string()),
            _ => server.url.as_deref().unwrap_or("(missing url)").to_string(),
        };
        let env = if server.env.is_empty() {
            "-".to_string()
        } else {
            server.env.keys().cloned().collect::<Vec<_>>().join(",")
        };
        println!(
            "- {} [{} {}] {} (source={}, env={})",
            server.name, status, server.transport, detail, server.source, env
        );
    }

    Ok(())
}

fn doctor(config: &AppConfig) -> AppResult<()> {
    if !config.mcp.enabled {
        println!("MCP is disabled by config: mcp.enabled = false");
        return Ok(());
    }

    let inventory = load_inventory(config)?;
    print_sources(&inventory);
    let enabled = inventory
        .servers
        .iter()
        .filter(|server| server.enabled)
        .count();
    println!(
        "mcp doctor: ok ({} server(s), {} enabled)",
        inventory.servers.len(),
        enabled
    );
    Ok(())
}

fn list_remote_tools(config: &AppConfig, requested_server: Option<&str>) -> AppResult<()> {
    if !config.mcp.enabled {
        println!("MCP is disabled by config: mcp.enabled = false");
        return Ok(());
    }

    let inventory = load_inventory(config)?;
    print_sources(&inventory);
    let targets = select_tool_targets(&inventory, requested_server)?;

    if targets.is_empty() {
        println!("No enabled MCP servers configured. Run `deepseek mcp list` to inspect config.");
        return Ok(());
    }

    println!("MCP remote tools:");
    for server in targets {
        if server.transport != "stdio" {
            let message = format!(
                "mcp tools currently supports stdio servers only; `{}` uses {}",
                server.name, server.transport
            );
            if requested_server.is_some() {
                return Err(app_error(message));
            }
            println!(
                "- {} [{}]: skipped ({message})",
                server.name, server.transport
            );
            continue;
        }

        let tools = list_stdio_tools(server)?;
        println!("- {} [stdio]: {} tool(s)", server.name, tools.len());
        for tool in tools {
            let description = tool.description.as_deref().unwrap_or("-");
            println!("  - {}: {}", tool.name, compact_inline(description, 140));
            if let Some(input_schema) = tool.input_schema {
                println!("    schema: {}", compact_inline(&input_schema, 220));
            }
        }
    }

    Ok(())
}

fn call_remote_tool(
    config: &AppConfig,
    server_name: &str,
    tool_name: &str,
    arguments_json: Option<&str>,
) -> AppResult<()> {
    if !config.mcp.enabled {
        println!("MCP is disabled by config: mcp.enabled = false");
        return Ok(());
    }

    let arguments = parse_call_arguments(arguments_json)?;
    let inventory = load_inventory(config)?;
    print_sources(&inventory);
    let targets = select_tool_targets(&inventory, Some(server_name))?;
    let server = targets[0];
    if server.transport != "stdio" {
        return Err(app_error(format!(
            "mcp call currently supports stdio servers only; `{}` uses {}",
            server.name, server.transport
        )));
    }

    let result = call_stdio_tool(server, tool_name, &arguments)?;
    println!("MCP tool call:");
    println!(
        "- {}/{} [stdio]: {}",
        server.name,
        tool_name,
        if result.is_error { "tool-error" } else { "ok" }
    );
    if result.content.is_empty() {
        println!("  content: -");
    } else {
        println!("  content:");
        for item in result.content {
            println!("  - {}", compact_inline(&item, 260));
        }
    }
    if let Some(structured_content) = result.structured_content {
        println!(
            "  structuredContent: {}",
            compact_inline(&structured_content, 260)
        );
    }

    Ok(())
}

fn parse_call_arguments(arguments_json: Option<&str>) -> AppResult<BTreeMap<String, JsonValue>> {
    let Some(arguments_json) = arguments_json else {
        return Ok(BTreeMap::new());
    };
    let parsed = parse_json_value(arguments_json.trim()).map_err(|error| {
        app_error(format!(
            "failed to parse mcp call JSON arguments object: {error}"
        ))
    })?;
    let JsonValue::Object(arguments) = parsed else {
        return Err(app_error(
            "mcp call JSON arguments must be an object, for example '{\"path\":\"README.md\"}'",
        ));
    };
    Ok(arguments)
}

fn select_tool_targets<'a>(
    inventory: &'a McpInventory,
    requested_server: Option<&str>,
) -> AppResult<Vec<&'a McpServer>> {
    if let Some(name) = requested_server {
        let server = inventory
            .servers
            .iter()
            .find(|server| server.name == name)
            .ok_or_else(|| app_error(format!("unknown MCP server: {name}")))?;
        if !server.enabled {
            return Err(app_error(format!("MCP server `{name}` is disabled")));
        }
        return Ok(vec![server]);
    }

    Ok(inventory
        .servers
        .iter()
        .filter(|server| server.enabled)
        .collect())
}

fn list_stdio_tools(server: &McpServer) -> AppResult<Vec<McpRemoteTool>> {
    let command = server
        .command
        .as_deref()
        .ok_or_else(|| app_error(format!("stdio MCP server `{}` has no command", server.name)))?;
    let mut command_builder = Command::new(command);
    command_builder
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in &server.env {
        command_builder.env(key, value);
    }

    let mut child = command_builder.spawn().map_err(|error| {
        app_error(format!(
            "failed to start stdio MCP server `{}` with `{}`: {error}",
            server.name, command
        ))
    })?;

    let result = run_stdio_tools_session(server, &mut child);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn call_stdio_tool(
    server: &McpServer,
    tool_name: &str,
    arguments: &BTreeMap<String, JsonValue>,
) -> AppResult<McpToolCallResult> {
    let command = server
        .command
        .as_deref()
        .ok_or_else(|| app_error(format!("stdio MCP server `{}` has no command", server.name)))?;
    let mut command_builder = Command::new(command);
    command_builder
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in &server.env {
        command_builder.env(key, value);
    }

    let mut child = command_builder.spawn().map_err(|error| {
        app_error(format!(
            "failed to start stdio MCP server `{}` with `{}`: {error}",
            server.name, command
        ))
    })?;

    let result = run_stdio_call_session(server, &mut child, tool_name, arguments);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn run_stdio_call_session(
    server: &McpServer,
    child: &mut std::process::Child,
    tool_name: &str,
    arguments: &BTreeMap<String, JsonValue>,
) -> AppResult<McpToolCallResult> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| app_error("failed to open MCP server stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| app_error("failed to open MCP server stdout"))?;
    let receiver = spawn_stdout_reader(stdout);

    send_json_rpc(&mut stdin, &build_initialize_request(1))?;
    read_json_rpc_response(&receiver, 1, MCP_RESPONSE_TIMEOUT).map_err(|error| {
        app_error(format!(
            "MCP server `{}` initialize failed: {error}",
            server.name
        ))
    })?;
    send_json_rpc(&mut stdin, build_initialized_notification())?;
    send_json_rpc(
        &mut stdin,
        &build_tools_call_request(2, tool_name, arguments),
    )?;
    let response = read_json_rpc_response(&receiver, 2, MCP_RESPONSE_TIMEOUT).map_err(|error| {
        app_error(format!(
            "MCP server `{}` tools/call failed: {error}",
            server.name
        ))
    })?;
    parse_tool_call_result(&response)
}

fn run_stdio_tools_session(
    server: &McpServer,
    child: &mut std::process::Child,
) -> AppResult<Vec<McpRemoteTool>> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| app_error("failed to open MCP server stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| app_error("failed to open MCP server stdout"))?;
    let receiver = spawn_stdout_reader(stdout);

    send_json_rpc(&mut stdin, &build_initialize_request(1))?;
    read_json_rpc_response(&receiver, 1, MCP_RESPONSE_TIMEOUT).map_err(|error| {
        app_error(format!(
            "MCP server `{}` initialize failed: {error}",
            server.name
        ))
    })?;
    send_json_rpc(&mut stdin, build_initialized_notification())?;

    let mut request_id = 2u64;
    let mut cursor: Option<String> = None;
    let mut tools = Vec::new();
    loop {
        send_json_rpc(
            &mut stdin,
            &build_tools_list_request(request_id, cursor.as_deref()),
        )?;
        let response = read_json_rpc_response(&receiver, request_id, MCP_RESPONSE_TIMEOUT)
            .map_err(|error| {
                app_error(format!(
                    "MCP server `{}` tools/list failed: {error}",
                    server.name
                ))
            })?;
        let (mut page_tools, next_cursor) = parse_tools_list_result(&response)?;
        tools.append(&mut page_tools);
        let Some(next_cursor) = next_cursor else {
            break;
        };
        cursor = Some(next_cursor);
        request_id += 1;
    }

    Ok(tools)
}

fn spawn_stdout_reader(stdout: std::process::ChildStdout) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if sender
                .send(line.map_err(|error| error.to_string()))
                .is_err()
            {
                break;
            }
        }
    });
    receiver
}

fn send_json_rpc(stdin: &mut std::process::ChildStdin, message: &str) -> AppResult<()> {
    stdin.write_all(message.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn read_json_rpc_response(
    receiver: &Receiver<Result<String, String>>,
    expected_id: u64,
    timeout: Duration,
) -> AppResult<BTreeMap<String, JsonValue>> {
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(app_error(format!(
                "timed out waiting for JSON-RPC response id {expected_id}"
            )));
        }
        let remaining = deadline.saturating_duration_since(now);
        let line = match receiver.recv_timeout(remaining) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => return Err(app_error(format!("failed to read stdout: {error}"))),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(app_error(format!(
                    "timed out waiting for JSON-RPC response id {expected_id}"
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(app_error(format!(
                    "MCP server stdout closed before response id {expected_id}"
                )));
            }
        };

        if line.trim().is_empty() {
            continue;
        }
        let root = parse_root_object(line.trim()).map_err(|error| {
            app_error(format!(
                "MCP server wrote invalid JSON-RPC message `{}`: {error}",
                compact_inline(&line, 160)
            ))
        })?;
        if !response_id_matches(&root, expected_id) {
            continue;
        }
        if let Some(error) = root.get("error") {
            return Err(app_error(format!(
                "JSON-RPC error for id {expected_id}: {}",
                describe_json_rpc_error(error)
            )));
        }
        if !root.contains_key("result") {
            return Err(app_error(format!(
                "JSON-RPC response id {expected_id} missing `result`"
            )));
        }
        return Ok(root);
    }
}

fn response_id_matches(root: &BTreeMap<String, JsonValue>, expected_id: u64) -> bool {
    match root.get("id") {
        Some(JsonValue::Number(value)) => value == &expected_id.to_string(),
        Some(JsonValue::String(value)) => value == &expected_id.to_string(),
        _ => false,
    }
}

fn describe_json_rpc_error(error: &JsonValue) -> String {
    let Some(object) = json_as_object(error) else {
        return json_value_to_string(error);
    };
    object
        .get("message")
        .and_then(json_as_string)
        .map(ToString::to_string)
        .unwrap_or_else(|| json_value_to_string(error))
}

fn build_initialize_request(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"initialize","params":{{"protocolVersion":"{protocol}","capabilities":{{}},"clientInfo":{{"name":"DeepseekCode","version":"{version}"}}}}}}"#,
        protocol = MCP_PROTOCOL_VERSION,
        version = env!("CARGO_PKG_VERSION"),
    )
}

fn build_initialized_notification() -> &'static str {
    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
}

fn build_tools_list_request(id: u64, cursor: Option<&str>) -> String {
    match cursor {
        Some(cursor) => format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/list","params":{{"cursor":"{}"}}}}"#,
            crate::util::json::json_escape(cursor)
        ),
        None => format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/list","params":{{}}}}"#),
    }
}

fn build_tools_call_request(
    id: u64,
    tool_name: &str,
    arguments: &BTreeMap<String, JsonValue>,
) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{}","arguments":{}}}}}"#,
        crate::util::json::json_escape(tool_name),
        json_value_to_string(&JsonValue::Object(arguments.clone())),
    )
}

fn parse_tools_list_result(
    response: &BTreeMap<String, JsonValue>,
) -> AppResult<(Vec<McpRemoteTool>, Option<String>)> {
    let result = response
        .get("result")
        .and_then(json_as_object)
        .ok_or_else(|| app_error("tools/list response `result` must be an object"))?;
    let tools = result
        .get("tools")
        .and_then(json_as_array)
        .ok_or_else(|| app_error("tools/list response `result.tools` must be an array"))?;
    let next_cursor = result
        .get("nextCursor")
        .and_then(json_as_string)
        .map(ToString::to_string);

    let mut parsed = Vec::with_capacity(tools.len());
    for tool in tools {
        let object = json_as_object(tool)
            .ok_or_else(|| app_error("tools/list response tool entries must be objects"))?;
        let name = object
            .get("name")
            .and_then(json_as_string)
            .ok_or_else(|| app_error("tools/list response tool entry missing string `name`"))?;
        let description = object
            .get("description")
            .and_then(json_as_string)
            .map(ToString::to_string);
        let input_schema = object.get("inputSchema").map(json_value_to_string);
        parsed.push(McpRemoteTool {
            name: name.to_string(),
            description,
            input_schema,
        });
    }

    Ok((parsed, next_cursor))
}

fn parse_tool_call_result(response: &BTreeMap<String, JsonValue>) -> AppResult<McpToolCallResult> {
    let result = response
        .get("result")
        .and_then(json_as_object)
        .ok_or_else(|| app_error("tools/call response `result` must be an object"))?;
    let is_error = result
        .get("isError")
        .and_then(json_as_bool)
        .unwrap_or(false);
    let structured_content = result.get("structuredContent").map(json_value_to_string);
    let content = match result.get("content") {
        Some(value) => {
            let items = json_as_array(value).ok_or_else(|| {
                app_error("tools/call response `result.content` must be an array")
            })?;
            let mut parsed = Vec::with_capacity(items.len());
            for item in items {
                let Some(object) = json_as_object(item) else {
                    parsed.push(json_value_to_string(item));
                    continue;
                };
                match object.get("type").and_then(json_as_string) {
                    Some("text") => parsed.push(
                        object
                            .get("text")
                            .and_then(json_as_string)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    _ => parsed.push(json_value_to_string(item)),
                }
            }
            parsed
        }
        None => Vec::new(),
    };

    Ok(McpToolCallResult {
        is_error,
        content,
        structured_content,
    })
}

fn compact_inline(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = String::new();
    for (index, ch) in normalized.chars().enumerate() {
        if index >= limit {
            output.push_str("...");
            return output;
        }
        output.push(ch);
    }
    output
}

fn print_sources(inventory: &McpInventory) {
    println!("MCP config sources:");
    for source in &inventory.sources {
        println!("- {}: {}", source.scope, source.path.display());
    }
    if inventory.sources.is_empty() {
        println!("- none found");
    }
}

pub(crate) fn init_mcp_config_at(
    root: &Path,
    config: &AppConfig,
    force: bool,
) -> AppResult<PathBuf> {
    let path = root.join(config.mcp.project_file_path());
    if path.exists() && !force {
        return Err(app_error(format!(
            "MCP config already exists: {} (use --force to overwrite)",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, default_mcp_config())?;
    Ok(path)
}

fn default_mcp_config() -> &'static str {
    r#"{
  "mcpServers": {
    "example-filesystem": {
      "disabled": true,
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    }
  }
}
"#
}

fn load_inventory(config: &AppConfig) -> AppResult<McpInventory> {
    let mut inventory = McpInventory::default();
    let mut merged = BTreeMap::<String, McpServer>::new();

    for (scope, path) in [
        ("user", config.mcp.user_file_path()),
        ("project", config.mcp.project_file_path()),
    ] {
        if !path.exists() {
            continue;
        }
        inventory.sources.push(McpSource {
            scope: scope.to_string(),
            path: path.clone(),
        });
        for server in read_mcp_config_file(scope, &path)? {
            merged.insert(server.name.clone(), server);
        }
    }

    inventory.servers = merged.into_values().collect();
    Ok(inventory)
}

fn read_mcp_config_file(scope: &str, path: &Path) -> AppResult<Vec<McpServer>> {
    let content = std::fs::read_to_string(path)?;
    let root = parse_root_object(&content).map_err(|error| {
        app_error(format!(
            "failed to parse MCP config {}: {error}",
            path.display()
        ))
    })?;
    parse_mcp_servers(scope, path, &root)
}

fn parse_mcp_servers(
    scope: &str,
    path: &Path,
    root: &BTreeMap<String, JsonValue>,
) -> AppResult<Vec<McpServer>> {
    let Some(servers_value) = root.get("mcpServers").or_else(|| root.get("servers")) else {
        return Err(app_error(format!(
            "MCP config {} must contain a `mcpServers` object",
            path.display()
        )));
    };
    let Some(servers_object) = json_as_object(servers_value) else {
        return Err(app_error(format!(
            "MCP config {} `mcpServers` must be an object",
            path.display()
        )));
    };

    let mut servers = Vec::new();
    for (name, value) in servers_object {
        let Some(object) = json_as_object(value) else {
            return Err(app_error(format!(
                "MCP server `{name}` in {} must be an object",
                path.display()
            )));
        };
        servers.push(parse_mcp_server(scope, path, name, object)?);
    }
    Ok(servers)
}

fn parse_mcp_server(
    scope: &str,
    path: &Path,
    name: &str,
    object: &BTreeMap<String, JsonValue>,
) -> AppResult<McpServer> {
    let disabled = object
        .get("disabled")
        .and_then(json_as_bool)
        .unwrap_or(false);
    let enabled = object.get("enabled").and_then(json_as_bool).unwrap_or(true) && !disabled;
    let transport = normalize_transport(
        object
            .get("transport")
            .or_else(|| object.get("type"))
            .and_then(json_as_string)
            .unwrap_or_else(|| {
                if object.get("url").is_some() {
                    "http"
                } else {
                    "stdio"
                }
            }),
    )
    .map_err(|error| {
        app_error(format!(
            "MCP server `{name}` in {} has invalid transport: {error}",
            path.display()
        ))
    })?;

    let command = optional_string(object, "command")?;
    let url = optional_string(object, "url")?;
    let args = optional_string_array(object, "args")?;
    let env = optional_string_object(object, "env")?;
    let headers = optional_string_object(object, "headers")?;

    if enabled && transport == "stdio" && command.as_deref().unwrap_or("").trim().is_empty() {
        return Err(app_error(format!(
            "enabled stdio MCP server `{name}` in {} must define `command`",
            path.display()
        )));
    }
    if enabled && transport != "stdio" && url.as_deref().unwrap_or("").trim().is_empty() {
        return Err(app_error(format!(
            "enabled {transport} MCP server `{name}` in {} must define `url`",
            path.display()
        )));
    }

    Ok(McpServer {
        name: name.to_string(),
        source: scope.to_string(),
        transport,
        enabled,
        command,
        args,
        url,
        env,
        headers,
    })
}

fn normalize_transport(raw: &str) -> AppResult<String> {
    match raw {
        "stdio" => Ok("stdio".to_string()),
        "http" | "streamable-http" => Ok("http".to_string()),
        "sse" => Ok("sse".to_string()),
        other => Err(app_error(format!(
            "`{other}` (expected stdio|http|streamable-http|sse)"
        ))),
    }
}

fn optional_string(object: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<Option<String>> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let Some(value) = json_as_string(value) else {
        return Err(app_error(format!("MCP field `{key}` must be a string")));
    };
    Ok(Some(value.to_string()))
}

fn optional_string_array(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<Vec<String>> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = json_as_array(value) else {
        return Err(app_error(format!("MCP field `{key}` must be an array")));
    };
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let Some(value) = json_as_string(item) else {
            return Err(app_error(format!(
                "MCP field `{key}` entries must be strings"
            )));
        };
        result.push(value.to_string());
    }
    Ok(result)
}

fn optional_string_object(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<BTreeMap<String, String>> {
    let Some(value) = object.get(key) else {
        return Ok(BTreeMap::new());
    };
    let Some(map) = json_as_object(value) else {
        return Err(app_error(format!("MCP field `{key}` must be an object")));
    };
    let mut result = BTreeMap::new();
    for (entry_key, entry_value) in map {
        let Some(value) = json_as_string(entry_value) else {
            return Err(app_error(format!(
                "MCP `{key}.{entry_key}` value must be a string"
            )));
        };
        result.insert(entry_key.clone(), value.to_string());
    }
    Ok(result)
}

fn json_as_bool(value: &JsonValue) -> Option<bool> {
    match value {
        JsonValue::Bool(value) => Some(*value),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct McpInventory {
    sources: Vec<McpSource>,
    servers: Vec<McpServer>,
}

#[derive(Debug)]
struct McpSource {
    scope: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct McpServer {
    name: String,
    source: String,
    transport: String,
    enabled: bool,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    env: BTreeMap<String, String>,
    #[allow(dead_code)]
    headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct McpRemoteTool {
    name: String,
    description: Option<String>,
    input_schema: Option<String>,
}

#[derive(Debug, Clone)]
struct McpToolCallResult {
    is_error: bool,
    content: Vec<String>,
    structured_content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-mcp-{name}-{}-{suffix}",
            std::process::id()
        ))
    }

    #[test]
    fn parse_mcp_servers_reads_stdio_server() {
        let root = parse_root_object(
            r#"{
              "mcpServers": {
                "local": {
                  "command": "node",
                  "args": ["server.js"],
                  "env": {"TOKEN": "value"}
                }
              }
            }"#,
        )
        .unwrap();
        let servers = parse_mcp_servers("project", Path::new(".dscode/mcp.json"), &root).unwrap();

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "local");
        assert_eq!(servers[0].transport, "stdio");
        assert_eq!(servers[0].command.as_deref(), Some("node"));
        assert_eq!(servers[0].args, vec!["server.js"]);
        assert_eq!(
            servers[0].env.get("TOKEN").map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn build_mcp_protocol_messages_match_stdio_lifecycle_shape() {
        let init = build_initialize_request(1);
        let root = parse_root_object(&init).unwrap();
        assert_eq!(
            root.get("method").and_then(json_as_string),
            Some("initialize")
        );
        let params = root
            .get("params")
            .and_then(json_as_object)
            .expect("initialize params");
        assert_eq!(
            params.get("protocolVersion").and_then(json_as_string),
            Some(MCP_PROTOCOL_VERSION)
        );

        let list = build_tools_list_request(2, Some("next page"));
        let root = parse_root_object(&list).unwrap();
        assert_eq!(
            root.get("method").and_then(json_as_string),
            Some("tools/list")
        );
        let params = root
            .get("params")
            .and_then(json_as_object)
            .expect("tools/list params");
        assert_eq!(
            params.get("cursor").and_then(json_as_string),
            Some("next page")
        );
        assert_eq!(
            parse_root_object(build_initialized_notification())
                .unwrap()
                .get("method")
                .and_then(json_as_string),
            Some("notifications/initialized")
        );
    }

    #[test]
    fn build_tools_call_request_includes_tool_name_and_arguments() {
        let arguments = parse_call_arguments(Some(r#"{"path":"README.md","limit":2}"#)).unwrap();
        let request = build_tools_call_request(3, "read_file", &arguments);
        let root = parse_root_object(&request).unwrap();
        assert_eq!(
            root.get("method").and_then(json_as_string),
            Some("tools/call")
        );
        let params = root
            .get("params")
            .and_then(json_as_object)
            .expect("tools/call params");
        assert_eq!(
            params.get("name").and_then(json_as_string),
            Some("read_file")
        );
        let args = params
            .get("arguments")
            .and_then(json_as_object)
            .expect("arguments");
        assert_eq!(args.get("path").and_then(json_as_string), Some("README.md"));
    }

    #[test]
    fn parse_call_arguments_rejects_non_object_json() {
        let error = parse_call_arguments(Some("[1,2]")).unwrap_err().to_string();
        assert!(error.contains("must be an object"));
    }

    #[test]
    fn parse_tools_list_result_reads_tools_schema_and_cursor() {
        let root = parse_root_object(
            r#"{
              "jsonrpc": "2.0",
              "id": 2,
              "result": {
                "nextCursor": "page-2",
                "tools": [
                  {
                    "name": "read_file",
                    "description": "Read a file",
                    "inputSchema": {
                      "type": "object",
                      "properties": {"path": {"type": "string"}}
                    }
                  }
                ]
              }
            }"#,
        )
        .unwrap();

        let (tools, next_cursor) = parse_tools_list_result(&root).unwrap();
        assert_eq!(next_cursor.as_deref(), Some("page-2"));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[0].description.as_deref(), Some("Read a file"));
        assert!(tools[0]
            .input_schema
            .as_deref()
            .unwrap()
            .contains("\"properties\""));
    }

    #[test]
    fn parse_tool_call_result_reads_text_structured_content_and_error_flag() {
        let root = parse_root_object(
            r#"{
              "jsonrpc": "2.0",
              "id": 2,
              "result": {
                "isError": true,
                "content": [
                  {"type": "text", "text": "not found"},
                  {"type": "image", "data": "abc", "mimeType": "image/png"}
                ],
                "structuredContent": {"code": "ENOENT"}
              }
            }"#,
        )
        .unwrap();

        let result = parse_tool_call_result(&root).unwrap();
        assert!(result.is_error);
        assert_eq!(result.content[0], "not found");
        assert!(result.content[1].contains("\"image\""));
        assert_eq!(
            result.structured_content.as_deref(),
            Some(r#"{"code":"ENOENT"}"#)
        );
    }

    #[test]
    fn read_json_rpc_response_skips_notifications_until_matching_id() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(
                r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#.to_string(),
            ))
            .unwrap();
        sender
            .send(Ok(
                r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#.to_string()
            ))
            .unwrap();

        let response = read_json_rpc_response(&receiver, 2, Duration::from_millis(50)).unwrap();
        assert!(response.contains_key("result"));
    }

    #[test]
    fn select_tool_targets_rejects_disabled_requested_server() {
        let inventory = McpInventory {
            sources: Vec::new(),
            servers: vec![McpServer {
                name: "disabled".to_string(),
                source: "project".to_string(),
                transport: "stdio".to_string(),
                enabled: false,
                command: None,
                args: Vec::new(),
                url: None,
                env: BTreeMap::new(),
                headers: BTreeMap::new(),
            }],
        };

        let error = select_tool_targets(&inventory, Some("disabled"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("disabled"));
    }

    #[test]
    fn parse_mcp_servers_accepts_disabled_incomplete_server() {
        let root = parse_root_object(
            r#"{
              "mcpServers": {
                "planned": {
                  "disabled": true,
                  "transport": "stdio"
                }
              }
            }"#,
        )
        .unwrap();
        let servers = parse_mcp_servers("project", Path::new(".dscode/mcp.json"), &root).unwrap();

        assert_eq!(servers.len(), 1);
        assert!(!servers[0].enabled);
        assert_eq!(servers[0].command, None);
    }

    #[test]
    fn parse_mcp_servers_rejects_enabled_stdio_without_command() {
        let root = parse_root_object(
            r#"{
              "mcpServers": {
                "bad": {
                  "transport": "stdio"
                }
              }
            }"#,
        )
        .unwrap();
        let error = parse_mcp_servers("project", Path::new(".dscode/mcp.json"), &root)
            .unwrap_err()
            .to_string();

        assert!(error.contains("must define `command`"));
    }

    #[test]
    fn init_mcp_config_refuses_existing_file_without_force() {
        let root = temp_root("init");
        let config = AppConfig::default();
        let path = init_mcp_config_at(&root, &config, false).unwrap();
        std::fs::write(&path, "sentinel").unwrap();

        let error = init_mcp_config_at(&root, &config, false).unwrap_err();
        assert!(error.to_string().contains("already exists"));

        init_mcp_config_at(&root, &config, true).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("mcpServers"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_inventory_project_overrides_user_server_with_same_name() {
        let root = temp_root("merge");
        std::fs::create_dir_all(&root).unwrap();
        let user_file = root.join("user-mcp.json");
        let project_file = root.join("project-mcp.json");
        std::fs::write(
            &user_file,
            r#"{"mcpServers":{"shared":{"command":"user-server"}}}"#,
        )
        .unwrap();
        std::fs::write(
            &project_file,
            r#"{"mcpServers":{"shared":{"command":"project-server"}}}"#,
        )
        .unwrap();

        let mut config = AppConfig::default();
        config.mcp.user_file = user_file.display().to_string();
        config.mcp.project_file = project_file.display().to_string();
        let inventory = load_inventory(&config).unwrap();

        assert_eq!(inventory.servers.len(), 1);
        assert_eq!(inventory.servers[0].source, "project");
        assert_eq!(
            inventory.servers[0].command.as_deref(),
            Some("project-server")
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
