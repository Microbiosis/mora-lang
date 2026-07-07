//! Mora MCP Server (v0.04: 内嵌到解释器中)
//!
//! 设计: JSON-RPC 2.0 over stdin/stdout (MCP 协议)
//! - 客户端 (如 Claude Desktop) 通过 stdin 发 JSON-RPC 请求
//! - Server 通过 stdout 发 JSON-RPC 响应
//! - 协议层复用 LSP transport (Content-Length framing)
//!
//! 协议流程:
//! 1. initialize → capabilities: {tools: {listChanged: false}}
//! 2. initialized (notification, no response)
//! 3. tools/list → 列出脚本里 `tool` 块
//! 4. tools/call → 调 tool handler

use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

use crate::interpreter::{Interpreter, Value};
use crate::lsp::json::{Value as JsonValue, to_string as json_to_string};

/// MCP tool entry: name + description + parameters JSON schema + handler closure
#[derive(Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub parameters: String, // JSON Schema 字符串
    pub handler: Value,     // Mora 闭包
    pub toolset: String,    // v0.24: 所属 toolset
}

pub type ToolRegistry = Arc<tokio::sync::RwLock<HashMap<String, McpTool>>>;

/// v0.24: Toolset 配置
#[derive(Clone, Debug)]
pub struct ToolsetConfig {
    /// 启用的 toolset 列表
    pub enabled: Vec<String>,
    /// 启用的单个工具列表
    pub tools: Vec<String>,
    /// 是否只读模式
    pub read_only: bool,
    /// 是否 insiders 模式
    pub insiders: bool,
}

impl Default for ToolsetConfig {
    fn default() -> Self {
        Self {
            enabled: vec!["default".to_string()],
            tools: Vec::new(),
            read_only: false,
            insiders: false,
        }
    }
}

impl ToolsetConfig {
    /// 检查工具是否应该启用
    pub fn is_tool_enabled(&self, tool: &McpTool) -> bool {
        // 如果指定了具体工具，只启用这些
        if !self.tools.is_empty() {
            return self.tools.contains(&tool.name);
        }

        // 如果启用了 "all"，启用所有工具
        if self.enabled.contains(&"all".to_string()) {
            return true;
        }

        // 检查工具所属的 toolset 是否启用
        self.enabled.contains(&tool.toolset) || self.enabled.contains(&"default".to_string())
    }
}

/// v0.24: 内置 toolset 定义
pub fn builtin_toolsets() -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    map.insert(
        "ai".to_string(),
        vec![
            "ai.chat".to_string(),
            "ai.stream".to_string(),
            "ai.create".to_string(),
            "ai.critic".to_string(),
        ],
    );
    map.insert(
        "json".to_string(),
        vec!["json.parse".to_string(), "json.stringify".to_string()],
    );
    map.insert(
        "file".to_string(),
        vec![
            "file.read_text".to_string(),
            "file.write_text".to_string(),
            "file.exists".to_string(),
            "file.list".to_string(),
            "file.mkdir".to_string(),
            "file.remove".to_string(),
        ],
    );
    map.insert("web".to_string(), vec!["web.fetch".to_string()]);
    map.insert(
        "default".to_string(),
        vec![
            "ai.chat".to_string(),
            "json.parse".to_string(),
            "json.stringify".to_string(),
            "file.read_text".to_string(),
            "web.fetch".to_string(),
        ],
    );
    map
}

/// 启动 MCP server (async tokio, 读 stdin 写 stdout)
/// v0.50.0: 完全 async 化 — tokio::sync::RwLock + spawn_blocking 包装同步 handler
pub async fn start(
    tool_registry: ToolRegistry,
    interpreter: Arc<tokio::sync::RwLock<Interpreter>>,
    _stdio: Option<tokio::io::Stdin>,
) -> io::Result<()> {
    let config = ToolsetConfig::default();

    eprintln!("[mcp] Mora MCP server starting on stdio");
    {
        let tools = tool_registry.read().await;
        let enabled_count = tools.values().filter(|t| config.is_tool_enabled(t)).count();
        eprintln!(
            "[mcp] Registered {} tool(s) ({} enabled):",
            tools.len(),
            enabled_count
        );
        for (name, tool) in tools.iter() {
            let status = if config.is_tool_enabled(tool) {
                "✓"
            } else {
                "✗"
            };
            eprintln!("[mcp]   {} {}", status, name);
        }
    }
    if config.read_only {
        eprintln!("[mcp] Mode: read-only");
    }
    if config.insiders {
        eprintln!("[mcp] Mode: insiders");
    }
    eprintln!();

    let stdin = _stdio.unwrap_or_else(tokio::io::stdin);
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = stdout;

    loop {
        // 读一条 JSON-RPC message (Content-Length framing)
        let body = match read_message(&mut reader).await {
            Ok(Some(b)) => b,
            Ok(None) => {
                eprintln!("[mcp] stdin closed, exiting");
                break;
            }
            Err(e) => {
                eprintln!("[mcp] read error: {}, exiting", e);
                break;
            }
        };

        // parse JSON-RPC
        let req: JsonValue = match crate::lsp::json::parse(&body) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[mcp] JSON parse error: {}", e);
                continue;
            }
        };

        let is_notification = req.get("id").is_none();

        if is_notification {
            // notification 直接异步处理，不等待响应
            let tools = tool_registry.clone();
            let interp = interpreter.clone();
            tokio::spawn(async move {
                let _ = dispatch(&req, &tools, &interp).await;
            });
        } else {
            // 请求同步处理并回写响应
            let response = dispatch(&req, &tool_registry, &interpreter).await;
            if let Some(resp) = response {
                let resp_str = json_to_string(&resp);
                write_message(&mut writer, &resp_str).await?;
            }
        }
    }
    Ok(())
}

