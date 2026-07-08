//! Mora HTTP Server (v0.50.0: async tokio rewrite)
//!
//! 设计: 零依赖 HTTP/1.1 + 动态路由表 (HTTP method + path → Mora 闭包)
//! 与 v0.03 src/serve/server.rs 不同: endpoint 是 Mora 脚本里声明的,
//! 不是 Rust 代码里写死的 8 个。
//!
//! 协议:
//! - 顶层 `serve as http on port N do ... end` 块收集 routes
//! - 每个 `GET "/path" -> fn(req) ... end` 注册到路由表
//! - 收到 HTTP request 时: 解析 method/path/body,找路由,调闭包
//! - 闭包返回值是 dict,自动 json.stringify 成 response body

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, timeout};

use crate::interpreter::{Interpreter, Value};
use crate::lsp::json::{Value as JsonValue, to_string as json_to_string};

/// HTTP request 解析结果
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// v0.06.4: 路径参数 (/users/:id → {"id": "123"})
    pub params: HashMap<String, String>,
}

/// 动态路由条目: (pattern, handler) — pattern 支持 :param
pub type RouteTable = Arc<tokio::sync::RwLock<HashMap<(String, String), Value>>>;

/// v0.06.4: 尝试匹配路径参数 — 返回 Some(params) 或 None
fn match_path_pattern(pattern: &str, actual: &str) -> Option<HashMap<String, String>> {
    let pat_segs: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let act_segs: Vec<&str> = actual.trim_matches('/').split('/').collect();
    if pat_segs.len() != act_segs.len() {
        return None;
    }
    let mut params = HashMap::new();
    for (p, a) in pat_segs.iter().zip(act_segs.iter()) {
        if let Some(name) = p.strip_prefix(':') {
            params.insert(name.to_string(), a.to_string());
        } else if p != a {
            return None;
        }
    }
    Some(params)
}

/// v0.06.4: 解析 query string → dict
fn parse_query_dict(query: &str) -> Value {
    use std::collections::HashMap;
    let mut m = HashMap::new();
    if query.is_empty() {
        return Value::Dict(m);
    }
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        m.insert(k.to_string(), Value::String(v.to_string()));
    }
    Value::Dict(m)
}

/// 启动 HTTP server (async)
/// routes 是从 Router 显式 API 收集的路由表
/// v0.50.0: 完全 async 化 — tokio::sync::RwLock + tokio::spawn + spawn_blocking
pub async fn start(
    host: &str,
    port: u16,
    routes: RouteTable,
    interpreter: Arc<tokio::sync::RwLock<Interpreter>>,
) -> io::Result<()> {
    let (listener, actual_port) = bind_with_fallback(host, port, 4).await?;
    let addr = format!("{}:{}", host, actual_port);
    eprintln!("[serve] Mora HTTP server listening on http://{}", addr);
    eprintln!("[serve] Endpoints:");
    let by_method: HashMap<String, Vec<String>> = {
        let routes = routes.read().await;
        let mut by_method: HashMap<String, Vec<String>> = HashMap::new();
        for (m, p) in routes.keys() {
            by_method.entry(m.clone()).or_default().push(p.clone());
        }
        by_method
    };
    for (m, paths) in &by_method {
        for p in paths {
            eprintln!("  {:6} {}", m, p);
        }
    }
    eprintln!();

    loop {
        let (stream, _) = listener.accept().await?;
        let routes = routes.clone();
        let interp = interpreter.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, routes, interp).await {
                eprintln!("[serve] connection error: {}", e);
            }
        });
    }
}

/// v0.50.0: 异步端口绑定（带 fallback）
async fn bind_with_fallback(
    host: &str,
    requested_port: u16,
    max_attempts: u16,
) -> io::Result<(TcpListener, u16)> {
    for offset in 0..max_attempts {
        let port = match requested_port.checked_add(offset) {
            Some(p) => p,
            None => break,
        };
        let bind_addr = format!("{}:{}", host, port);
        match TcpListener::bind(&bind_addr).await {
            Ok(listener) => {
                if offset > 0 {
                    eprintln!(
                        "[serve] requested port {} unavailable, using {} instead",
                        requested_port, port
                    );
                }
                return Ok((listener, port));
            }
            Err(_) => continue,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        format!(
            "could not bind to any port in range {}-{}",
            requested_port,
            requested_port.saturating_add(max_attempts - 1)
        ),
    ))
}

