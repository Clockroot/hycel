//! Bounded MCP 2025-11-25 stdio adapter over Hycel's tested CLI/project services.
//!
//! The server pins a single canonical project root, does not execute project code
//! or access the network, and keeps mutating tools opt-in at process startup.
//! Path validation is not race-resistant against concurrent filesystem replacement.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use hycel_cli::{CliOutput, execute};
use hycel_project::{
    ResourceEditOperation, SceneEditOperation, resolve_existing_project_path,
    validate_relative_project_path,
};
use serde_json::{Value, json};

/// MCP protocol revision implemented by this adapter.
pub const PROTOCOL_VERSION: &str = "2025-11-25";
/// Maximum UTF-8 JSON-RPC message size accepted by stdio transport.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Maximum JSON-RPC response size emitted by stdio transport.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_REQUESTS_PER_SESSION: u64 = 10_000;
const MAX_PENDING_PREVIEWS: usize = 32;

#[derive(Clone, PartialEq, Eq)]
enum PendingOperation {
    Scene(SceneEditOperation),
    Resource(ResourceEditOperation),
}

#[derive(Clone)]
struct PendingPreview {
    relative_path: String,
    operation: PendingOperation,
    original_sha256: String,
    candidate_sha256: String,
}

/// A single-project stdio MCP server.
pub struct McpServer {
    project_root: PathBuf,
    allow_writes: bool,
    initialize_requested: bool,
    initialized: bool,
    request_count: u64,
    next_preview_token: u64,
    pending_previews: BTreeMap<String, PendingPreview>,
}

impl McpServer {
    /// Creates a server pinned to an existing project directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the root cannot be canonicalized or does not pass
    /// Hycel project validation.
    pub fn new(project_root: &Path, allow_writes: bool) -> Result<Self, String> {
        let project_root = project_root
            .canonicalize()
            .map_err(|error| format!("cannot resolve project root: {error}"))?;
        let root_text = project_root
            .to_str()
            .ok_or_else(|| "MCP project root must be a UTF-8 path".to_owned())?;
        let checked = execute(["check", root_text, "--json"].map(OsString::from));
        if checked.exit_code != 0 {
            return Err(format!("project validation failed: {}", checked.stdout));
        }
        Ok(Self {
            project_root,
            allow_writes,
            initialize_requested: false,
            initialized: false,
            request_count: 0,
            next_preview_token: 1,
            pending_previews: BTreeMap::new(),
        })
    }

