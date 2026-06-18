//! Mora HTTP Server (v0.04: 内嵌在解释器中)
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
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use crate::ast::HttpMethod;
use crate::interpreter::{Interpreter, Value};
use crate::lsp::json::{to_string as json_to_string, Value as JsonValue};

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
pub type RouteTable = Arc<Mutex<HashMap<(String, String), Value>>>;

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

/// 启动 HTTP server (阻塞当前线程)
/// routes 是从 Stmt::Serve 提取的路由表
pub fn start(
    host: &str,
    port: u16,
    routes: RouteTable,
    interpreter: Arc<Mutex<Interpreter>>,
) -> io::Result<()> {
    let addr = format!("{}:{}", host, port);
    let listener = TcpListener::bind(&addr)?;
    eprintln!("[serve] Mora HTTP server listening on http://{}", addr);
    eprintln!("[serve] Endpoints:");
    {
        let routes = routes.lock().unwrap();
        let mut by_method: HashMap<&str, Vec<&str>> = HashMap::new();
        for (m, p) in routes.keys() {
            by_method.entry(m.as_str()).or_default().push(p.as_str());
        }
        for (m, paths) in by_method {
            for p in paths {
                eprintln!("  {:6} {}", m, p);
            }
        }
    }
    eprintln!();

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let routes = routes.clone();
                let interp = interpreter.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, routes, interp) {
                        eprintln!("[serve] connection error: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("[serve] accept error: {}", e),
        }
    }
    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    routes: RouteTable,
    interpreter: Arc<Mutex<Interpreter>>,
) -> io::Result<()> {
    let mut req = parse_request(&mut stream)?;
    req.params = HashMap::new();

    // v0.06.4: 先精确匹配，再模式匹配 (:param)
    let handler_with_params: Option<(Value, HashMap<String, String>)> = {
        let routes = routes.lock().unwrap();
        // 1) 精确匹配
        if let Some(h) = routes.get(&(req.method.clone(), req.path.clone())) {
            Some((h.clone(), HashMap::new()))
        } else {
            // 2) 模式匹配 — 遍历所有注册路由
            let mut found: Option<(Value, HashMap<String, String>)> = None;
            for ((_m, pattern), h) in routes.iter() {
                if *_m != req.method { continue; }
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
            match invoke_handler(handler_value, &req, interpreter) {
                Ok(value) => {
                    let json = value_to_json_string(&value);
                    (200, json)
                }
                Err(e) => (500, json_error(&format!("handler error: {}", e))),
            }
        }
        None => {
            (404, json_error(&format!("no route for {} {}", req.method, req.path)))
        }
    };

    send_response(&mut stream, status, &body_str)
}

/// 调 Mora 闭包,把 request 包装成 dict 传入
fn invoke_handler(
    handler: Value,
    req: &HttpRequest,
    interpreter: Arc<Mutex<Interpreter>>,
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
    let mut interp = interpreter.lock().unwrap();
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
                out.insert(k, json_to_value(v));
            }
            Value::Dict(out)
        }
        Ok(JsonValue::Array(items)) => {
            Value::List(items.into_iter().map(json_to_value).collect())
        }
        Ok(other) => json_to_value(other),
        Err(_) => Value::String(body.to_string()),
    }
}

fn json_to_value(j: JsonValue) -> Value {
    match j {
        JsonValue::Null => Value::Nil,
        JsonValue::Bool(b) => Value::Bool(b),
        JsonValue::Number(n) => Value::Number(n),
        JsonValue::String_(s) => Value::String(s),
        JsonValue::Array(items) => Value::List(items.into_iter().map(json_to_value).collect()),
        JsonValue::Object(map) => {
            let mut out = HashMap::new();
            for (k, v) in map {
                out.insert(k, json_to_value(v));
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
        Value::Number(n) => JsonValue::Number(*n),
        Value::String(s) => JsonValue::String_(s.clone()),
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
        Value::Builtin(name) => JsonValue::String_(format!("<builtin {}>", name)),
        Value::Stream { .. } => JsonValue::String_("<stream>".to_string()),
        Value::Conversation { .. } => JsonValue::String_("<conversation>".to_string()),
        Value::Agent { .. } => JsonValue::String_("<agent>".to_string()),
        Value::AiConfig { .. } => JsonValue::String_("<ai_config>".to_string()),
        Value::Router { .. } => JsonValue::String_("<router>".to_string()),
        Value::HttpRequest { .. } => JsonValue::String_("<http_request>".to_string()),
    }
}


fn json_error(msg: &str) -> String {
    let escaped = msg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{{\"error\":\"{}\"}}", escaped)
}

// ===================================================================
// HTTP 解析
// ===================================================================

fn parse_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let parts: Vec<&str> = first_line.trim().split_whitespace().collect();
    let method = parts.get(0).unwrap_or(&"GET").to_string();

    // path 可能带 query string
    let raw_path = parts.get(1).unwrap_or(&"/").to_string();
    let (path, query) = match raw_path.find('?') {
        Some(i) => (raw_path[..i].to_string(), raw_path[i+1..].to_string()),
        None => (raw_path, String::new()),
    };

    let mut content_length: usize = 0;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() { break; }
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
        reader.read_exact(&mut buf)?;
        body = String::from_utf8_lossy(&buf).to_string();
    }

    Ok(HttpRequest { method, path, query, headers, body, params: HashMap::new() })
}

fn send_response(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        status, status_text, body.as_bytes().len(), body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}