/// 读一条 JSON-RPC message (Content-Length framing)
/// 返回 None 表示 EOF
async fn read_message<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();

    // 读 headers
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            // 空行 = header 结束
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("Content-Length")
        {
            content_length = value.trim().parse().ok();
        }
    }
    let len = match content_length {
        Some(l) => l,
        None => return Ok(None), // 没 Content-Length header, EOF
    };
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(Some(String::from_utf8_lossy(&body).to_string()))
}

/// 写一条 JSON-RPC message (Content-Length framing)
async fn write_message<W: AsyncWriteExt + Unpin>(writer: &mut W, body: &str) -> io::Result<()> {
    let bytes = body.as_bytes();
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(bytes).await?;
    writer.flush().await
}

/// dispatch 一条 JSON-RPC 请求
/// 返回 None 表示是 notification (不响应)
async fn dispatch(
    req: &JsonValue,
    tools: &ToolRegistry,
    interp: &Arc<RwLock<Interpreter>>,
) -> Option<JsonValue> {
    let method = req.get("method").and_then(|v| v.as_str())?;
    let id = req.get("id").cloned();

    // notification 没 id
    let is_notification = id.is_none();

    let result = match method {
        "initialize" => Some(handle_initialize(req)),
        "tools/list" => Some(handle_tools_list(tools).await),
        "tools/call" => Some(handle_tools_call(req, tools, interp).await),
        _ => {
            // 未知 method
            if !is_notification {
                let id_clone = id.clone().unwrap_or(JsonValue::Null);
                Some(JsonValue::Object({
                    let mut m = std::collections::BTreeMap::new();
                    m.insert("jsonrpc".to_string(), JsonValue::String_("2.0".to_string()));
                    m.insert("id".to_string(), id_clone);
                    m.insert(
                        "error".to_string(),
                        JsonValue::Object({
                            let mut e = std::collections::BTreeMap::new();
                            e.insert("code".to_string(), JsonValue::Number(-32601.0));
                            e.insert(
                                "message".to_string(),
                                JsonValue::String_(format!("Method not found: {}", method)),
                            );
                            e
                        }),
                    );
                    m
                }))
            } else {
                None
            }
        }
    };

    if is_notification {
        None
    } else {
        result.map(|r| wrap_response(id.unwrap_or(JsonValue::Null), r))
    }
}

fn wrap_response(id: JsonValue, result: JsonValue) -> JsonValue {
    let mut m = std::collections::BTreeMap::new();
    m.insert("jsonrpc".to_string(), JsonValue::String_("2.0".to_string()));
    m.insert("id".to_string(), id);
    m.insert("result".to_string(), result);
    JsonValue::Object(m)
}

fn handle_initialize(_req: &JsonValue) -> JsonValue {
    // 返回 capabilities (v0.04: 只支持 tools)
    let mut capabilities = std::collections::BTreeMap::new();
    let mut tools_cap = std::collections::BTreeMap::new();
    tools_cap.insert("listChanged".to_string(), JsonValue::Bool(false));
    capabilities.insert("tools".to_string(), JsonValue::Object(tools_cap));

    let mut result = std::collections::BTreeMap::new();
    result.insert(
        "protocolVersion".to_string(),
        JsonValue::String_("2024-11-05".to_string()),
    );
    result.insert("capabilities".to_string(), JsonValue::Object(capabilities));
    result.insert(
        "serverInfo".to_string(),
        JsonValue::Object({
            let mut s = std::collections::BTreeMap::new();
            s.insert("name".to_string(), JsonValue::String_("mora".to_string()));
            s.insert(
                "version".to_string(),
                JsonValue::String_("0.04".to_string()),
            );
            s
        }),
    );
    JsonValue::Object(result)
}