    /// Handles one newline-delimited JSON-RPC message. Notifications produce no response.
    #[must_use]
    pub fn handle_line(&mut self, line: &[u8]) -> Option<Vec<u8>> {
        let message = line
            .strip_suffix(b"\n")
            .unwrap_or(line)
            .strip_suffix(b"\r")
            .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line));
        if message.len() > MAX_REQUEST_BYTES {
            return Some(error_response(
                None,
                -32700,
                "request exceeds the 1 MiB limit",
            ));
        }
        let request: Value = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(error) => {
                return Some(error_response(
                    None,
                    -32700,
                    &format!("invalid JSON: {error}"),
                ));
            }
        };
        let Some(object) = request.as_object() else {
            return Some(error_response(
                None,
                -32600,
                "MCP stdio expects one JSON-RPC object per line",
            ));
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                request.get("id").filter(|id| is_valid_id(id)).cloned(),
                -32600,
                "jsonrpc must be \"2.0\"",
            ));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Some(error_response(
                request.get("id").filter(|id| is_valid_id(id)).cloned(),
                -32600,
                "request method must be a string",
            ));
        };
        let id = object.get("id").cloned();
        if id.is_none() {
            self.handle_notification(method);
            return None;
        }
        let id = id.unwrap_or(Value::Null);
        if !is_valid_id(&id) {
            return Some(error_response(
                None,
                -32600,
                "request id must be a string or number",
            ));
        }
        self.request_count = self.request_count.saturating_add(1);
        if self.request_count > MAX_REQUESTS_PER_SESSION {
            return Some(error_response(
                Some(id),
                -32000,
                "session request limit exceeded",
            ));
        }
        let empty_params = json!({});
        let params = object.get("params").unwrap_or(&empty_params);
        let response = match method {
            "initialize" => self.initialize(id.clone(), params),
            "ping" => json!({"jsonrpc":"2.0", "id":id, "result":{}}),
            "tools/list" if self.initialized => self.tools_list(id.clone(), params),
            "tools/call" if self.initialized => self.call_tool(id.clone(), params),
            "tools/list" | "tools/call" => {
                error_value(id.clone(), -32002, "server is not initialized")
            }
            _ => error_value(id.clone(), -32601, "method not found"),
        };
        let encoded = serde_json::to_vec(&response).unwrap_or_else(|_| {
            error_response(Some(id.clone()), -32603, "response serialization failed")
        });
        if encoded.len() > MAX_RESPONSE_BYTES {
            Some(error_response(
                Some(id),
                -32603,
                "response exceeds the 1 MiB output limit",
            ))
        } else {
            Some(encoded)
        }
    }

    fn initialize(&mut self, id: Value, params: &Value) -> Value {
        if self.initialize_requested {
            return error_value(
                id,
                -32600,
                "initialize may only be requested once per session",
            );
        }
        let Some(object) = params.as_object() else {
            return error_value(id, -32602, "initialize params must be an object");
        };
        if !object.get("protocolVersion").is_some_and(Value::is_string) {
            return error_value(id, -32602, "initialize requires a string protocolVersion");
        }
        let valid_client_info = object
            .get("clientInfo")
            .and_then(Value::as_object)
            .is_some_and(|info| {
                info.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| !name.is_empty())
                    && info
                        .get("version")
                        .and_then(Value::as_str)
                        .is_some_and(|version| !version.is_empty())
            });
        if !valid_client_info || !object.get("capabilities").is_some_and(Value::is_object) {
            return error_value(
                id,
                -32602,
                "initialize requires clientInfo and capabilities objects",
            );
        }
        self.initialize_requested = true;
        json!({
            "jsonrpc":"2.0", "id":id,
            "result":{
                "protocolVersion":PROTOCOL_VERSION,
                "capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"hycel-project-services","title":"Hycel Project Services","version":env!("CARGO_PKG_VERSION")},
                "instructions":"Pinned to one validated project root. No project code or network is executed. Scene/resource descriptor writes require process startup with --allow-writes and an explicit source hash."
            }
        })
    }

    fn handle_notification(&mut self, method: &str) {
        if method == "notifications/initialized" && self.initialize_requested {
            self.initialized = true;
        }
    }

    fn tools_list(&self, id: Value, params: &Value) -> Value {
        if params
            .as_object()
            .is_none_or(|object| object.keys().any(|key| key != "cursor" && key != "_meta"))
        {
            return error_value(id, -32602, "tools/list accepts only cursor and _meta");
        }
        if params.get("_meta").is_some_and(|meta| !meta.is_object()) {
            return error_value(id, -32602, "tools/list _meta must be an object");
        }
        if params
            .get("cursor")
            .is_some_and(|cursor| !cursor.is_string())
        {
            return error_value(
                id,
                -32602,
                "tool pagination is not needed for this bounded tool set",
            );
        }
        if params.get("cursor").is_some() {
            return error_value(
                id,
                -32602,
                "tool pagination is not needed for this bounded tool set",
            );
        }
        json!({"jsonrpc":"2.0", "id":id, "result":{"tools":self.tool_definitions()}})
    }

    fn tool_definitions(&self) -> Vec<Value> {
        let mut tools = vec![
            tool(
                "project_check",
                "Validate the pinned project and its authored documents.",
                &empty_schema(),
                true,
            ),
            tool(
                "project_inspect",
                "Read-only project summary, scene/resource index, and asset dependencies.",
                &json!({"type":"object","properties":{"offset":{"type":"integer","minimum":0,"maximum":100_000},"limit":{"type":"integer","minimum":1,"maximum":1000}},"additionalProperties":false}),
                true,
            ),
            tool(
                "project_tests",
                "Run one named headless reference-game scenario or the bounded default suite.",
                &json!({"type":"object","properties":{"scenario":{"type":"string","enum":["authored-content","fixed-tick-gameplay","echo-flight","hazard-respawn","platform-support","audio-decode"]}},"additionalProperties":false}),
                true,
            ),
            tool(
                "project_replay",
                "Replay a project-relative bounded replay JSON file; no project code is executed.",
                &json!({"type":"object","properties":{"replay_file":{"type":"string","maxLength":4096}},"required":["replay_file"],"additionalProperties":false}),
                true,
            ),
            tool(
                "scene_edit_preview",
                "Preview a strict typed scene rename/transform/entity create/delete/component upsert/removal and receive a bounded diff and source hash. Does not write.",
                &edit_tool_schema(false),
                true,
            ),
            tool(
                "resource_edit_preview",
                "Preview a typed resource-source change and receive a bounded diff and source hash. Does not write.",
                &resource_edit_tool_schema(false),
                true,
            ),
        ];
        if self.allow_writes {
            tools.push(tool("scene_edit_apply", "Apply exactly one previously previewed operation using its single-use preview token; preserves original data where present. Confirm with the user before calling.", &edit_tool_schema(true), false));
            tools.push(tool("resource_edit_apply", "Apply exactly one previously previewed resource-source change using its single-use token; creates a backup. Confirm with the user before calling.", &resource_edit_tool_schema(true), false));
            tools.push(tool("scene_screenshot", "Write a deterministic GPU-free SVG overview under the pinned project root; existing files are not overwritten.", &json!({"type":"object","properties":{"scene_id":{"type":"string","maxLength":64},"output_file":{"type":"string","maxLength":4096}},"required":["scene_id","output_file"],"additionalProperties":false}), false));
        }
        tools
    }

    fn call_tool(&mut self, id: Value, params: &Value) -> Value {
        let Some(object) = params.as_object() else {
            return error_value(id, -32602, "tools/call params must be an object");
        };
        if object
            .keys()
            .any(|key| key != "name" && key != "arguments" && key != "_meta")
        {
            return error_value(
                id,
                -32602,
                "tools/call accepts only name, arguments, and _meta",
            );
        }
        if object.get("_meta").is_some_and(|meta| !meta.is_object()) {
            return error_value(id, -32602, "tools/call _meta must be an object");
        }
        let Some(name) = object.get("name").and_then(Value::as_str) else {
            return error_value(id, -32602, "tool name is required");
        };
        let empty_arguments = json!({});
        let arguments = object.get("arguments").unwrap_or(&empty_arguments);
        let result = self.invoke_tool(name, arguments);
        let (text, structured, is_error) = match result {
            Ok((text, structured)) => (text, structured, false),
            Err(error) => {
                let structured = serde_json::from_str(&error).unwrap_or_else(|_| {
                    json!({
                        "schema_version":1,
                        "error":{
                            "code":"HYCEL-MCP-001",
                            "path":"tool.arguments",
                            "message":error,
                            "remediation":"check the advertised tool schema, the pinned project root, and write opt-in"
                        }
                    })
                });
                (error, structured, true)
            }
        };
        json!({
            "jsonrpc":"2.0", "id":id,
            "result":{"content":[{"type":"text","text":text}],"structuredContent":structured,"isError":is_error}
        })
    }

    #[allow(clippy::too_many_lines)] // Centralized tool routing keeps the capability boundary auditable.
    fn invoke_tool(&mut self, name: &str, arguments: &Value) -> Result<(String, Value), String> {
        let object = arguments
            .as_object()
            .ok_or_else(|| "tool arguments must be an object".to_owned())?;
        let root = self.project_root.to_string_lossy().into_owned();
        match name {
            "project_check" => {
                require_only_keys(object, &[])?;
                Self::cli_result(vec!["check".to_owned(), root, "--json".to_owned()])
            }
            "project_inspect" => {
                require_only_keys(object, &["offset", "limit"])?;
                let offset = optional_usize(object, "offset", 0, 100_000)?;
                let limit = optional_usize(object, "limit", 100, 1_000)?;
                if limit == 0 {
                    return Err("limit must be at least 1".to_owned());
                }
                Self::cli_result(vec![
                    "inspect".to_owned(),
                    root,
                    "--offset".to_owned(),
                    offset.to_string(),
                    "--limit".to_owned(),
                    limit.to_string(),
                    "--json".to_owned(),
                ])
            }
            "project_tests" => {
                require_only_keys(object, &["scenario"])?;
                let mut args = vec!["test".to_owned(), root];
                if let Some(scenario) = object.get("scenario") {
                    let scenario = scenario
                        .as_str()
                        .ok_or_else(|| "scenario must be a string".to_owned())?;
                    if ![
                        "authored-content",
                        "fixed-tick-gameplay",
                        "echo-flight",
                        "hazard-respawn",
                        "platform-support",
                        "audio-decode",
                    ]
                    .contains(&scenario)
                    {
                        return Err("unknown headless test scenario".to_owned());
                    }
                    args.extend(["--test".to_owned(), scenario.to_owned()]);
                }
                args.push("--json".to_owned());
                Self::cli_result(args)
            }
            "project_replay" => {
                require_only_keys(object, &["replay_file"])?;
                let replay_file = required_string_max(object, "replay_file", 4096)?;
                let path = resolve_existing_project_path(&self.project_root, replay_file)
                    .map_err(|diagnostic| diagnostic.to_string())?;
                if !path.is_file() {
                    return Err("replay_file must resolve to a regular project file".to_owned());
                }
                Self::cli_result(vec![
                    "replay".to_owned(),
                    root,
                    path.to_string_lossy().into_owned(),
                    "--json".to_owned(),
                ])
            }
            "scene_edit_preview" | "scene_edit_apply" => {
                let applying = name == "scene_edit_apply";
                if applying && !self.allow_writes {
                    return Err(
                        "scene_edit_apply is disabled; restart with --allow-writes".to_owned()
                    );
                }
                let keys: &[&str] = if applying {
                    &["scene_file", "operation", "preview_token"]
                } else {
                    &["scene_file", "operation"]
                };
                require_only_keys(object, keys)?;
                let scene_file = required_string_max(object, "scene_file", 4096)?;
                let operation = serde_json::from_value::<SceneEditOperation>(
                    object
                        .get("operation")
                        .cloned()
                        .ok_or_else(|| "operation is required".to_owned())?,
                )
                .map_err(|error| format!("invalid strict scene operation: {error}"))?;
                validate_edit_operation_bounds(&operation)?;
                let response = if applying {
                    let token = required_string_max(object, "preview_token", 64)?;
                    if token.chars().count() < 10 {
                        return Err("preview_token must contain at least 10 characters".to_owned());
                    }
                    let pending = self.pending_previews.remove(token).ok_or_else(|| {
                        "preview_token is missing, expired, or already used".to_owned()
                    })?;
                    if pending.relative_path != scene_file
                        || pending.operation != PendingOperation::Scene(operation.clone())
                    {
                        return Err("preview_token is bound to a different scene or operation; preview the requested edit again".to_owned());
                    }
                    let current =
                        hycel_cli::preview_scene_edit(&self.project_root, scene_file, &operation)?;
                    if current.original_sha256 != pending.original_sha256
                        || current.candidate_sha256 != pending.candidate_sha256
                    {
                        return Err("source or candidate changed after preview; preview the requested edit again".to_owned());
                    }
                    let receipt = hycel_cli::apply_scene_edit(
                        &self.project_root,
                        scene_file,
                        &operation,
                        &pending.original_sha256,
                    )?;
                    serde_json::to_value(receipt).map_err(|error| error.to_string())?
                } else {
                    let preview =
                        hycel_cli::preview_scene_edit(&self.project_root, scene_file, &operation)?;
                    let next_token = self
                        .next_preview_token
                        .checked_add(1)
                        .ok_or_else(|| "preview token capacity exhausted".to_owned())?;
                    let token = format!("preview-{:016x}", self.next_preview_token);
                    self.next_preview_token = next_token;
                    if self.pending_previews.len() >= MAX_PENDING_PREVIEWS {
                        if let Some(oldest) = self.pending_previews.keys().next().cloned() {
                            self.pending_previews.remove(&oldest);
                        }
                    }
                    self.pending_previews.insert(
                        token.clone(),
                        PendingPreview {
                            relative_path: scene_file.to_owned(),
                            operation: PendingOperation::Scene(operation.clone()),
                            original_sha256: preview.original_sha256.clone(),
                            candidate_sha256: preview.candidate_sha256.clone(),
                        },
                    );
                    let mut value =
                        serde_json::to_value(preview).map_err(|error| error.to_string())?;
                    value["preview_token"] = Value::String(token);
                    value
                };
                let text =
                    serde_json::to_string_pretty(&response).map_err(|error| error.to_string())?;
                Ok((text, response))
            }
            "resource_edit_preview" | "resource_edit_apply" => {
                let applying = name == "resource_edit_apply";
                if applying && !self.allow_writes {
                    return Err(
                        "resource_edit_apply is disabled; restart with --allow-writes".to_owned(),
                    );
                }
                let keys: &[&str] = if applying {
                    &["resource_file", "operation", "preview_token"]
                } else {
                    &["resource_file", "operation"]
                };
                require_only_keys(object, keys)?;
                let resource_file = required_string_max(object, "resource_file", 4096)?;
                let operation = serde_json::from_value::<ResourceEditOperation>(
                    object
                        .get("operation")
                        .cloned()
                        .ok_or_else(|| "operation is required".to_owned())?,
                )
                .map_err(|error| format!("invalid strict resource operation: {error}"))?;
                validate_resource_operation_bounds(&operation)?;
                let response = if applying {
                    let token = required_string_max(object, "preview_token", 64)?;
                    if token.chars().count() < 10 {
                        return Err("preview_token must contain at least 10 characters".to_owned());
                    }
                    let pending = self.pending_previews.remove(token).ok_or_else(|| {
                        "preview_token is missing, expired, or already used".to_owned()
                    })?;
                    if pending.relative_path != resource_file
                        || pending.operation != PendingOperation::Resource(operation.clone())
                    {
                        return Err("preview_token is bound to a different resource or operation; preview the requested edit again".to_owned());
                    }
                    let current = hycel_cli::preview_resource_edit(
                        &self.project_root,
                        resource_file,
                        &operation,
                    )?;
                    if current.original_sha256 != pending.original_sha256
                        || current.candidate_sha256 != pending.candidate_sha256
                    {
                        return Err("source or candidate changed after preview; preview the requested resource edit again".to_owned());
                    }
                    let receipt = hycel_cli::apply_resource_edit(
                        &self.project_root,
                        resource_file,
                        &operation,
                        &pending.original_sha256,
                    )?;
                    serde_json::to_value(receipt).map_err(|error| error.to_string())?
                } else {
                    let preview = hycel_cli::preview_resource_edit(
                        &self.project_root,
                        resource_file,
                        &operation,
                    )?;
                    let next_token = self
                        .next_preview_token
                        .checked_add(1)
                        .ok_or_else(|| "preview token capacity exhausted".to_owned())?;
                    let token = format!("preview-{:016x}", self.next_preview_token);
                    self.next_preview_token = next_token;
                    if self.pending_previews.len() >= MAX_PENDING_PREVIEWS {
                        if let Some(oldest) = self.pending_previews.keys().next().cloned() {
                            self.pending_previews.remove(&oldest);
                        }
                    }
                    self.pending_previews.insert(
                        token.clone(),
                        PendingPreview {
                            relative_path: resource_file.to_owned(),
                            operation: PendingOperation::Resource(operation),
                            original_sha256: preview.original_sha256.clone(),
                            candidate_sha256: preview.candidate_sha256.clone(),
                        },
                    );
                    let mut value =
                        serde_json::to_value(preview).map_err(|error| error.to_string())?;
                    value["preview_token"] = Value::String(token);
                    value
                };
                let text =
                    serde_json::to_string_pretty(&response).map_err(|error| error.to_string())?;
                Ok((text, response))
            }
            "scene_screenshot" => {
                if !self.allow_writes {
                    return Err(
                        "scene_screenshot is disabled; restart with --allow-writes".to_owned()
                    );
                }
                require_only_keys(object, &["scene_id", "output_file"])?;
                let scene_id = required_string_max(object, "scene_id", 64)?;
                let output_file = required_string_max(object, "output_file", 4096)?;
                validate_relative_project_path(output_file)
                    .map_err(|diagnostic| diagnostic.to_string())?;
                if !Path::new(output_file)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
                {
                    return Err("output_file must have an .svg extension".to_owned());
                }
                let output_path = self.confined_output_path(output_file)?;
                Self::cli_result(vec![
                    "screenshot".to_owned(),
                    root,
                    "--scene".to_owned(),
                    scene_id.to_owned(),
                    "--output".to_owned(),
                    output_path.to_string_lossy().into_owned(),
                    "--json".to_owned(),
                ])
            }
            _ => Err(format!("unknown or disabled tool: {name}")),
        }
    }

    fn confined_output_path(&self, relative_path: &str) -> Result<PathBuf, String> {
        let relative = Path::new(relative_path);
        let file_name = relative
            .file_name()
            .ok_or_else(|| "output_file must name a file".to_owned())?;
        let parent_relative = relative.parent().unwrap_or_else(|| Path::new("."));
        let parent = if parent_relative == Path::new(".") {
            self.project_root.clone()
        } else {
            resolve_existing_project_path(&self.project_root, &parent_relative.to_string_lossy())
                .map_err(|diagnostic| diagnostic.to_string())?
        };
        if !parent.is_dir() || !parent.starts_with(&self.project_root) {
            return Err(
                "output parent must be an existing directory inside the pinned project root"
                    .to_owned(),
            );
        }
        let output = parent.join(file_name);
        if fs::symlink_metadata(&output).is_ok() {
            return Err("screenshot output already exists; MCP never overwrites files".to_owned());
        }
        Ok(output)
    }

    fn cli_result(arguments: Vec<String>) -> Result<(String, Value), String> {
        let output: CliOutput = execute(arguments.into_iter().map(OsString::from));
        let structured = serde_json::from_str(&output.stdout)
            .unwrap_or_else(|_| json!({"stdout":output.stdout,"stderr":output.stderr}));
        if output.exit_code == 0 {
            Ok((output.stdout, structured))
        } else {
            Err(output.stdout.if_empty_then(&output.stderr))
        }
    }
}