async fn handle_connection(
    mut stream: TcpStream,
    routes: RouteTable,
    interpreter: Arc<tokio::sync::RwLock<Interpreter>>,
) -> io::Result<()> {
    let mut req = match timeout(Duration::from_secs(30), parse_request(&mut stream)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request read timeout",
            ));
        }
    };
    req.params = HashMap::new();

    // v0.37 (P1-1.6b): hoist method/path clones before the lock so the
    // lock only guards the HashMap::get() and the iter(), not String
    // allocations.
    let req_method = req.method.clone();
    let req_path = req.path.clone();
    let handler_with_params: Option<(Value, HashMap<String, String>)> = {
        let routes = routes.read().await;
        // 1) 精确匹配
        if let Some(h) = routes.get(&(req_method.clone(), req_path.clone())) {
            Some((h.clone(), HashMap::new()))
        } else {
            // 2) 模式匹配 — 遍历所有注册路由
            let mut found: Option<(Value, HashMap<String, String>)> = None;
            for ((_m, pattern), h) in routes.iter() {
                if *_m != req.method {
                    continue;
                }
                if let Some(params) = match_path_pattern(pattern, &req.path) {
                    found = Some((h.clone(), params));
                    break;
                }
            }
            found
        }
    };

    let (status, body_str) = match handler_with_params {
        Some((handler_value, params)) => {
            req.params = params;
            // v0.50.0 (P0-4): handler 级 60s 超时, 防止 LLM/AI 慢响应饿死 worker
            // spawn_blocking 调 invoke_handler (内部持 interpreter lock);
            // 超时返回 504 Gateway Timeout.
            let req_for_task = HttpRequest {
                method: req.method.clone(),
                path: req.path.clone(),
                query: req.query.clone(),
                headers: req.headers.clone(),
                body: req.body.clone(),
                params: req.params.clone(),
            };
            let interp_clone = interpreter.clone();
            let handle = tokio::task::spawn_blocking(move || {
                invoke_handler(handler_value, &req_for_task, interp_clone)
            });
            match timeout(Duration::from_secs(60), handle).await {
                Ok(Ok(Ok(value))) => {
                    let json = value_to_json_string(&value);
                    (200, json)
                }
                Ok(Ok(Err(e))) => (500, json_error(&format!("handler error: {}", e))),
                Ok(Err(_)) => (500, json_error("handler panicked")),
                Err(_) => (504, json_error("handler timeout after 60s")),
            }
        }
        None => (
            404,
            json_error(&format!("no route for {} {}", req.method, req.path)),
        ),
    };

    send_response(&mut stream, status, &body_str).await
}

/// 调 Mora 闭包,把 request 包装成 dict 传入. 附加 .json() / .text() 方法
/// 在 spawn_blocking 中执行，使用 blocking_write() 获取 interpreter 锁
fn invoke_handler(
    handler: Value,
    req: &HttpRequest,
    interpreter: Arc<tokio::sync::RwLock<Interpreter>>,
) -> Result<Value, String> {
    // 构造 req dict
    let mut req_dict = HashMap::new();
    req_dict.insert("method".to_string(), Value::String(req.method.clone()));
    req_dict.insert("path".to_string(), Value::String(req.path.clone()));
    // v0.06.4: query 改为 dict
    req_dict.insert("query".to_string(), parse_query_dict(&req.query));
    req_dict.insert("body".to_string(), parse_body_value(&req.body));
    let mut headers_dict = HashMap::new();
    for (k, v) in &req.headers {
        headers_dict.insert(k.clone(), Value::String(v.clone()));
    }
    req_dict.insert("headers".to_string(), Value::Dict(headers_dict));
    // v0.06.4: 路径参数注入
    let mut params_dict = HashMap::new();
    for (k, v) in &req.params {
        params_dict.insert(k.clone(), Value::String(v.clone()));
    }
    req_dict.insert("params".to_string(), Value::Dict(params_dict));
    let req_value = Value::Dict(req_dict);

    // 调闭包: handler(req_dict)
    let mut interp = interpreter.blocking_write();
    interp.call_value(&handler, vec![req_value])
}

/// 解析 body 字符串为 Value
/// 优先尝试 json.parse,失败则当 string
fn parse_body_value(body: &str) -> Value {
    if body.is_empty() {
        return Value::String(String::new());
    }
    // 试 JSON
    match crate::lsp::json::parse(body) {
        Ok(JsonValue::Object(map)) => {
            let mut out = HashMap::new();
            for (k, v) in map {
                out.insert(k, json_lsp_to_value(v));
            }
            Value::Dict(out)
        }
        Ok(JsonValue::Array(items)) => {
            Value::List(items.into_iter().map(json_lsp_to_value).collect())
        }
        Ok(other) => json_lsp_to_value(other),
        Err(_) => Value::String(body.to_string()),
    }
}

