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
use std::io::{self, BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

use crate::interpreter::{Interpreter, Value};
use crate::lsp::json::{Value as JsonValue, to_string as json_to_string};

/// MCP tool entry: name + description + parameters JSON schema + handler closure
#[derive(Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub parameters: String, // JSON Schema 字符串
    pub handler: Value,     // Mora 闭包
}

pub type ToolRegistry = Arc<Mutex<HashMap<String, McpTool>>>;

/// 启动 MCP server (阻塞当前线程, 读 stdin 写 stdout)
/// v0.22: 异步处理优化 - 使用线程池处理请求
pub fn start(tools: ToolRegistry, interpreter: Arc<Mutex<Interpreter>>) -> io::Result<()> {
    eprintln!("[mcp] Mora MCP server starting on stdio");
    {
        let tools = tools.lock().expect("tools mutex poisoned");
        eprintln!("[mcp] Registered {} tool(s):", tools.len());
        for (name, _) in tools.iter() {
            eprintln!("[mcp]   - {}", name);
        }
    }
    eprintln!();

    // v0.22: 创建请求处理线程池
    let pool_size = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);  // MCP 最多 8 个并发

    let (tx, rx) = std::sync::mpsc::channel::<(JsonValue, std::sync::mpsc::Sender<JsonValue>)>();
    let rx = Arc::new(Mutex::new(rx));

    // 启动工作线程
    for _ in 0..pool_size {
        let rx = rx.clone();
        let tools = tools.clone();
        let interp = interpreter.clone();
        std::thread::spawn(move || {
            loop {
                let result = {
                    let guard = rx.lock().expect("rx mutex poisoned");
                    guard.recv()
                };
                let (req, resp_tx) = match result {
                    Ok(v) => v,
                    Err(_) => break, // channel 关闭
                };

                // 处理请求
                let response = dispatch(&req, &tools, &interp);

                // 发送响应
                if let Some(resp) = response {
                    let _ = resp_tx.send(resp);
                }
            }
        });
    }

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        // 读一条 JSON-RPC message (Content-Length framing)
        let body = match read_message(&mut reader) {
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

        // v0.22: 异步分发请求
        let is_notification = req.get("id").is_none();

        if is_notification {
            // notification 直接处理，不等待响应
            let _ = dispatch(&req, &tools, &interpreter);
        } else {
            // 请求异步处理
            let (resp_tx, resp_rx) = std::sync::mpsc::channel();
            if let Err(e) = tx.send((req, resp_tx)) {
                eprintln!("[mcp] dispatch error: {}", e);
                continue;
            }

            // 等待响应
            match resp_rx.recv() {
                Ok(resp) => {
                    let resp_str = json_to_string(&resp);
                    write_message(&mut writer, &resp_str)?;
                }
                Err(e) => {
                    eprintln!("[mcp] response error: {}", e);
                }
            }
        }
    }
    Ok(())
}

/// 读一条 JSON-RPC message (Content-Length framing)
/// 返回 None 表示 EOF
fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();

    // 读 headers
    loop {
        line.clear();
        let mut byte = [0u8; 1];
        let mut got_eof = false;
        loop {
            match reader.read(&mut byte) {
                Ok(0) => {
                    got_eof = true;
                    break;
                }
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    }
                    if byte[0] != b'\r' {
                        line.push(byte[0] as char);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if got_eof {
            return Ok(None);
        }
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
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).to_string()))
}

/// 写一条 JSON-RPC message (Content-Length framing)
fn write_message<W: Write>(writer: &mut W, body: &str) -> io::Result<()> {
    let bytes = body.as_bytes();
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(bytes)?;
    writer.flush()
}

/// dispatch 一条 JSON-RPC 请求
/// 返回 None 表示是 notification (不响应)
fn dispatch(
    req: &JsonValue,
    tools: &ToolRegistry,
    interp: &Arc<Mutex<Interpreter>>,
) -> Option<JsonValue> {
    let method = req.get("method").and_then(|v| v.as_str())?;
    let id = req.get("id").cloned();

    // notification 没 id
    let is_notification = id.is_none();

    let result = match method {
        "initialize" => Some(handle_initialize(req)),
        "tools/list" => Some(handle_tools_list(tools)),
        "tools/call" => Some(handle_tools_call(req, tools, interp)),
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

fn handle_tools_list(tools: &ToolRegistry) -> JsonValue {
    let tools_locked = tools.lock().expect("tools mutex poisoned");
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

fn handle_tools_call(
    req: &JsonValue,
    tools: &ToolRegistry,
    interp: &Arc<Mutex<Interpreter>>,
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
        let tools = tools.lock().expect("tools mutex poisoned");
        tools.get(&name).cloned()
    };
    let tool = match tool {
        Some(t) => t,
        None => return mcp_error(-32602, &format!("Unknown tool: {}", name)),
    };

    // 把 args 转成 Mora Value
    let args_value = json_to_mora_value(&args_json);

    // 调 handler 闭包
    let result = {
        let mut interp = interp.lock().expect("interp mutex poisoned");
        match interp.call_value(&tool.handler, vec![args_value]) {
            Ok(v) => v,
            Err(e) => return mcp_error(-32603, &format!("Tool execution error: {}", e)),
        }
    };

    // 转成 MCP content 格式
    let result_json = mora_to_json(&result);
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