trait EmptyText {
    fn if_empty_then(self, fallback: &str) -> String;
}

impl EmptyText for String {
    fn if_empty_then(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_owned()
        } else {
            self
        }
    }
}

fn is_valid_id(value: &Value) -> bool {
    value.is_string()
        || value.as_i64().is_some()
        || value.as_u64().is_some()
        || value.as_f64().is_some()
}

fn error_response(id: Option<Value>, code: i32, message: &str) -> Vec<u8> {
    let response = json!({
        "jsonrpc":"2.0",
        "id":id.unwrap_or(Value::Null),
        "error":{"code":code,"message":message}
    });
    serde_json::to_vec(&response).unwrap_or_else(|_| {
        b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"internal error\"}}".to_vec()
    })
}

fn error_value(id: Value, code: i32, message: &str) -> Value {
    let mut response = json!({"jsonrpc":"2.0","id":null,"error":{"code":code,"message":message}});
    response["id"] = id;
    response
}

fn edit_tool_schema(applying: bool) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "scene_file".to_owned(),
        json!({"type":"string","maxLength":4096}),
    );
    properties.insert("operation".to_owned(), json!({
        "oneOf":[
            {"type":"object","properties":{"operation":{"const":"restore_scene_backup"},"backup_sha256":{"type":"string","minLength":64,"maxLength":64,"pattern":"^[0-9a-f]{64}$"}},"required":["operation","backup_sha256"],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"create_scene"},"scene_id":{"type":"string","maxLength":64},"name":{"type":"string","maxLength":128}},"required":["operation","scene_id","name"],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"rename_scene"},"name":{"type":"string","maxLength":128}},"required":["operation","name"],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"rename_entity"},"entity_id":{"type":"string","maxLength":64},"name":{"type":"string","maxLength":128}},"required":["operation","entity_id","name"],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"set_entity_transform"},"entity_id":{"type":"string","maxLength":64},"translation_milli":{"type":"array","items":{"type":"integer"},"minItems":2,"maxItems":2},"rotation_units":{"type":"integer","minimum":0,"maximum":65535},"scale_milli":{"type":"array","items":{"type":"integer"},"minItems":2,"maxItems":2}},"required":["operation","entity_id"],"anyOf":[{"required":["translation_milli"]},{"required":["rotation_units"]},{"required":["scale_milli"]}],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"create_entity"},"entity_id":{"type":"string","maxLength":64},"name":{"type":"string","maxLength":128},"translation_milli":{"type":"array","items":{"type":"integer"},"minItems":2,"maxItems":2}},"required":["operation","entity_id","name"],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"delete_entity"},"entity_id":{"type":"string","maxLength":64}},"required":["operation","entity_id"],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"remove_entity_component"},"entity_id":{"type":"string","maxLength":64},"component_type":{"type":"string","maxLength":128}},"required":["operation","entity_id","component_type"],"additionalProperties":false},
            {"type":"object","properties":{"operation":{"const":"set_entity_component"},"entity_id":{"type":"string","maxLength":64},"component_type":{"type":"string","maxLength":128},"schema_version":{"type":"integer","minimum":1},"data":{"type":"object"}},"required":["operation","entity_id","component_type","schema_version","data"],"additionalProperties":false}
        ]
    }));
    let mut required = vec!["scene_file", "operation"];
    if applying {
        properties.insert(
            "preview_token".to_owned(),
            json!({"type":"string","minLength":10,"maxLength":64}),
        );
        required.push("preview_token");
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn resource_edit_tool_schema(applying: bool) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "resource_file".to_owned(),
        json!({"type":"string","maxLength":4096}),
    );
    properties.insert(
        "operation".to_owned(),
        json!({
            "oneOf":[
                {"type":"object","properties":{"operation":{"const":"set_source"},"source":{"type":"string","maxLength":4096}},"required":["operation","source"],"additionalProperties":false},
                {"type":"object","properties":{"operation":{"const":"restore_resource_backup"},"backup_sha256":{"type":"string","minLength":64,"maxLength":64,"pattern":"^[0-9a-f]{64}$"}},"required":["operation","backup_sha256"],"additionalProperties":false}
            ]
        }),
    );
    let mut required = vec!["resource_file", "operation"];
    if applying {
        properties.insert(
            "preview_token".to_owned(),
            json!({"type":"string","minLength":10,"maxLength":64}),
        );
        required.push("preview_token");
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

fn empty_schema() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}