fn json_lsp_to_value(j: JsonValue) -> Value {
    match j {
        JsonValue::Null => Value::Nil,
        JsonValue::Bool(b) => Value::Bool(b),
        JsonValue::Number(n) => Value::Number(n),
        JsonValue::String_(s) => Value::String(s),
        JsonValue::Array(items) => Value::List(items.into_iter().map(json_lsp_to_value).collect()),
        JsonValue::Object(map) => {
            let mut out = HashMap::new();
            for (k, v) in map {
                out.insert(k, json_lsp_to_value(v));
            }
            Value::Dict(out)
        }
    }
}

/// Mora Value → JSON 字符串
fn value_to_json_string(v: &Value) -> String {
    json_to_string(&value_to_json(v))
}

fn value_to_json(v: &Value) -> JsonValue {
    match v {
        Value::Nil => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        // v0.38: Int fits i64; Float and Number use f64.
        Value::Int(i) => JsonValue::Number(*i as f64),
        Value::Float(n) => JsonValue::Number(*n),
        Value::Number(n) => JsonValue::Number(*n),
        Value::String(s) => JsonValue::String_(s.clone()),
        Value::Char(c) => JsonValue::String_(c.to_string()),
        Value::List(items) => JsonValue::Array(items.iter().map(value_to_json).collect()),
        Value::Dict(map) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in map {
                out.insert(k.clone(), value_to_json(v));
            }
            JsonValue::Object(out)
        }
        Value::Task { name, .. } => JsonValue::String_(format!("<task {}>", name)),
        Value::Closure { .. } => JsonValue::String_("<closure>".to_string()),
        Value::Builtin(kind) => JsonValue::String_(format!("<builtin {}>", kind)),
        Value::Stream { .. } => JsonValue::String_("<stream>".to_string()),
        Value::Conversation { .. } => JsonValue::String_("<conversation>".to_string()),
        Value::Agent { .. } => JsonValue::String_("<agent>".to_string()),
        Value::AiConfig { .. } => JsonValue::String_("<ai_config>".to_string()),
        Value::Router { .. } => JsonValue::String_("<router>".to_string()),
        Value::TraitObject { .. } => JsonValue::String_("<trait_object>".to_string()),
        Value::HttpRequest { .. } => JsonValue::String_("<http_request>".to_string()),
        Value::McpServer { .. } => JsonValue::String_("<mcp_server>".to_string()),
        Value::Compose(_) => JsonValue::String_("<compose>".to_string()),
        Value::Partial(_, _) => JsonValue::String_("<partial>".to_string()),
        Value::Atom(_) => JsonValue::String_("<atom>".to_string()),
        Value::Macro { .. } => JsonValue::String_("<macro>".to_string()),
        Value::PromptSection { .. } => JsonValue::String_("<prompt_section>".to_string()),
        Value::Document { .. } => JsonValue::String_("<document>".to_string()),
    }
}

fn json_error(msg: &str) -> String {
    let escaped = msg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{{\"error\":\"{}\"}}", escaped)
}

// ===================================================================
// HTTP 解析 (async)
// ===================================================================

async fn parse_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut reader = BufReader::new(stream);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).await?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().unwrap_or(&"GET").to_string();

    // path 可能带 query string
    let raw_path = parts.get(1).unwrap_or(&"/").to_string();
    let (path, query) = match raw_path.find('?') {
        Some(i) => (raw_path[..i].to_string(), raw_path[i + 1..].to_string()),
        None => (raw_path, String::new()),
    };

    let mut content_length: usize = 0;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim().split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    let mut body = String::new();
    if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf).await?;
        body = String::from_utf8_lossy(&buf).to_string();
    }

    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
        params: HashMap::new(),
    })
}

async fn send_response(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    // S5 fix: CORS 通配符改为可配置。默认 *（本地开发兼容），生产环境应通过
    // MORA_CORS_ORIGIN 环境变量收紧（如 "https://example.com"）。
    // 注意：S6 已将默认绑定改为 127.0.0.1，* 的实际风险已大幅降低（仅本机可达）。
    let cors_origin = std::env::var("MORA_CORS_ORIGIN").unwrap_or_else(|_| "*".to_string());
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        body.len(),
        cors_origin,
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}