async fn handle_tools_list(tools: &ToolRegistry) -> JsonValue {
    let tools_locked = tools.read().await;
    let mut tools_array = Vec::new();
    for (name, tool) in tools_locked.iter() {
        let mut t = std::collections::BTreeMap::new();
        t.insert("name".to_string(), JsonValue::String_(name.clone()));
        t.insert(
            "description".to_string(),
            JsonValue::String_(tool.description.clone()),
        );
        // parameters: 解析 JSON Schema 字符串,失败则用空 schema
        let schema = crate::lsp::json::parse(&tool.parameters)
            .unwrap_or_else(|_| JsonValue::Object(std::collections::BTreeMap::new()));
        t.insert("inputSchema".to_string(), schema);
        tools_array.push(JsonValue::Object(t));
    }
    let mut result = std::collections::BTreeMap::new();
    result.insert("tools".to_string(), JsonValue::Array(tools_array));
    JsonValue::Object(result)
}

async fn handle_tools_call(
    req: &JsonValue,
    tools: &ToolRegistry,
    interp: &Arc<RwLock<Interpreter>>,
) -> JsonValue {
    // parse params.name and params.arguments
    let params = req.get("params");
    let name = match params.and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
        Some(n) => n.to_string(),
        None => return mcp_error(-32602, "Missing 'name' in params"),
    };
    let args_json = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or(JsonValue::Object(std::collections::BTreeMap::new()));

    // 找 tool
    let tool = {
        let tools = tools.read().await;
        tools.get(&name).cloned()
    };
    let tool = match tool {
        Some(t) => t,
        None => return mcp_error(-32602, &format!("Unknown tool: {}", name)),
    };

    // 把 args 转成 Mora Value
    let args_value = json_to_mora_value(&args_json);

    // 调 handler 闭包 — 在 spawn_blocking 中执行同步调用，避免阻塞 tokio 异步线程
    let handler = tool.handler.clone();
    let interp = interp.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut interp = interp.blocking_write();
        interp.call_value(&handler, vec![args_value])
    })
    .await;

    match result {
        Ok(Ok(v)) => {
            // 转成 MCP content 格式
            let result_json = mora_to_json(&v);
            let content_item = match result_json {
                JsonValue::String_(s) => {
                    let mut c = std::collections::BTreeMap::new();
                    c.insert("type".to_string(), JsonValue::String_("text".to_string()));
                    c.insert("text".to_string(), JsonValue::String_(s));
                    JsonValue::Object(c)
                }
                other => {
                    // 非 string 结果: 转 JSON 字符串
                    let mut c = std::collections::BTreeMap::new();
                    c.insert("type".to_string(), JsonValue::String_("text".to_string()));
                    c.insert(
                        "text".to_string(),
                        JsonValue::String_(json_to_string(&other)),
                    );
                    JsonValue::Object(c)
                }
            };

            let mut result_map = std::collections::BTreeMap::new();
            result_map.insert("content".to_string(), JsonValue::Array(vec![content_item]));
            JsonValue::Object(result_map)
        }
        Ok(Err(e)) => mcp_error(-32603, &format!("Tool execution error: {}", e)),
        Err(e) => mcp_error(-32603, &format!("Task panicked: {}", e)),
    }
}

fn mcp_error(code: i64, message: &str) -> JsonValue {
    let mut m = std::collections::BTreeMap::new();
    m.insert("code".to_string(), JsonValue::Number(code as f64));
    m.insert(
        "message".to_string(),
        JsonValue::String_(message.to_string()),
    );
    JsonValue::Object(m)
}

fn json_to_mora_value(j: &JsonValue) -> Value {
    match j {
        JsonValue::Null => Value::Nil,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => Value::Number(*n),
        JsonValue::String_(s) => Value::String(s.clone()),
        JsonValue::Array(items) => Value::List(items.iter().map(json_to_mora_value).collect()),
        JsonValue::Object(map) => {
            let mut out = HashMap::new();
            for (k, v) in map {
                out.insert(k.clone(), json_to_mora_value(v));
            }
            Value::Dict(out)
        }
    }
}

fn mora_to_json(v: &Value) -> JsonValue {
    match v {
        Value::Nil => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Number(n) => JsonValue::Number(*n),
        Value::String(s) => JsonValue::String_(s.clone()),
        Value::List(items) => JsonValue::Array(items.iter().map(mora_to_json).collect()),
        Value::Dict(map) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in map {
                out.insert(k.clone(), mora_to_json(v));
            }
            JsonValue::Object(out)
        }
        other => JsonValue::String_(other.to_string()),
    }
}