fn tool(name: &str, description: &str, input_schema: &Value, read_only: bool) -> Value {
    json!({
        "name":name,
        "description":description,
        "inputSchema":input_schema,
        "annotations":{"readOnlyHint":read_only,"destructiveHint":!read_only,"openWorldHint":false}
    })
}

fn require_only_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unknown tool argument: {key}"));
    }
    Ok(())
}

fn optional_usize(
    object: &serde_json::Map<String, Value>,
    key: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, String> {
    let Some(value) = object.get(key) else {
        return Ok(default);
    };
    let number = value
        .as_u64()
        .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
    let number = usize::try_from(number).map_err(|_| format!("{key} is out of range"))?;
    if number > maximum {
        return Err(format!("{key} must not exceed {maximum}"));
    }
    Ok(number)
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn required_string_max<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    maximum_characters: usize,
) -> Result<&'a str, String> {
    let value = required_string(object, key)?;
    if value.chars().count() > maximum_characters {
        return Err(format!(
            "{key} must not exceed {maximum_characters} characters"
        ));
    }
    Ok(value)
}

fn validate_resource_operation_bounds(operation: &ResourceEditOperation) -> Result<(), String> {
    match operation {
        ResourceEditOperation::SetSource { source } if source.chars().count() > 4096 => {
            Err("source must not exceed 4096 characters".to_owned())
        }
        ResourceEditOperation::RestoreResourceBackup { backup_sha256 }
            if backup_sha256.len() != 64
                || !backup_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            Err("backup_sha256 must be 64 lowercase hexadecimal characters".to_owned())
        }
        ResourceEditOperation::SetSource { .. }
        | ResourceEditOperation::RestoreResourceBackup { .. } => Ok(()),
    }
}

fn validate_edit_operation_bounds(operation: &SceneEditOperation) -> Result<(), String> {
    let check = |field: &str, value: &str, maximum: usize| {
        if value.chars().count() > maximum {
            Err(format!("{field} must not exceed {maximum} characters"))
        } else {
            Ok(())
        }
    };
    match operation {
        SceneEditOperation::RestoreSceneBackup { backup_sha256 } => {
            check("backup_sha256", backup_sha256, 64)
        }
        SceneEditOperation::CreateScene { scene_id, name } => {
            check("scene_id", scene_id, 64)?;
            check("name", name, 128)
        }
        SceneEditOperation::RenameScene { name } => check("name", name, 128),
        SceneEditOperation::RenameEntity { entity_id, name }
        | SceneEditOperation::CreateEntity {
            entity_id, name, ..
        } => {
            check("entity_id", entity_id, 64)?;
            check("name", name, 128)
        }
        SceneEditOperation::SetEntityTransform { entity_id, .. }
        | SceneEditOperation::DeleteEntity { entity_id } => check("entity_id", entity_id, 64),
        SceneEditOperation::SetEntityTags { entity_id, tags } => {
            check("entity_id", entity_id, 64)?;
            if tags.len() > 64 {
                return Err("tags must contain at most 64 entries".to_owned());
            }
            for tag in tags {
                check("tag", tag, 64)?;
            }
            Ok(())
        }
        SceneEditOperation::RemoveEntityComponent {
            entity_id,
            component_type,
        }
        | SceneEditOperation::SetEntityComponent {
            entity_id,
            component_type,
            ..
        } => {
            check("entity_id", entity_id, 64)?;
            check("component_type", component_type, 128)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};

    use super::{McpServer, PROTOCOL_VERSION};

    #[test]
    fn protocol_handshake_lists_shared_services_and_rejects_preinit_calls() {
        let project = create_project();
        let mut server = McpServer::new(&project, false).unwrap();
        let before = server
            .handle_line(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&before).unwrap()["error"]["code"],
            -32002
        );
        let initialize = json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}});
        let response = server
            .handle_line(&serde_json::to_vec(&initialize).unwrap())
            .unwrap();
        let response: Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            response["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let listed = server
            .handle_line(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{}}}"#)
            .unwrap();
        let listed: Value = serde_json::from_slice(&listed).unwrap();
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == "scene_edit_preview")
        );
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == "resource_edit_preview")
        );
        assert!(!tools.iter().any(|tool| tool["name"] == "scene_edit_apply"));
        assert!(
            !tools
                .iter()
                .any(|tool| tool["name"] == "resource_edit_apply")
        );
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn scene_creation_uses_single_use_preview_tokens_and_never_overwrites() {
        let project = create_project();
        let target = project.join("scenes/second-room.json");
        let mut server = McpServer::new(&project, true).unwrap();
        initialize(&mut server);
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let preview = json!({
            "jsonrpc":"2.0", "id":10, "method":"tools/call",
            "params":{"name":"scene_edit_preview","arguments":{
                "scene_file":"scenes/second-room.json",
                "operation":{"operation":"create_scene","scene_id":"11000000-0000-4000-8000-000000000009","name":"Second Room"}
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let result = &response["result"]["structuredContent"];
        assert_eq!(result["backup_path"], Value::Null);
        assert!(!target.exists());
        let token = result["preview_token"].as_str().unwrap();
        let apply = json!({
            "jsonrpc":"2.0", "id":11, "method":"tools/call",
            "params":{"name":"scene_edit_apply","arguments":{
                "scene_file":"scenes/second-room.json",
                "operation":{"operation":"create_scene","scene_id":"11000000-0000-4000-8000-000000000009","name":"Second Room"},
                "preview_token":token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["backup_path"],
            Value::Null
        );
        assert!(fs::read_to_string(&target).unwrap().contains("Second Room"));
        let reused = server
            .handle_line(&serde_json::to_vec(&apply).unwrap())
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&reused).unwrap()["result"]["isError"],
            true
        );
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn entity_tag_edits_use_single_use_preview_tokens_and_backups() {
        let project = create_project();
        let scene_path = project.join("scenes/first-room.json");
        let mut scene: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
        scene["entities"] = json!([{
            "id":"20000000-0000-4000-8000-000000000001",
            "name":"Courier", "tags":["player"], "components":[]
        }]);
        fs::write(&scene_path, serde_json::to_vec_pretty(&scene).unwrap()).unwrap();
        let original = fs::read(&scene_path).unwrap();
        let mut server = McpServer::new(&project, true).unwrap();
        initialize(&mut server);
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let operation = json!({
            "operation":"set_entity_tags",
            "entity_id":"20000000-0000-4000-8000-000000000001",
            "tags":["player", "courier"]
        });
        let preview = json!({
            "jsonrpc":"2.0", "id":31, "method":"tools/call",
            "params":{"name":"scene_edit_preview","arguments":{
                "scene_file":"scenes/first-room.json", "operation":operation
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false, "{response}");
        let preview_token = response["result"]["structuredContent"]["preview_token"]
            .as_str()
            .unwrap();
        assert_eq!(fs::read(&scene_path).unwrap(), original);
        let apply = json!({
            "jsonrpc":"2.0", "id":32, "method":"tools/call",
            "params":{"name":"scene_edit_apply","arguments":{
                "scene_file":"scenes/first-room.json", "operation":operation,
                "preview_token":preview_token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let backup = response["result"]["structuredContent"]["backup_path"]
            .as_str()
            .unwrap();
        assert_eq!(fs::read(project.join(backup)).unwrap(), original);
        let scene: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
        assert_eq!(scene["entities"][0]["tags"], json!(["player", "courier"]));
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn enabled_scene_edits_use_preview_hash_and_preserve_a_backup() {
        let project = create_project();
        let scene_path = project.join("scenes/first-room.json");
        let original = fs::read(&scene_path).unwrap();
        let mut server = McpServer::new(&project, true).unwrap();
        initialize(&mut server);
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let preview = json!({
            "jsonrpc":"2.0", "id":10, "method":"tools/call",
            "params":{"name":"scene_edit_preview","arguments":{
                "scene_file":"scenes/first-room.json",
                "operation":{"operation":"rename_scene","name":"MCP Observatory"}
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let preview_result = &response["result"]["structuredContent"];
        let preview_token = preview_result["preview_token"].as_str().unwrap();
        assert_eq!(fs::read(&scene_path).unwrap(), original);

        let substituted = json!({
            "jsonrpc":"2.0", "id":11, "method":"tools/call",
            "params":{"name":"scene_edit_apply","arguments":{
                "scene_file":"scenes/first-room.json",
                "operation":{"operation":"rename_scene","name":"Not Previewed"},
                "preview_token":preview_token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&substituted).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);

        let preview = json!({
            "jsonrpc":"2.0", "id":12, "method":"tools/call",
            "params":{"name":"scene_edit_preview","arguments":{
                "scene_file":"scenes/first-room.json",
                "operation":{"operation":"rename_scene","name":"MCP Observatory"}
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        let preview_token = response["result"]["structuredContent"]["preview_token"]
            .as_str()
            .unwrap();

        let apply = json!({
            "jsonrpc":"2.0", "id":13, "method":"tools/call",
            "params":{"name":"scene_edit_apply","arguments":{
                "scene_file":"scenes/first-room.json",
                "operation":{"operation":"rename_scene","name":"MCP Observatory"},
                "preview_token":preview_token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let mut repeated_apply = apply.clone();
        repeated_apply["id"] = json!(14);
        let repeated_response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&repeated_apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(repeated_response["result"]["isError"], true);
        let backup_relative = response["result"]["structuredContent"]["backup_path"]
            .as_str()
            .unwrap();
        assert_eq!(fs::read(project.join(backup_relative)).unwrap(), original);
        assert!(
            fs::read_to_string(&scene_path)
                .unwrap()
                .contains("MCP Observatory")
        );
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Keeps preview, apply, restore and token-reuse assertions together.
    fn resource_source_edits_require_bound_preview_tokens_and_keep_backups() {
        let project = create_project();
        fs::write(project.join("assets/old.rgba"), b"old pixels").unwrap();
        fs::write(project.join("assets/new.rgba"), b"new pixels").unwrap();
        let descriptor_path = project.join("assets/texture.hycel.json");
        let original = br#"{"schema_version":1,"id":"30000000-0000-4000-8000-000000000001","kind":"texture","source":"assets/old.rgba","import":{}}"#;
        fs::write(&descriptor_path, original).unwrap();

        let mut server = McpServer::new(&project, true).unwrap();
        initialize(&mut server);
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let operation = json!({"operation":"set_source","source":"assets/new.rgba"});
        let preview = json!({
            "jsonrpc":"2.0", "id":38, "method":"tools/call",
            "params":{"name":"resource_edit_preview","arguments":{
                "resource_file":"assets/texture.hycel.json","operation":operation
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(fs::read(&descriptor_path).unwrap(), original);
        let token = response["result"]["structuredContent"]["preview_token"]
            .as_str()
            .unwrap();
        let apply = json!({
            "jsonrpc":"2.0", "id":39, "method":"tools/call",
            "params":{"name":"resource_edit_apply","arguments":{
                "resource_file":"assets/texture.hycel.json","operation":operation,
                "preview_token":token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let backup = response["result"]["structuredContent"]["backup_path"]
            .as_str()
            .unwrap();
        assert_eq!(fs::read(project.join(backup)).unwrap(), original);
        assert!(
            fs::read_to_string(&descriptor_path)
                .unwrap()
                .contains("assets/new.rgba")
        );
        let edited_bytes = fs::read(&descriptor_path).unwrap();
        let restore_operation = json!({
            "operation":"restore_resource_backup",
            "backup_sha256":response["result"]["structuredContent"]["original_sha256"]
        });
        let restore_preview = json!({
            "jsonrpc":"2.0", "id":40, "method":"tools/call",
            "params":{"name":"resource_edit_preview","arguments":{
                "resource_file":"assets/texture.hycel.json","operation":restore_operation
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&restore_preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let restore_token = response["result"]["structuredContent"]["preview_token"]
            .as_str()
            .unwrap();
        let restore_apply = json!({
            "jsonrpc":"2.0", "id":41, "method":"tools/call",
            "params":{"name":"resource_edit_apply","arguments":{
                "resource_file":"assets/texture.hycel.json","operation":restore_operation,
                "preview_token":restore_token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&restore_apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let restore_backup = response["result"]["structuredContent"]["backup_path"]
            .as_str()
            .unwrap();
        assert_eq!(
            fs::read(project.join(restore_backup)).unwrap(),
            edited_bytes
        );
        assert!(
            fs::read_to_string(&descriptor_path)
                .unwrap()
                .contains("assets/old.rgba")
        );

        let repeated = server
            .handle_line(&serde_json::to_vec(&apply).unwrap())
            .unwrap();
        let repeated: Value = serde_json::from_slice(&repeated).unwrap();
        assert_eq!(repeated["result"]["isError"], true);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn advertised_echo_flight_scenario_runs_through_mcp() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/bellglass-courier");
        let mut server = McpServer::new(&project, false).unwrap();
        initialize(&mut server);
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

        let call = json!({
            "jsonrpc":"2.0", "id":40, "method":"tools/call",
            "params":{"name":"project_tests","arguments":{"scenario":"echo-flight"}}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&call).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["result"]["tests"][0]["name"],
            "echo-flight"
        );
        assert_eq!(
            response["result"]["structuredContent"]["result"]["tests"][0]["passed"],
            true
        );
    }

    #[test]
    fn mcp_preview_token_can_restore_an_exact_scene_backup() {
        let project = create_project();
        let scene_path = project.join("scenes/first-room.json");
        let original = fs::read(&scene_path).unwrap();
        let mut server = McpServer::new(&project, true).unwrap();
        initialize(&mut server);
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let rename = json!({"operation":"rename_scene","name":"Later Version"});
        let preview = json!({
            "jsonrpc":"2.0", "id":30, "method":"tools/call",
            "params":{"name":"scene_edit_preview","arguments":{
                "scene_file":"scenes/first-room.json","operation":rename
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        let preview_token = response["result"]["structuredContent"]["preview_token"]
            .as_str()
            .unwrap();
        let apply = json!({
            "jsonrpc":"2.0", "id":31, "method":"tools/call",
            "params":{"name":"scene_edit_apply","arguments":{
                "scene_file":"scenes/first-room.json","operation":rename,
                "preview_token":preview_token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let backup_sha256 = response["result"]["structuredContent"]["original_sha256"].clone();
        let edited = fs::read(&scene_path).unwrap();
        let restore = json!({"operation":"restore_scene_backup","backup_sha256":backup_sha256});
        let preview = json!({
            "jsonrpc":"2.0", "id":32, "method":"tools/call",
            "params":{"name":"scene_edit_preview","arguments":{
                "scene_file":"scenes/first-room.json","operation":restore
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&preview).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let preview_token = response["result"]["structuredContent"]["preview_token"]
            .as_str()
            .unwrap();
        let apply = json!({
            "jsonrpc":"2.0", "id":33, "method":"tools/call",
            "params":{"name":"scene_edit_apply","arguments":{
                "scene_file":"scenes/first-room.json","operation":restore,
                "preview_token":preview_token
            }}
        });
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&apply).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(fs::read(&scene_path).unwrap(), original);
        let restore_backup = response["result"]["structuredContent"]["backup_path"]
            .as_str()
            .unwrap();
        assert_eq!(fs::read(project.join(restore_backup)).unwrap(), edited);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn tool_calls_are_pinned_bounded_and_mutations_require_optin() {
        let project = create_project();
        let mut read_only = McpServer::new(&project, false).unwrap();
        initialize(&mut read_only);
        let _ = read_only.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let call = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"project_check","arguments":{}}});
        let response: Value = serde_json::from_slice(
            &read_only
                .handle_line(&serde_json::to_vec(&call).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let denied = json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"scene_edit_apply","arguments":{}}});
        let response: Value = serde_json::from_slice(
            &read_only
                .handle_line(&serde_json::to_vec(&denied).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["error"]["code"],
            "HYCEL-MCP-001"
        );
        let denied_resource = json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"resource_edit_apply","arguments":{}}});
        let response: Value = serde_json::from_slice(
            &read_only
                .handle_line(&serde_json::to_vec(&denied_resource).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
        let outside = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"project_replay","arguments":{"replay_file":"../outside.json"}}});
        let response: Value = serde_json::from_slice(
            &read_only
                .handle_line(&serde_json::to_vec(&outside).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn optional_mcp_fields_negotiation_bounds_and_error_ids_follow_wire_contract() {
        let project = create_project();
        let mut server = McpServer::new(&project, false).unwrap();
        let malformed = json!({"jsonrpc":"2.0","id":20,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{}}});
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&malformed).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32602);
        let initialize = json!({"jsonrpc":"2.0","id":21,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}});
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&initialize).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        let _ = server.handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let listed: Value = serde_json::from_slice(
            &server
                .handle_line(br#"{"jsonrpc":"2.0","id":22,"method":"tools/list"}"#)
                .unwrap(),
        )
        .unwrap();
        assert!(listed["result"]["tools"].is_array());
        let check = json!({"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"project_check"}});
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&check).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        let too_long = json!({"jsonrpc":"2.0","id":25,"method":"tools/call","params":{"name":"scene_edit_preview","arguments":{"scene_file":"scenes/first-room.json","operation":{"operation":"rename_scene","name":"x".repeat(129)}}}});
        let response: Value = serde_json::from_slice(
            &server
                .handle_line(&serde_json::to_vec(&too_long).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);

        let malformed_json = server.handle_line(b"{").unwrap();
        let malformed_json: Value = serde_json::from_slice(&malformed_json).unwrap();
        assert_eq!(malformed_json["id"], Value::Null);

        let null_id = server
            .handle_line(br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#)
            .unwrap();
        let null_id: Value = serde_json::from_slice(&null_id).unwrap();
        assert_eq!(null_id["id"], Value::Null);
        assert_eq!(null_id["error"]["code"], -32600);

        let mut exact = br#"{"jsonrpc":"2.0","id":24,"method":"ping"}"#.to_vec();
        exact.resize(super::MAX_REQUEST_BYTES, b' ');
        exact.extend_from_slice(b"\r\n");
        let response: Value = serde_json::from_slice(&server.handle_line(&exact).unwrap()).unwrap();
        assert_eq!(response["result"], json!({}));
        exact.insert(super::MAX_REQUEST_BYTES, b' ');
        let response: Value = serde_json::from_slice(&server.handle_line(&exact).unwrap()).unwrap();
        assert_eq!(response["error"]["code"], -32700);
        assert_eq!(response["id"], Value::Null);
        fs::remove_dir_all(project).unwrap();
    }

    fn initialize(server: &mut McpServer) {
        let value = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}});
        let _ = server.handle_line(&serde_json::to_vec(&value).unwrap());
    }

    fn create_project() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hycel-mcp-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let output = hycel_cli::execute(["new", path.to_str().unwrap(), "--json"].map(Into::into));
        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        path
    }
}
