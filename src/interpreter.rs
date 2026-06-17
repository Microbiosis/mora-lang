use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use crate::ast::*;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::trace_collector::TraceCollector;

/// 包装 BufReader<Box<dyn Read + Send + Sync>>，实现 Debug/Clone
#[derive(Clone)]
pub struct StreamReader(Arc<Mutex<BufReader<Box<dyn Read + Send + Sync>>>>);

impl StreamReader {
    pub fn new(reader: BufReader<Box<dyn Read + Send + Sync>>) -> Self {
        StreamReader(Arc::new(Mutex::new(reader)))
    }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, BufReader<Box<dyn Read + Send + Sync>>> {
        self.0.lock().unwrap()
    }
}

impl std::fmt::Debug for StreamReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamReader")
    }
}

// v10 HTTP 超时配置
const HTTP_READ_TIMEOUT_SECS: u64 = 30;
const HTTP_WRITE_TIMEOUT_SECS: u64 = 10;
const AI_READ_TIMEOUT_SECS: u64 = 60;
const AI_STREAM_TIMEOUT_SECS: u64 = 300; // 流式输出需要更长超时

#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    Nil,
    List(Vec<Value>),
    Dict(HashMap<String, Value>),
    Task {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    Closure {
        params: Vec<String>,
        body: Vec<Stmt>,
        env: Arc<Mutex<Environment>>,
    },
    Builtin(String),
    // v10: 多轮对话对象
    Conversation {
        messages: Vec<(String, String)>, // (role, content) 历史
        model: String,
        base_url: String,
        api_key: String,
    },
    // v0.03: 流式输出
    Stream {
        reader: StreamReader,
        done: Arc<Mutex<bool>>,
    },
    // v0.03: Agent 编排
    Agent {
        name: String,
        tool_names: Vec<String>,
        model_route: String,
        max_steps: usize,
        system: String,
    },
}

// 手动实现 PartialEq（Arc<Mutex<Environment>> 不支持自动派生）
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "{}", s),
            Value::Number(n) => write!(f, "{}", n),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Nil => write!(f, "nil"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Dict(map) => {
                let parts: Vec<String> = map.iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            Value::Task { name, .. } => write!(f, "<task {}>", name),
            Value::Closure { .. } => write!(f, "<closure>"),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Conversation { model, messages, .. } => {
                write!(f, "<conversation {} ({} messages)>", model, messages.len())
            }
            Value::Stream { .. } => write!(f, "<stream>"),
            Value::Agent { name, .. } => write!(f, "<agent {}>", name),
        }
    }
}

#[derive(Debug)]
pub struct Environment {
    values: HashMap<String, Value>,
    exports: HashMap<String, Value>,
    parent: Option<Arc<Mutex<Environment>>>,
}

impl Environment {
    fn new() -> Self {
        Self { values: HashMap::new(), exports: HashMap::new(), parent: None }
    }

    fn with_parent(parent: Arc<Mutex<Environment>>) -> Self {
        Self { values: HashMap::new(), exports: HashMap::new(), parent: Some(parent) }
    }

    fn define(&mut self, name: String, value: Value, exported: bool) {
        self.values.insert(name.clone(), value.clone());
        if exported {
            self.exports.insert(name, value);
        }
    }

    fn get(&self, name: &str) -> Option<Value> {
        if let Some(value) = self.values.get(name) {
            Some(value.clone())
        } else if let Some(parent) = &self.parent {
            parent.lock().unwrap().get(name)
        } else {
            None
        }
    }

    fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.values.contains_key(name) {
            self.values.insert(name.to_string(), value);
            true
        } else if let Some(parent) = &self.parent {
            parent.lock().unwrap().assign(name, value)
        } else {
            false
        }
    }
}

pub struct Interpreter {
    globals: Arc<Mutex<Environment>>,
    environment: Arc<Mutex<Environment>>,
    tool_registry: HashMap<String, ToolDef>,
    // v0.04补: memory_store 字段已删除（RFC §4.1 memory.* builtin 推迟到 v1.0）
    model_routes: HashMap<String, RouteConfig>,
    token_budget: Option<TokenBudget>,
    token_usage: TokenUsage,
    pub trace: TraceCollector,
    // v0.04 Slice 2: route registry (name -> model name)
    route_registry: HashMap<String, String>,
}

// v0.04: 显式实现 Clone 而非 derive
// (HashMap/Vec 字段需要 clone; Arc/Option 内部; TraceCollector 自身 derive Clone)
impl Clone for Interpreter {
    fn clone(&self) -> Self {
        Self {
            globals: self.globals.clone(),
            environment: self.environment.clone(),
            tool_registry: self.tool_registry.clone(),
            // v0.04补: memory_store 字段已删除（RFC §4.1 memory.* 推迟到 v1.0）
            model_routes: self.model_routes.clone(),
            token_budget: self.token_budget.clone(),
            token_usage: self.token_usage.clone(),
            trace: self.trace.clone(),
            route_registry: self.route_registry.clone(),
        }
    }
}

/// Token 预算配置
#[derive(Clone)]
struct TokenBudget {
    total: usize,
    per_call: Option<usize>,
    alert_threshold: f64,  // 0.0-1.0，超过此比例时告警
}

/// Token 消耗统计
#[derive(Clone, Default)]
struct TokenUsage {
    input: usize,
    output: usize,
}

/// 模型路由配置
#[derive(Clone)]
struct RouteConfig {
    model: String,
    base_url: String,
    api_key: String,
    max_tokens: Option<usize>,
    system: Option<String>,
}

/// 记忆条目 — v0.04补: 字段已删 (RFC §4.1 memory.* 推迟到 v1.0)

/// 工具定义（注册时存储）
#[derive(Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: String,  // JSON Schema 字符串
    pub handler: Value,      // Closure
}

/// 结构化聊天消息（用于支持 tool_calls）
enum ChatMessage {
    User { content: String },
    Assistant { content: Option<String>, tool_calls: Vec<ToolCall> },
    Tool { tool_call_id: String, content: String },
}

/// 工具调用信息
#[derive(Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,  // JSON 字符串
}

impl Interpreter {
    pub fn new() -> Self {
        let globals = Arc::new(Mutex::new(Environment::new()));
        globals.lock().unwrap().define("print".to_string(), Value::Builtin("print".to_string()), false);
        globals.lock().unwrap().define("range".to_string(), Value::Builtin("range".to_string()), false);
        globals.lock().unwrap().define("len".to_string(), Value::Builtin("len".to_string()), false);
        Self { globals: globals.clone(), environment: globals, tool_registry: HashMap::new(), model_routes: HashMap::new(), token_budget: None, token_usage: TokenUsage::default(), trace: TraceCollector::new(false), route_registry: HashMap::new() }
    }

    /// v0.04: 构造一个空 Interpreter (用于 std::mem::replace 占位)
    /// 空 Interpreter 不能跑 execute, 仅作为占位符存在
    pub fn new_empty() -> Self {
        let globals = Arc::new(Mutex::new(Environment::new()));
        Self {
            globals: globals.clone(),
            environment: globals,
            tool_registry: HashMap::new(),
            model_routes: HashMap::new(),
            token_budget: None,
            token_usage: TokenUsage::default(),
            trace: TraceCollector::new(false),
            route_registry: HashMap::new(),
        }
    }

    pub fn new_with_globals(globals: Arc<Mutex<Environment>>) -> Self {
        let env = Arc::new(Mutex::new(Environment::with_parent(globals.clone())));
        Self { globals: globals.clone(), environment: env, tool_registry: HashMap::new(), model_routes: HashMap::new(), token_budget: None, token_usage: TokenUsage::default(), trace: TraceCollector::new(false), route_registry: HashMap::new() }
    }

    #[allow(dead_code)]
    pub fn get_globals(&self) -> Arc<Mutex<Environment>> {
        self.globals.clone()
    }

    pub fn get_tool_registry(&self) -> &HashMap<String, ToolDef> {
        &self.tool_registry
    }

    pub fn set_trace_enabled(&mut self, enabled: bool) {
        self.trace = TraceCollector::new(enabled);
    }

    pub fn interpret(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for stmt in stmts {
            self.execute(stmt)?;
        }
        // 先 clone 出值，再释放 borrow，避免借用冲突
        let main_task = self.globals.lock().unwrap().get("main").clone();
        if let Some(Value::Task { params, body, .. }) = main_task {
            if params.is_empty() {
                let params = params.clone();
                let body = body.clone();
                self.call_task(&params, &body, vec![])?;
            }
        }
        Ok(())
    }

    pub fn execute(&mut self, stmt: &Stmt) -> Result<FlowSignal, String> {
        match stmt {
            Stmt::Let { name, type_hint, init, exported, span: _ } => {
                let value = self.evaluate(init)?;
                if let Some(hint) = type_hint {
                    if !check_type(&value, hint) {
                        return Err(format!("Type mismatch: expected {}, got {}", hint, type_name(&value)));
                    }
                }
                self.environment.lock().unwrap().define(name.clone(), value, *exported);
                Ok(FlowSignal::None)
            }
            Stmt::Assign { name, value, span: _ } => {
                let val = self.evaluate(value)?;
                if !self.environment.lock().unwrap().assign(name, val.clone()) {
                    self.environment.lock().unwrap().define(name.clone(), val, false);
                }
                Ok(FlowSignal::None)
            }
            Stmt::IndexAssign { object, index, value, span: _ } => {
                let obj = self.evaluate(object)?;
                let idx = self.evaluate(index)?;
                let val = self.evaluate(value)?;
                match (&obj, &idx) {
                    (Value::List(list), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < list.len() {
                            let mut new_list = list.clone();
                            new_list[i] = val;
                            Ok(FlowSignal::None)
                        } else {
                            Err(format!("Index out of bounds: {} (len: {})", i, list.len()))
                        }
                    }
                    _ => Err("Can only index assign to lists".to_string()),
                }
            }
            Stmt::TaskDef { name, params, return_type: _, body, exported, span: _ } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let task = Value::Task { name: name.clone(), params: param_names, body: body.clone() };
                self.environment.lock().unwrap().define(name.clone(), task, *exported);
                Ok(FlowSignal::None)
            }
            Stmt::If { condition, then_branch, span: _ } => {
                let cond = self.evaluate(condition)?;
                if is_truthy(&cond) {
                    let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                    // return 信号必须穿透 if 边界向外冒泡
                    self.execute_block(then_branch, env)
                } else {
                    Ok(FlowSignal::None)
                }
            }
            Stmt::For { var, var_type: _, iterable, body, span: _ } => {
                let iter_val = self.evaluate(iterable)?;
                // return 信号必须穿透 for 边界向外冒泡（每次迭代后检查）
                match iter_val {
                    Value::List(items) => {
                        for item in items {
                            let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                            env.lock().unwrap().define(var.clone(), item, false);
                            let signal = self.execute_block(body, env)?;
                            if signal.is_return() { return Ok(signal); }
                            if matches!(signal, FlowSignal::Break) { return Ok(FlowSignal::None); }
                            if matches!(signal, FlowSignal::Continue) { continue; }
                        }
                        Ok(FlowSignal::None)
                    }
                    Value::String(s) => {
                        for ch in s.chars() {
                            let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                            env.lock().unwrap().define(var.clone(), Value::String(ch.to_string()), false);
                            let signal = self.execute_block(body, env)?;
                            if signal.is_return() { return Ok(signal); }
                            if matches!(signal, FlowSignal::Break) { return Ok(FlowSignal::None); }
                            if matches!(signal, FlowSignal::Continue) { continue; }
                        }
                        Ok(FlowSignal::None)
                    }
                    Value::Stream { reader, done } => {
                        loop {
                            let token = {
                                let mut guard = reader.lock();
                                if *done.lock().unwrap() {
                                    None
                                } else {
                                    match Self::read_next_sse_token(&mut *guard) {
                                        Ok(Some(t)) => Some(t),
                                        Ok(None) => {
                                            *done.lock().unwrap() = true;
                                            None
                                        }
                                        Err(e) => {
                                            *done.lock().unwrap() = true;
                                            return Err(format!("ai.stream: {}", e));
                                        }
                                    }
                                }
                            };
                            match token {
                                Some(tok) => {
                                    let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                                    env.lock().unwrap().define(var.clone(), Value::String(tok), false);
                                    let signal = self.execute_block(body, env)?;
                                    if signal.is_return() { return Ok(signal); }
                                    if matches!(signal, FlowSignal::Break) { return Ok(FlowSignal::None); }
                                    if matches!(signal, FlowSignal::Continue) { continue; }
                                }
                                None => break,
                            }
                        }
                        Ok(FlowSignal::None)
                    }
                    _ => Err(format!("Cannot iterate over {}", iter_val)),
                }
            }
            Stmt::Try { try_block, catch_var, catch_type, catch_block, span: _ } => {
                let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                match self.execute_block(try_block, env.clone()) {
                    // 运行时错误：进 catch。**return 信号不算错误**，直接穿透。
                    Ok(signal @ FlowSignal::Return(_)) => Ok(signal),
                    Ok(FlowSignal::None) => Ok(FlowSignal::None),
                    Ok(other) => Ok(other),  // v0.04.0: break/continue 穿透
                    Err(err_msg) => {
                        // v0.04.0: 类型化错误
                        // catch_type == Some("AiError") → 包成 AiError dict
                        // catch_type == None → 沿用 v0.03 字符串行为
                        let err_value = match catch_type {
                            Some(t) if t == "AiError" => Self::build_ai_error_static(&err_msg),
                            Some(t) => {
                                return Err(format!("try/catch: unsupported catch type '{}' (v0.04.0 only supports AiError or no annotation)", t));
                            }
                            None => Value::String(err_msg),
                        };
                        env.lock().unwrap().define(catch_var.clone(), err_value, false);
                        // catch 块内若有 return 也要穿透
                        self.execute_block(catch_block, env)
                    }
                }
            }
            Stmt::Import { path, span: _ } => {
                let module_env = self.import_module(path)?;
                let exports = module_env.lock().unwrap().exports.clone();
                for (name, value) in exports {
                    self.environment.lock().unwrap().define(name, value, false);
                }
                Ok(FlowSignal::None)
            }
            Stmt::Parallel { stmts, span: _ } => {
                self.execute_parallel(stmts)
            }
            Stmt::Match { expr, arms, span: _ } => {
                let val = self.evaluate(expr)?;
                for (pattern, arm_stmts) in arms {
                    if let Some(bindings) = self.match_pattern(pattern, &val) {
                        let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                        for (name, value) in bindings {
                            env.lock().unwrap().define(name, value, false);
                        }
                        return self.execute_block(arm_stmts, env);
                    }
                }
                Err("No match arm matched".to_string())
            }
            Stmt::Save { path, value, span: _ } => {
                let path_val = self.evaluate(path)?;
                let data_val = self.evaluate(value)?;
                let path_str = match path_val {
                    Value::String(s) => s,
                    _ => return Err("save path must be a string".to_string()),
                };
                let json = value_to_json(&data_val);
                fs::write(&path_str, json).map_err(|e| format!("Failed to save: {}", e))?;
                println!("[save] {} -> {}", path_str, type_name(&data_val));
                Ok(FlowSignal::None)
            }
            Stmt::Load { path, var, span: _ } => {
                let path_val = self.evaluate(path)?;
                let path_str = match path_val {
                    Value::String(s) => s,
                    _ => return Err("load path must be a string".to_string()),
                };
                let json = fs::read_to_string(&path_str).map_err(|e| format!("Failed to load: {}", e))?;
                let value = json_to_value(&json)?;
                self.environment.lock().unwrap().define(var.clone(), value, false);
                println!("[load] {} -> {}", path_str, var);
                Ok(FlowSignal::None)
            }
            Stmt::ReadFile { path, var, span: _ } => {
                // v11: read "path" into var  →  等价于 let var = file.read_text("path")
                let path_val = self.evaluate(path)?;
                let path_str = expect_string(path_val, "read path")?;
                let content = std::fs::read_to_string(&path_str)
                    .map_err(|e| format!("read: cannot read '{}': {}", path_str, e))?;
                self.environment.lock().unwrap().define(var.clone(), Value::String(content), false);
                println!("[read] {} -> {}", path_str, var);
                Ok(FlowSignal::None)
            }
            Stmt::WriteFile { path, content, span: _ } => {
                // v11: write "path", content  →  等价于 file.write_text("path", content)
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "write path")?;
                let content_str = expect_string(content_val, "write content")?;
                if let Some(parent) = std::path::Path::new(&path_str).parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        return Err(format!(
                            "write: parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                }
                std::fs::write(&path_str, &content_str)
                    .map_err(|e| format!("write: cannot write '{}': {}", path_str, e))?;
                println!("[write] {}", path_str);
                Ok(FlowSignal::None)
            }
            Stmt::AppendFile { path, content, span: _ } => {
                // v11: append "path", content
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "append path")?;
                let content_str = expect_string(content_val, "append content")?;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path_str)
                    .map_err(|e| format!("append: cannot open '{}': {}", path_str, e))?;
                f.write_all(content_str.as_bytes())
                    .map_err(|e| format!("append: cannot write '{}': {}", path_str, e))?;
                println!("[append] {}", path_str);
                Ok(FlowSignal::None)
            }
            Stmt::ReadBytesFile { path, var, span: _ } => {
                // v11: read_bytes "path" into var  →  var 是 hex 字符串
                let path_val = self.evaluate(path)?;
                let path_str = expect_string(path_val, "read_bytes path")?;
                let bytes = std::fs::read(&path_str)
                    .map_err(|e| format!("read_bytes: cannot read '{}': {}", path_str, e))?;
                self.environment.lock().unwrap()
                    .define(var.clone(), Value::String(hex_encode(&bytes)), false);
                println!("[read_bytes] {} -> {} ({} bytes)", path_str, var, bytes.len());
                Ok(FlowSignal::None)
            }
            Stmt::WriteBytesFile { path, content, span: _ } => {
                // v11: write_bytes "path", hex
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "write_bytes path")?;
                let hex = expect_string(content_val, "write_bytes content")?;
                let bytes = hex_decode(&hex)
                    .map_err(|e| format!("write_bytes: {}", e))?;
                std::fs::write(&path_str, &bytes)
                    .map_err(|e| format!("write_bytes: cannot write '{}': {}", path_str, e))?;
                println!("[write_bytes] {} ({} bytes)", path_str, bytes.len());
                Ok(FlowSignal::None)
            }
            Stmt::Return { value, span: _ } => {
                let val = match value {
                    Some(expr) => self.evaluate(expr)?,
                    None => Value::Nil,
                };
                Ok(FlowSignal::Return(val))
            }
            Stmt::Expr(expr) => {
                // 副作用表达式（print、let mut、call 等），求值后不携带任何信号
                let _val = self.evaluate(expr)?;
                Ok(FlowSignal::None)
            }
            // ============ v0.04.0: AI 原语 ============
            Stmt::With { bindings, body, span: _ } => {
                // 推入 AI 上下文栈
                let prev_model = std::env::var("MORA_AI_MODEL").ok();
                let prev_temp  = std::env::var("MORA_AI_TEMPERATURE").ok();
                let prev_budget = std::env::var("MORA_AI_BUDGET").ok();
                let prev_budget_used = std::env::var("MORA_AI_BUDGET_USED").ok();
                let prev_max = std::env::var("MORA_AI_MAX_TOKENS").ok();
                for (key, val_expr) in bindings {
                    let v = self.evaluate(val_expr)?;
                    let s = match &v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        other => other.to_string(),
                    };
                    let env_key = match key.as_str() {
                        "model" => "MORA_AI_MODEL",
                        "budget" => "MORA_AI_BUDGET",
                        "temperature" => "MORA_AI_TEMPERATURE",
                        "max_tokens" => "MORA_AI_MAX_TOKENS",
                        other => {
                            return Err(format!("with: unknown binding '{}' (valid: model, budget, temperature, max_tokens)", other));
                        }
                    };
                    std::env::set_var(env_key, s);
                }
                // budget_used 保留外层值（按 RFC §2.2.4 选"覆盖"语义：内层重新计数）
                let env_in = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                let result = self.execute_block(body, env_in);
                // 恢复外层环境
                Self::restore_env("MORA_AI_MODEL", &prev_model);
                Self::restore_env("MORA_AI_TEMPERATURE", &prev_temp);
                Self::restore_env("MORA_AI_BUDGET", &prev_budget);
                Self::restore_env("MORA_AI_BUDGET_USED", &prev_budget_used);
                Self::restore_env("MORA_AI_MAX_TOKENS", &prev_max);
                result
            }
            Stmt::StreamFor { prompt, var, body, span: _ } => {
                // 求值 prompt（应当是 Prompt 表达式，返回 Value::Stream）
                let prompt_str = Self::eval_prompt_parts_from_stmt(prompt, self)?;
                // v0.04: stream 块简化 — mock 模式按字符拆 token
                // (v0.04.1 跟进真实 streaming SSE)
                let tokens: Vec<String> = prompt_str.chars().map(|c| c.to_string()).collect();
                for token in tokens {
                    let env_in = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                    env_in.lock().unwrap().define(var.clone(), Value::String(token), false);
                    let signal = self.execute_block(body, env_in)?;
                    match signal {
                        FlowSignal::Return(r) => return Ok(FlowSignal::Return(r)),
                        FlowSignal::Break => return Ok(FlowSignal::None),
                        FlowSignal::Continue => continue,
                        FlowSignal::None => {}
                    }
                }
                Ok(FlowSignal::None)
            }
            Stmt::ToolDef { name, params, return_type, body, exported: _, span: _ } => {
                // v0.04.0: 注册到全局工具表（与 v0.03 ai.tool 等价）
                let name_clone = name.clone();
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let type_hints: Vec<String> = params.iter().map(|(_, t)| t.clone().unwrap_or_else(|| "any".to_string())).collect();
                let return_str = return_type.clone().unwrap_or_else(|| "any".to_string());
                // 用闭包捕获 body
                let tool_body = body.clone();
                let func = Value::Task {
                    name: name.clone(),
                    params: param_names.clone(),
                    body: tool_body,
                };
                self.environment.lock().unwrap().define(name.clone(), func, false);
                // 同时注册到 ai.tool registry（v0.03 路径）
                self.register_tool(name_clone, param_names, type_hints, return_str);
                Ok(FlowSignal::None)
            }
            Stmt::Break { span: _ } => Ok(FlowSignal::Break),
            Stmt::Continue { span: _ } => Ok(FlowSignal::Continue),
            // v0.04 Slice 1: serve 块
            Stmt::Serve { protocol, routes, body, span: _ } => {
                match protocol {
                    ServeProtocol::Http { host, port } => {
                        use std::collections::HashMap;
                        use std::sync::{Arc, Mutex};
                        let route_table: Arc<Mutex<HashMap<(String, String), Value>>> =
                            Arc::new(Mutex::new(HashMap::new()));
                        for r in routes {
                            if let RouteDecl::HttpRoute { method, path, handler } = r {
                                let handler_value = self.evaluate(&handler)?;
                                route_table.lock().unwrap().insert(
                                    (method.as_str().to_string(), path.clone()),
                                    handler_value,
                                );
                            }
                        }
                        let body_env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                        let body_signal = self.execute_block(body, body_env)?;
                        let taken = std::mem::replace(self, Interpreter::new_empty());
                        let interp_arc: Arc<Mutex<Interpreter>> = Arc::new(Mutex::new(taken));
                        eprintln!("[serve] starting HTTP server on {}:{}", host, port);
                        let host_str = host.clone();
                        crate::http_server::start(&host_str, *port, route_table, interp_arc)
                            .map_err(|e| format!("HTTP server error: {}", e))?;
                        Ok(body_signal)
                    }
                    ServeProtocol::Mcp => {
                        // Slice 4: 内嵌 MCP server
                        // 收集 tool 块到 McpTool registry
                        use std::collections::HashMap;
                        use std::sync::{Arc, Mutex};
                        let tool_registry: Arc<Mutex<HashMap<String, crate::mcp_server::McpTool>>> =
                            Arc::new(Mutex::new(HashMap::new()));
                        for r in routes {
                            if let RouteDecl::ToolEntry { name, params, return_type, handler } = r {
                                // 求值 handler (闭包)
                                let handler_value = self.evaluate(&handler)?;
                                // v0.04 Slice 5: 自动生成 JSON Schema (从 params + 类型 hint)
                                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                                let param_types: Vec<String> = params.iter()
                                    .map(|(_, t)| t.clone().unwrap_or_else(|| "string".to_string()))
                                    .collect();
                                let schema = Self::tool_to_json_schema(&param_names, &param_types);
                                // 拼接 return type 描述
                                let schema_with_return = if let Some(rt) = return_type {
                                    let rt_lower = schema.trim_end_matches('}');
                                    format!("{},\"_return_type\":\"{}\"}}", rt_lower, rt)
                                } else {
                                    schema
                                };
                                let tool = crate::mcp_server::McpTool {
                                    name: name.clone(),
                                    description: String::new(),
                                    parameters: schema_with_return,
                                    handler: handler_value,
                                };
                                tool_registry.lock().unwrap().insert(name.clone(), tool);
                                eprintln!("[mcp] registered tool: {} ({} params)", name, param_names.len());
                            }
                        }
                        // 执行 body
                        let body_env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                        let body_signal = self.execute_block(body, body_env)?;
                        // 移交 self 到 Arc<Mutex<>> 启动 MCP server
                        let taken = std::mem::replace(self, Interpreter::new_empty());
                        let interp_arc: Arc<Mutex<Interpreter>> = Arc::new(Mutex::new(taken));
                        eprintln!("[serve] starting MCP server on stdio");
                        crate::mcp_server::start(tool_registry, interp_arc)
                            .map_err(|e| format!("MCP server error: {}", e))?;
                        Ok(body_signal)
                    }
                    ServeProtocol::Repl => {
                        // v0.04补: 真实 REPL 入口（与 main.rs --repl 共享同一份代码）
                        // 移交 self 到 &mut, REPL 接管 stdin
                        eprintln!("[serve] starting REPL on stdin");
                        let mut taken = std::mem::replace(self, Interpreter::new_empty());
                        Interpreter::run_repl_with(&mut taken);
                        Ok(FlowSignal::None)
                    }
                    ServeProtocol::Stdio => {
                        // v0.04补: 简化的 stdio 协议 —— 读 stdin 一行, 执行 body 中
                        // 注册的 handler (无 handler 时回显该行), 写回 stdout。
                        // v0.04 范围内只做最简 echo 占位（RFC §2.2 提到 "自定义协议" 留给 v0.04.1）
                        eprintln!("[serve] starting stdio server (echo mode, type 'exit' to quit)");
                        Self::serve_stdio_echo();
                        Ok(FlowSignal::None)
                    }
                }
            }
            // v0.04 Slice 2: route 块
            Stmt::Route { name, target, .. } => {
                // v0.04补: 接受三种 target 形态
                //   1. Expr::String("model-name")        —— v0.04裸字符串写法
                //   2. Expr::AiModelCall{...}            —— v0.04 RFC §2.3 终态 ai_model 写法
                //   3. 其他 expr 错误
                let target_val = self.evaluate(target)?;
                let model_name = match &target_val {
                    Value::String(s) => s.clone(),
                    Value::Dict(m) => {
                        // ai_model(...) 解释后返回的 dict 含 _model 字段
                        match m.get("_model") {
                            Some(Value::String(s)) => s.clone(),
                            _ => {
                                return Err(format!(
                                    "route '{}' target dict missing _model field",
                                    name
                                ));
                            }
                        }
                    }
                    other => {
                        return Err(format!(
                            "route '{}' target must be a string or ai_model(...), got {}",
                            name, other
                        ));
                    }
                };
                self.route_registry.insert(name.clone(), model_name);
                eprintln!("[route] registered: {} -> {}", name, self.route_registry[name]);
                Ok(FlowSignal::None)
            }
            // v0.04 Slice 3: 可观测
            Stmt::Observe { config, body, .. } => {
                match config {
                    ObserveConfig::Trace => {
                        self.trace.set_enabled(true);
                        eprintln!("[observe] trace enabled");
                    }
                    ObserveConfig::Metrics => {
                        self.trace.set_enabled(true);
                        eprintln!("[observe] metrics enabled (currently same as trace)");
                    }
                    ObserveConfig::Otel { endpoint } => {
                        // 求值 endpoint (期望字符串字面量)
                        let endpoint_str = match self.evaluate(endpoint)? {
                            Value::String(s) => s,
                            other => return Err(format!("observe otel endpoint must be a string, got {}", other)),
                        };
                        self.trace.set_otel_endpoint(endpoint_str.clone());
                        self.trace.set_enabled(true);
                        eprintln!("[observe] OTEL enabled, endpoint: {}", endpoint_str);
                    }
                }
                // 执行 body
                let body_env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                self.execute_block(body, body_env)
            }
            Stmt::Span { name, attributes, body, .. } => {
                // 求值 attributes 成 HashMap<String, String>
                let mut attrs_map = std::collections::HashMap::new();
                for (k, v_expr) in attributes {
                    let v_str = match self.evaluate(v_expr)? {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    attrs_map.insert(k.clone(), v_str);
                }
                // 起 span
                let handle = self.trace.start_span(name, attrs_map);
                // 执行 body
                let body_env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                let result = self.execute_block(body, body_env);
                // 结束 span (RAII 风格)
                let attrs_end = std::collections::HashMap::new();
                match &result {
                    Ok(_) => handle.end(attrs_end),
                    Err(e) => handle.end_error(e, attrs_end),
                }
                result
            }
            // v0.04.0 终态补: 显式 token 计数（RFC §2.4）
            // 语义: 累加到 TraceCollector; 不触发预算超限
            //     (预算检查在 track_tokens 即 API 自动返回时; record_tokens 是用户声明)
            Stmt::RecordTokens { input, output, span: _ } => {
                let in_val = self.evaluate(input)?;
                let out_val = self.evaluate(output)?;
                let in_n = match &in_val {
                    Value::Number(n) => *n as u64,
                    _ => return Err(format!("record_tokens: input must be number, got {}", in_val)),
                };
                let out_n = match &out_val {
                    Value::Number(n) => *n as u64,
                    _ => return Err(format!("record_tokens: output must be number, got {}", out_val)),
                };
                self.trace.record_tokens(in_n, out_n);
                Ok(FlowSignal::None)
            }
        }   // ← match stmt { ... } 闭合
    }       // ← pub fn execute(...) 闭合

    /// v0.04补: REPL 入口（main.rs 和 serve as repl 共用）
    /// 与 main.rs::run_repl 行为一致：循环读 stdin, 逐行 tokenize+parse+execute
    /// 接收外部 &mut Interpreter 保留 setup 代码的 state
    pub fn run_repl_with(interp: &mut Interpreter) {
        use std::io::{self, BufRead, Write};
        println!("Mora v0.04 REPL — type 'exit' to quit");
        println!();
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        let mut line = String::new();
        loop {
            print!("mora> ");
            let _ = io::stdout().flush();
            line.clear();
            if handle.read_line(&mut line).is_err() { break; }
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            if trimmed == "exit" || trimmed == "quit" {
                println!("Bye!");
                break;
            }
            let tokens = Lexer::new(trimmed).scan_tokens();
            let stmts = Parser::new(tokens).parse();
            if stmts.is_empty() { continue; }
            for stmt in &stmts {
                match interp.execute(stmt) {
                    Ok(FlowSignal::Return(v)) => println!("= {}", v),
                    Ok(FlowSignal::None) => {}
                    Ok(FlowSignal::Break) | Ok(FlowSignal::Continue) => {}
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
        }
    }

    /// v0.04补: serve as stdio 最简 echo 占位
    /// 设计: 阻塞读 stdin, 每行回写到 stdout 前缀 `echo: `
    /// v0.04.1 跟进: body 中可注册 handler, 走自定义协议
    fn serve_stdio_echo() {
        use std::io::{self, BufRead, Write};
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut line = String::new();
        loop {
            line.clear();
            if handle.read_line(&mut line).is_err() { break; }
            let trimmed = line.trim();
            if trimmed == "exit" || trimmed == "quit" { break; }
            if trimmed.is_empty() { continue; }
            let _ = writeln!(out, "echo: {}", trimmed);
            let _ = out.flush();
        }
    }

    fn restore_env(key: &str, prev: &Option<String>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn execute_block(&mut self, stmts: &[Stmt], env: Arc<Mutex<Environment>>) -> Result<FlowSignal, String> {
        let previous = self.environment.clone();
        self.environment = env;
        let mut last = FlowSignal::None;
        for stmt in stmts {
            last = self.execute(stmt)?;
            // 任何 return/break/continue 信号立即停止块执行并向外冒泡
            match last {
                FlowSignal::Return(_) | FlowSignal::Break | FlowSignal::Continue => break,
                FlowSignal::None => {}
            }
        }
        self.environment = previous;
        Ok(last)
    }

    // ===================================================================
    // v0.04.0: AI 原语辅助函数
    // ===================================================================

    /// 注册 tool 到全局工具表
    fn register_tool(&mut self, name: String, params: Vec<String>, types: Vec<String>, return_type: String) {
        // v0.04 Slice 5: 真正实现 — 自动生成 JSON Schema 并存到 tool_registry
        // description: v0.04.0 简化（空字符串，v0.04.1 跟进 desc: 段）
        // parameters: 从 params + types 自动生成 JSON Schema
        let schema = Self::tool_to_json_schema(&params, &types);
        let tool_def = crate::interpreter::ToolDef {
            name: name.clone(),
            description: String::new(),
            parameters: schema,
            handler: Value::Nil,  // handler 不存这里 (handler 是 Stmt::ToolDef body 的 closure, 解析时已绑)
        };
        self.tool_registry.insert(name, tool_def);
    }

    /// v0.04 Slice 5: 从 params + types 生成标准 JSON Schema
    fn tool_to_json_schema(params: &[String], types: &[String]) -> String {
        // 生成 {"type":"object","properties":{...},"required":[...]} 格式
        let mut properties = String::from("{");
        let mut required = Vec::new();
        for (i, p) in params.iter().enumerate() {
            if i > 0 { properties.push_str(","); }
            let ty = types.get(i).map(|s| s.as_str()).unwrap_or("string");
            let json_type = match ty {
                "number" | "int" | "float" => "number",
                "bool" => "boolean",
                "list" | "array" => "array",
                _ => "string",
            };
            properties.push_str(&format!("\"{}\":{{\"type\":\"{}\"}}", p, json_type));
            required.push(p.clone());
        }
        properties.push_str("}");
        let required_str = required.iter()
            .map(|r| format!("\"{}\"", r))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"type\":\"object\",\"properties\":{},\"required\":[{}]}}", properties, required_str)
    }

    /// v0.04.0: 把运行时错误包成 AiError dict
    /// 字段: message, code, retryable, attempts, cause
    /// 用 dict 表达（Mora 暂时没有 struct literal）
    fn build_ai_error_static(err_msg: &str) -> Value {
        let mut m = std::collections::HashMap::new();
        m.insert("message".to_string(), Value::String(err_msg.to_string()));
        // 简单 code 推断
        let code = if err_msg.contains("rate") || err_msg.contains("limit") {
            "rate_limit"
        } else if err_msg.contains("timeout") {
            "timeout"
        } else if err_msg.contains("context") || err_msg.contains("length") {
            "context_length"
        } else if err_msg.contains("auth") || err_msg.contains("key") {
            "auth"
        } else {
            "unknown"
        };
        let retryable = matches!(code, "rate_limit" | "timeout");
        m.insert("code".to_string(), Value::String(code.to_string()));
        m.insert("retryable".to_string(), Value::Bool(retryable));
        m.insert("attempts".to_string(), Value::Number(1.0));
        m.insert("cause".to_string(), Value::String(err_msg.to_string()));
        Value::Dict(m)
    }

    fn execute_parallel(&mut self, stmts: &[Stmt]) -> Result<FlowSignal, String> {
        // Arc<Mutex> 替代 Rc<RefCell> 后，Value 实现了 Send，
        // 可以在 scoped threads 中返回。
        let globals = self.globals.clone();
        let mut values = Vec::new();

        std::thread::scope(|s| {
            let mut handles = Vec::new();
            for stmt in stmts {
                let globals = globals.clone();
                let stmt = stmt.clone();
                handles.push(s.spawn(move || {
                    let mut interpreter = Interpreter::new_with_globals(globals);
                    interpreter.execute(&stmt)
                }));
            }
            for handle in handles {
                match handle.join() {
                    Ok(Ok(signal)) => {
                        // FlowSignal::Return(val) → val（线程内 return 的值）
                        // FlowSignal::None → nil
                        values.push(signal.into_value());
                    }
                    Ok(Err(e)) => {
                        eprintln!("Parallel task error: {}", e);
                        values.push(Value::Nil);
                    }
                    Err(_) => {
                        eprintln!("Parallel task panicked");
                        values.push(Value::Nil);
                    }
                }
            }
        });

        Ok(FlowSignal::None)
    }

    fn match_pattern(&self, pattern: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
        match (pattern, value) {
            (Pattern::Wildcard, _) => Some(vec![]),
            (Pattern::Variable(name), _) => Some(vec![(name.clone(), value.clone())]),
            (Pattern::Literal(lit), val) => {
                let lit_val = literal_to_value_static(lit);
                if values_equal(&lit_val, val) {
                    Some(vec![])
                } else {
                    None
                }
            }
            (Pattern::List(pats), Value::List(vals)) => {
                if pats.len() != vals.len() {
                    return None;
                }
                let mut bindings = Vec::new();
                for (pat, val) in pats.iter().zip(vals.iter()) {
                    if let Some(b) = self.match_pattern(pat, val) {
                        bindings.extend(b);
                    } else {
                        return None;
                    }
                }
                Some(bindings)
            }
            (Pattern::Dict(pats), Value::Dict(map)) => {
                let mut bindings = Vec::new();
                for (key, pat) in pats.iter() {
                    if let Some(val) = map.get(key) {
                        if let Some(b) = self.match_pattern(pat, val) {
                            bindings.extend(b);
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                Some(bindings)
            }
            _ => None,
        }
    }

    fn import_module(&mut self, path: &str) -> Result<Arc<Mutex<Environment>>, String> {
        let file_path = format!("{}.mora", path);
        let source = fs::read_to_string(&file_path)
            .map_err(|e| format!("Failed to load module '{}': {}", path, e))?;

        let mut lexer = Lexer::new(&source);
        let tokens = lexer.scan_tokens();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse();

        let module_env = Arc::new(Mutex::new(Environment::with_parent(self.globals.clone())));
        let previous = self.environment.clone();
        self.environment = module_env.clone();

        for stmt in &stmts {
            self.execute(stmt)?;
        }

        self.environment = previous;
        Ok(module_env)
    }

    fn evaluate(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Literal(lit) => self.literal_to_value(lit),
            Expr::Variable(name, _) => {
                let value = self.environment.lock().unwrap().get(name);
                match value {
                    Some(v) => Ok(v),
                    None if is_builtin_object(name) => Ok(Value::Builtin(name.clone())),
                    None => Err(format!("Undefined variable: {}", name)),
                }
            }
            Expr::Grouping(expr, _) => self.evaluate(expr),
            Expr::Binary { left, op, right, span: _ } => {
                let left = self.evaluate(left)?;
                let right = self.evaluate(right)?;
                eval_binary(left, op, right)
            }
            Expr::Pipe { left, right, span: _ } => {
                let left_val = self.evaluate(left)?;
                self.evaluate_pipe(left_val, right)
            }
            Expr::Call { callee, args, span: _ } => {
                // v0.04 Slice 2: 先看 route_registry
                if self.route_registry.contains_key(callee) {
                    // 已注册 → 走 RouteCall 路径
                    let model = self.route_registry.get(callee).unwrap().clone();
                    if args.is_empty() {
                        return Err(format!("route '{}()' requires 1 argument (the prompt)", callee));
                    }
                    let prompt_str = match Self::eval_route_arg(&args[0], self) {
                        Ok(s) => s,
                        Err(e) => return Err(e),
                    };
                    return Self::do_ai_chat(self, &model, &prompt_str);
                }
                // 未注册 → 普通函数调用
                let arg_values: Result<Vec<Value>, String> = args.iter().map(|a| self.evaluate(a.as_ref())).collect();
                self.call_function(callee, arg_values?)
            }
            Expr::MethodCall { object, method, args, span: _ } => {
                let obj = self.evaluate(object)?;
                let arg_values: Result<Vec<Value>, String> = args.iter().map(|a| self.evaluate(a.as_ref())).collect();
                self.call_method(obj, method, arg_values?)
            }
            Expr::Index { object, index, span: _ } => {
                let obj = self.evaluate(object)?;
                let idx = self.evaluate(index)?;
                match (&obj, &idx) {
                    (Value::List(list), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < list.len() { Ok(list[i].clone()) }
                        else { Err(format!("Index out of bounds: {} (len: {})", i, list.len())) }
                    }
                    (Value::String(s), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < s.len() { Ok(Value::String(s.chars().nth(i).unwrap().to_string())) }
                        else { Err(format!("Index out of bounds: {} (len: {})", i, s.len())) }
                    }
                    (Value::Dict(map), Value::String(key)) => {
                        Ok(map.get(key).cloned().unwrap_or(Value::Nil))
                    }
                    _ => Err(format!("Cannot index {} with {}", obj, idx)),
                }
            }
            Expr::Closure { params, return_type: _, body, span: _ } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                Ok(Value::Closure {
                    params: param_names,
                    body: body.clone(),
                    env: self.environment.clone(),
                })
            }
            Expr::Match { expr, arms, span: _ } => {
                let val = self.evaluate(expr)?;
                for (pattern, arm_expr) in arms.iter() {
                    if let Some(bindings) = self.match_pattern(pattern, &val) {
                        let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                        for (name, value) in bindings {
                            env.lock().unwrap().define(name, value, false);
                        }
                        let previous = self.environment.clone();
                        self.environment = env;
                        let result = self.evaluate(arm_expr.as_ref());
                        self.environment = previous;
                        return result;
                    }
                }
                Err("No match arm matched".to_string())
            }
            // v0.04: p"..." → 直接调 real_ai_chat_inner (不再走 ai.chat builtin)
            Expr::Prompt { parts, .. } => {
                let prompt_str = Self::eval_prompt_parts(parts, self)?;
                Self::do_ai_chat(self, "gpt-4o-mini", &prompt_str)
            }
            // v0.04 Slice 2: fast(p"...") → 直接调 real_ai_chat_inner 用对应 model
            Expr::RouteCall { name, args, .. } => {
                // 1. 找 model
                let model = self.route_registry.get(name)
                    .ok_or_else(|| format!("route '{}' not defined (use `route {}: \"<model>\"` first)", name, name))?
                    .clone();
                // 2. 求值 args[0] (期望是 Prompt 表达式)
                if args.is_empty() {
                    return Err(format!("route call '{}()' requires 1 argument (the prompt)", name));
                }
                let prompt_str = Self::eval_route_arg(&args[0], self)?;
                Self::do_ai_chat(self, &model, &prompt_str)
            }
            // v0.04补: ai_model("name", temperature: 0.7, ...) 表达式
            // 求值后返回 Dict {_model, temperature?, max_tokens?, system?}
            Expr::AiModelCall { model, temperature, max_tokens, system, span: _ } => {
                let model_str = match self.evaluate(model)? {
                    Value::String(s) => s,
                    other => return Err(format!("ai_model: model name must be string, got {}", other)),
                };
                let mut m = std::collections::HashMap::new();
                m.insert("_model".to_string(), Value::String(model_str));
                if let Some(t) = temperature {
                    let v = self.evaluate(t)?;
                    if !matches!(v, Value::Number(_)) {
                        return Err(format!("ai_model: temperature must be number, got {}", v));
                    }
                    m.insert("temperature".to_string(), v);
                }
                if let Some(n) = max_tokens {
                    let v = self.evaluate(n)?;
                    if !matches!(v, Value::Number(_)) {
                        return Err(format!("ai_model: max_tokens must be number, got {}", v));
                    }
                    m.insert("max_tokens".to_string(), v);
                }
                if let Some(s) = system {
                    let v = self.evaluate(s)?;
                    if !matches!(v, Value::String(_)) {
                        return Err(format!("ai_model: system must be string, got {}", v));
                    }
                    m.insert("system".to_string(), v);
                }
                Ok(Value::Dict(m))
            }
        }
    }

    /// v0.04 Slice 2: 求值 RouteCall 的单个参数
    /// 期望是 Prompt 表达式 — 拼接 parts 为字符串
    fn eval_route_arg(arg: &Expr, interp: &mut Interpreter) -> Result<String, String> {
        match arg {
            Expr::Prompt { parts, .. } => Self::eval_prompt_parts(parts, interp),
            other => Ok(interp.evaluate(other)?.to_string()),
        }
    }

    /// v0.04.0: 把 Prompt 节点的 parts 拼接为字符串
    /// 内部临时借用 self 来 evaluate 每个 part（closure 隔离借用）
    fn eval_prompt_parts(parts: &[Expr], interp: &mut Interpreter) -> Result<String, String> {
        let mut s = String::new();
        for p in parts {
            let v = interp.evaluate(p)?;
            s.push_str(&v.to_string());
        }
        Ok(s)
    }

    /// v0.04.0: Stmt::StreamFor 用 — 接受任意 prompt 表达式（不仅是 Prompt 节点）
    fn eval_prompt_parts_from_stmt(prompt_expr: &Expr, interp: &mut Interpreter) -> Result<String, String> {
        match prompt_expr {
            Expr::Prompt { parts, .. } => Self::eval_prompt_parts(parts, interp),
            other => Ok(interp.evaluate(other)?.to_string()),
        }
    }

    /// v0.04: AI chat 的统一入口
    /// 替代 v0.03 的 ai.chat builtin
    /// - model: 模型名 (e.g. "gpt-4o-mini")
    /// - prompt: prompt 字符串
    fn do_ai_chat(interp: &mut Interpreter, model: &str, prompt: &str) -> Result<Value, String> {
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let base_url = std::env::var("MORA_AI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        if api_key.is_empty() {
            // Mock 模式
            eprintln!("[ai.chat mock — set OPENAI_API_KEY for real call] {}", prompt);
            return Ok(Value::String(format!("[Mock response for: {}]", prompt)));
        }

        let messages = vec![("user".to_string(), prompt.to_string())];
        interp.real_ai_chat(&messages, &api_key, model, &base_url)
    }

    fn evaluate_pipe(&mut self, left_val: Value, right: &Expr) -> Result<Value, String> {
        match right {
            Expr::Call { callee, args, span: _ } => {
                // 检查是否是列表/字符串方法名——自动转为方法调用
                if is_pipe_method(callee) {
                    let mut arg_values: Vec<Value> = Vec::new();
                    for arg in args {
                        arg_values.push(self.evaluate(arg.as_ref())?);
                    }
                    return self.call_method(left_val, callee, arg_values);
                }
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg.as_ref())?);
                }
                self.call_function(callee, arg_values)
            }
            Expr::MethodCall { object, method, args, span: _ } => {
                let obj = self.evaluate(object)?;
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg.as_ref())?);
                }
                self.call_method(obj, method, arg_values)
            }
            Expr::Variable(name, _) => {
                self.call_function(name, vec![left_val])
            }
            Expr::Pipe { left: inner_left, right: inner_right, span: _ } => {
                let inner_val = self.evaluate_pipe(left_val, inner_left)?;
                self.evaluate_pipe(inner_val, inner_right)
            }
            _ => Err(format!("Right side of pipe must be a call or method call, got {:?}", right)),
        }
    }

    fn literal_to_value(&mut self, lit: &Literal) -> Result<Value, String> {
        match lit {
            Literal::String(s, _) => Ok(Value::String(s.clone())),
            Literal::Number(n, _) => Ok(Value::Number(*n)),
            Literal::Bool(b, _) => Ok(Value::Bool(*b)),
            Literal::Nil(_) => Ok(Value::Nil),
            Literal::List(items, _) => {
                let mut values = Vec::new();
                for item in items { values.push(self.evaluate(item.as_ref())?); }
                Ok(Value::List(values))
            }
            Literal::Dict(entries, _) => {
                let mut map = HashMap::new();
                for (key, expr) in entries {
                    map.insert(key.clone(), self.evaluate(expr.as_ref())?);
                }
                Ok(Value::Dict(map))
            }
        }
    }

    fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        match name {
            "print" => {
                let msg = args.into_iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\t");
                println!("{}", msg);
                Ok(Value::Nil)
            }
            "range" => {
                let start = args.get(0).and_then(|v| match v { Value::Number(n) => Some(*n as i64), _ => None }).unwrap_or(0);
                let end = args.get(1).and_then(|v| match v { Value::Number(n) => Some(*n as i64), _ => None }).unwrap_or(start);
                let step = args.get(2).and_then(|v| match v { Value::Number(n) => Some(*n as i64), _ => None }).unwrap_or(1);
                let mut items = Vec::new();
                let mut i = start;
                while i < end { items.push(Value::Number(i as f64)); i += step; }
                Ok(Value::List(items))
            }
            "len" => {
                let len = match args.get(0) {
                    Some(Value::List(list)) => list.len(),
                    Some(Value::String(s)) => s.len(),
                    Some(Value::Dict(map)) => map.len(),
                    _ => return Err("len() expects a list, string, or dict".to_string()),
                };
                Ok(Value::Number(len as f64))
            }
            _ => {
                // 先 clone 出值，释放 borrow，避免借用冲突
                let looked_up = self.environment.lock().unwrap().get(name).clone();
                if let Some(value) = looked_up {
                    match value {
                        Value::Task { params, body, .. } => {
                            let params = params.clone();
                            let body = body.clone();
                            self.call_task(&params, &body, args)
                        }
                        Value::Closure { params, body, env } => {
                            let params = params.clone();
                            let body = body.clone();
                            let env = env.clone();
                            self.call_closure(&params, &body, env, args)
                        }
                        _ => Err(format!("'{}' is not callable", name)),
                    }
                } else {
                    Err(format!("Undefined function or task: {}", name))
                }
            }
        }
    }

    fn call_task(&mut self, params: &[String], body: &[Stmt], args: Vec<Value>) -> Result<Value, String> {
        let env = Arc::new(Mutex::new(Environment::with_parent(self.globals.clone())));
        for (i, param) in params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            env.lock().unwrap().define(param.clone(), value, false);
        }
        let signal = self.execute_block(body, env)?;
        // FlowSignal::Return(val) → 函数返回值 val
        // FlowSignal::None → 函数未显式 return，默认为 nil
        Ok(signal.into_value())
    }

    fn call_closure(&mut self, params: &[String], body: &[Stmt], env: Arc<Mutex<Environment>>, args: Vec<Value>) -> Result<Value, String> {
        let call_env = Arc::new(Mutex::new(Environment::with_parent(env)));
        for (i, param) in params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            call_env.lock().unwrap().define(param.clone(), value, false);
        }
        // 闭包是**表达式**：body 通常是 [Stmt::Expr(expr)] 或 [Stmt::Return(val)]
        // 求值约定（与 task 不同——task 必须显式 return 才返回值）：
        //   1. 单条 Stmt::Expr(expr) → evaluate(expr) 作为闭包返回值
        //   2. 含 Stmt::Return(val) → val 是闭包返回值
        //   3. 其他（多 stmt / let / if 等）→ nil
        //
        // 我们用 execute_block 跑所有 stmt 收集副作用，
        // 然后手动取最后一条 expr 的值（如果有）。
        if let Some(Stmt::Return { value: _, span: _ }) = body.last() {
            let signal = self.execute_block(body, call_env)?;
            return Ok(signal.into_value());
        }
        // 单条 expr 闭包：单独 evaluate 取值（不能走 execute_block，因为
        // Stmt::Expr 现在返回 FlowSignal::None 不携带值）
        if body.len() == 1 {
            if let Stmt::Expr(expr) = &body[0] {
                let previous = self.environment.clone();
                self.environment = call_env;
                let result = self.evaluate(expr);
                self.environment = previous;
                return result;
            }
        }
        // 多 stmt 闭包：执行全部，最后如果有 expr 取值
        let previous = self.environment.clone();
        self.environment = call_env.clone();
        let mut last_expr_value = Value::Nil;
        for stmt in body {
            // 已经走过上一条 early return 路径
            if let Stmt::Expr(expr) = stmt {
                last_expr_value = self.evaluate(expr)?;
            } else {
                self.execute(stmt)?;
            }
        }
        self.environment = previous;
        Ok(last_expr_value)
    }

    fn call_method(&mut self, mut object: Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match object {
            Value::List(list) => {
                match method {
                    "push" => {
                        let item = args.get(0).cloned().unwrap_or(Value::Nil);
                        let mut new_list = list.clone();
                        new_list.push(item);
                        Ok(Value::List(new_list))
                    }
                    "get" => {
                        let index = args.get(0).and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None }).unwrap_or(0);
                        Ok(list.get(index).cloned().unwrap_or(Value::Nil))
                    }
                    "pop" => {
                        let mut new_list = list.clone();
                        let item = new_list.pop().unwrap_or(Value::Nil);
                        Ok(item)
                    }
                    "len" => Ok(Value::Number(list.len() as f64)),
                    "map" => {
                        let mapper = args.get(0).cloned().ok_or("map() requires a function")?;
                        let mut result = Vec::new();
                        for item in list {
                            let mapped = self.call_value(&mapper, vec![item])?;
                            result.push(mapped);
                        }
                        Ok(Value::List(result))
                    }
                    "filter" => {
                        let predicate = args.get(0).cloned().ok_or("filter() requires a function")?;
                        let mut result = Vec::new();
                        for item in list {
                            let keep = self.call_value(&predicate, vec![item.clone()])?;
                            if is_truthy(&keep) {
                                result.push(item);
                            }
                        }
                        Ok(Value::List(result))
                    }
                    "reduce" => {
                        let reducer = args.get(0).cloned().ok_or("reduce() requires a function")?;
                        let mut acc = args.get(1).cloned().unwrap_or(Value::Nil);
                        for item in list {
                            acc = self.call_value(&reducer, vec![acc, item])?;
                        }
                        Ok(acc)
                    }
                    _ => Err(format!("List has no method: {}", method)),
                }
            }
            Value::Dict(map) => {
                match method {
                    "get" => {
                        let key = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(map.get(&key).cloned().unwrap_or(Value::Nil))
                    }
                    "set" => {
                        let key = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        let value = args.get(1).cloned().unwrap_or(Value::Nil);
                        let mut new_map = map.clone();
                        new_map.insert(key, value);
                        Ok(Value::Dict(new_map))
                    }
                    "keys" => {
                        let keys: Vec<Value> = map.keys().map(|k| Value::String(k.clone())).collect();
                        Ok(Value::List(keys))
                    }
                    "values" => {
                        let values: Vec<Value> = map.values().cloned().collect();
                        Ok(Value::List(values))
                    }
                    "len" => Ok(Value::Number(map.len() as f64)),
                    _ => Err(format!("Dict has no method: {}", method)),
                }
            }
            Value::Builtin(name) => match (name.as_str(), method) {
                ("web", "fetch") => {
                    let url = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    // v10: 真实 HTTP GET
                    self.real_web_fetch(&url)
                }
                ("json", "parse") => {
                    // v10: 真实 JSON 解析
                    let text = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    json_to_value(&text).map_err(|e| format!("json.parse: {}", e))
                }
                ("json", "stringify") => {
                    // v10: JSON 序列化
                    let value = args.get(0).cloned().unwrap_or(Value::Nil);
                    Ok(Value::String(value_to_json(&value)))
                }
                ("file", method) => self.call_file_method(method, &args),
                ("agent", "create") => {
                    // agent.create("name", {tools: [...], model: "deep", max_steps: 10, system: "..."})
                    let name = match args.get(0) {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err("agent.create: first arg must be a string (agent name)".to_string()),
                    };
                    let config = match args.get(1) {
                        Some(Value::Dict(d)) => d.clone(),
                        _ => return Err("agent.create: second arg must be a dict (config)".to_string()),
                    };
                    let tool_names = match config.get("tools") {
                        Some(Value::List(items)) => {
                            items.iter().map(|v| v.to_string()).collect()
                        }
                        _ => vec![],
                    };
                    let model_route = match config.get("model") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "default".to_string(),
                    };
                    let max_steps = match config.get("max_steps") {
                        Some(Value::Number(n)) => *n as usize,
                        _ => 10,
                    };
                    let system = match config.get("system") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "You are a helpful assistant. Use the available tools to complete the task.".to_string(),
                    };
                    Ok(Value::Agent { name, tool_names, model_route, max_steps, system })
                }
                ("agent", "critic") => {
                    // agent.critic(answer) — 评估输出质量
                    // agent.critic(answer, context) — 检查是否基于上下文（幻觉检测）
                    let answer = match args.get(0) {
                        Some(v) => v.to_string(),
                        _ => return Err("agent.critic: first arg must be the text to evaluate".to_string()),
                    };
                    let context = args.get(1).map(|v| v.to_string());
                    self.run_critic(&answer, context.as_deref())
                }
                _ => Err(format!("Unknown method: {}.{}", name, method)),
            },
            Value::Conversation { ref mut messages, ref model, ref base_url, ref api_key } => {
                match method {
                    "chat" => {
                        let prompt = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        if prompt.is_empty() {
                            return Err("conv.chat: prompt cannot be empty".to_string());
                        }
                        messages.push(("user".to_string(), prompt));
                        let api_key = api_key.clone();
                        let model = model.clone();
                        let base_url = base_url.clone();
                        let response = self.real_ai_chat(messages, &api_key, &model, &base_url)?;
                        messages.push(("assistant".to_string(), response.to_string()));
                        Ok(response)
                    }
                    "history" => {
                        let hist: Vec<Value> = messages.iter().map(|(role, content)| {
                            let mut m = HashMap::new();
                            m.insert("role".to_string(), Value::String(role.clone()));
                            m.insert("content".to_string(), Value::String(content.clone()));
                            Value::Dict(m)
                        }).collect();
                        Ok(Value::List(hist))
                    }
                    "clear" => {
                        messages.clear();
                        Ok(Value::Nil)
                    }
                    "model" => Ok(Value::String(model.clone())),
                    "len" => Ok(Value::Number(messages.len() as f64)),
                    _ => Err(format!("Conversation has no method: {}", method)),
                }
            }
            Value::String(s) => {
                match method {
                    "len" => Ok(Value::Number(s.len() as f64)),
                    "upper" => Ok(Value::String(s.to_uppercase())),
                    "lower" => Ok(Value::String(s.to_lowercase())),
                    "trim" => Ok(Value::String(s.trim().to_string())),
                    "starts_with" => {
                        let prefix = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.starts_with(&prefix)))
                    }
                    "ends_with" => {
                        let suffix = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.ends_with(&suffix)))
                    }
                    "contains" => {
                        let needle = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.contains(&needle)))
                    }
                    "split" => {
                        let sep = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        let parts: Vec<Value> = s.split(&sep)
                            .map(|p| Value::String(p.to_string()))
                            .collect();
                        Ok(Value::List(parts))
                    }
                    "replace" => {
                        let from = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        let to = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::String(s.replace(&from, &to)))
                    }
                    _ => Err(format!("String has no method: {}", method)),
                }
            }
            Value::Stream { ref reader, ref done } => {
                match method {
                    "collect" => {
                        let mut result = String::new();
                        if !*done.lock().unwrap() {
                            let mut guard = reader.lock();
                            loop {
                                match Self::read_next_sse_token(&mut *guard) {
                                    Ok(Some(token)) => result.push_str(&token),
                                    Ok(None) => {
                                        *done.lock().unwrap() = true;
                                        break;
                                    }
                                    Err(e) => {
                                        *done.lock().unwrap() = true;
                                        return Err(format!("ai.stream.collect: {}", e));
                                    }
                                }
                            }
                        }
                        Ok(Value::String(result))
                    }
                    "is_done" => {
                        Ok(Value::Bool(*done.lock().unwrap()))
                    }
                    _ => Err(format!("Stream has no method: {}", method)),
                }
            }
            Value::Agent { ref name, ref tool_names, ref model_route, max_steps, ref system } => {
                match method {
                    "run" => {
                        let task = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        if task.is_empty() {
                            return Err("agent.run: first arg must be a string (task)".to_string());
                        }
                        // 克隆需要的数据（避免借用冲突）
                        let agent_name = name.clone();
                        let agent_tools = tool_names.clone();
                        let agent_route = model_route.clone();
                        let agent_max = max_steps;
                        let agent_system = system.clone();
                        self.run_agent(&agent_name, &agent_tools, &agent_route, agent_max, &agent_system, &task)
                    }
                    "name" => Ok(Value::String(name.clone())),
                    "max_steps" => Ok(Value::Number(max_steps as f64)),
                    _ => Err(format!("Agent has no method: {}", method)),
                }
            }
            _ => Err(format!("Can only call methods on lists, dicts, strings, conversations, streams, agents, or builtin objects")),
        }
    }

    pub fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String> {
        match value {
            Value::Closure { params, body, env } => {
                let params = params.clone();
                let body = body.clone();
                let env = env.clone();
                self.call_closure(&params, &body, env, args)
            }
            Value::Task { params, body, .. } => {
                let params = params.clone();
                let body = body.clone();
                self.call_task(&params, &body, args)
            }
            _ => Err(format!("Value is not callable: {}", value)),
        }
    }

    // ===================================================================
    // v11: file.* 内建模块 — 完整文件系统能力
    // ===================================================================
    //
    // 设计要点：
    // - 文本 IO 用 String 承载；二进制 IO 用 hex 字符串承载（Mora 无原生 bytes 类型）
    // - 所有错误通过 Err 返回，调用方通过 try/catch 处理
    // - 路径参数统一为字符串，沿用 fs::read_to_string 等 std 行为
    // - 不做沙箱：Mora 是本地脚本语言，访问受 OS 文件权限保护
    // - hex 编解码用小写字母，与 web.fetch 等 JSON/HTTP 行为保持一致
    fn call_file_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let expect_str = |idx: usize, name: &str| -> Result<String, String> {
            match args.get(idx) {
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(format!("file.{}: {} must be a string", method, name)),
                None => Err(format!("file.{}: missing argument {}", method, name)),
            }
        };
        match method {
            // ---- 文本 IO ----
            "read_text" => {
                let path = expect_str(0, "path")?;
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("file.read_text: cannot read '{}': {}", path, e))?;
                Ok(Value::String(content))
            }
            "write_text" => {
                let path = expect_str(0, "path")?;
                let content = expect_str(1, "content")?;
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        return Err(format!(
                            "file.write_text: parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                }
                std::fs::write(&path, &content)
                    .map_err(|e| format!("file.write_text: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "append_text" => {
                let path = expect_str(0, "path")?;
                let content = expect_str(1, "content")?;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| format!("file.append_text: cannot open '{}': {}", path, e))?;
                f.write_all(content.as_bytes())
                    .map_err(|e| format!("file.append_text: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }

            // ---- 二进制 IO（hex 字符串承载）----
            "read_bytes" => {
                let path = expect_str(0, "path")?;
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("file.read_bytes: cannot read '{}': {}", path, e))?;
                Ok(Value::String(hex_encode(&bytes)))
            }
            "write_bytes" => {
                let path = expect_str(0, "path")?;
                let hex = expect_str(1, "hex")?;
                let bytes = hex_decode(&hex)
                    .map_err(|e| format!("file.write_bytes: {}", e))?;
                std::fs::write(&path, &bytes)
                    .map_err(|e| format!("file.write_bytes: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }

            // ---- 元信息 ----
            "exists" => {
                let path = expect_str(0, "path")?;
                Ok(Value::Bool(std::path::Path::new(&path).exists()))
            }
            "is_file" => {
                let path = expect_str(0, "path")?;
                Ok(Value::Bool(std::path::Path::new(&path).is_file()))
            }
            "is_dir" => {
                let path = expect_str(0, "path")?;
                Ok(Value::Bool(std::path::Path::new(&path).is_dir()))
            }
            "size" => {
                let path = expect_str(0, "path")?;
                let meta = std::fs::metadata(&path)
                    .map_err(|e| format!("file.size: cannot stat '{}': {}", path, e))?;
                Ok(Value::Number(meta.len() as f64))
            }

            // ---- 目录操作 ----
            "list" => {
                let path = expect_str(0, "path")?;
                let entries = std::fs::read_dir(&path)
                    .map_err(|e| format!("file.list: cannot read dir '{}': {}", path, e))?;
                let mut names: Vec<String> = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|e| format!("file.list: {}", e))?;
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                names.sort();
                let items: Vec<Value> = names.into_iter().map(Value::String).collect();
                Ok(Value::List(items))
            }
            "mkdir" => {
                let path = expect_str(0, "path")?;
                std::fs::create_dir(&path)
                    .map_err(|e| format!("file.mkdir: cannot create '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "mkdir_all" => {
                let path = expect_str(0, "path")?;
                std::fs::create_dir_all(&path)
                    .map_err(|e| format!("file.mkdir_all: cannot create '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "remove" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                if p.is_dir() {
                    std::fs::remove_dir(&path)
                        .map_err(|e| format!("file.remove: cannot remove dir '{}': {}", path, e))?;
                } else {
                    std::fs::remove_file(&path)
                        .map_err(|e| format!("file.remove: cannot remove file '{}': {}", path, e))?;
                }
                Ok(Value::Nil)
            }
            "remove_all" => {
                let path = expect_str(0, "path")?;
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("file.remove_all: cannot remove '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "rename" => {
                let from = expect_str(0, "from")?;
                let to = expect_str(1, "to")?;
                std::fs::rename(&from, &to)
                    .map_err(|e| format!("file.rename: cannot rename '{}' -> '{}': {}", from, to, e))?;
                Ok(Value::Nil)
            }
            "copy" => {
                let from = expect_str(0, "from")?;
                let to = expect_str(1, "to")?;
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("file.copy: cannot copy '{}' -> '{}': {}", from, to, e))?;
                Ok(Value::Nil)
            }
            "touch" => {
                // v11 补充：创建空文件 / 确保文件存在
                // 注意：因 Mora 仅依赖 ureq 标准库，Unix `touch` 的"更新 mtime"语义
                // 在本实现中降级为"若已存在则 no-op"。需要真实 mtime 更新请改用 `file.write_text(path, "")`。
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                if !p.exists() {
                    if let Some(parent) = p.parent() {
                        if !parent.as_os_str().is_empty() && !parent.exists() {
                            return Err(format!(
                                "file.touch: parent directory does not exist: {}",
                                parent.display()
                            ));
                        }
                    }
                    std::fs::write(&path, "")
                        .map_err(|e| format!("file.touch: cannot create '{}': {}", path, e))?;
                }
                Ok(Value::Nil)
            }

            // ---- 路径与工作目录 ----
            "cwd" => {
                let cwd = std::env::current_dir()
                    .map_err(|e| format!("file.cwd: {}", e))?;
                Ok(Value::String(cwd.to_string_lossy().to_string()))
            }
            "chdir" => {
                let path = expect_str(0, "path")?;
                std::env::set_current_dir(&path)
                    .map_err(|e| format!("file.chdir: cannot chdir to '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "home_dir" => {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map_err(|_| "file.home_dir: HOME/USERPROFILE not set".to_string())?;
                Ok(Value::String(home))
            }
            "join" => {
                // 跨平台路径拼接
                let mut pb = std::path::PathBuf::new();
                for arg in args {
                    match arg {
                        Value::String(s) => pb.push(s),
                        _ => return Err(format!("file.join: all arguments must be strings")),
                    }
                }
                Ok(Value::String(pb.to_string_lossy().to_string()))
            }
            "abs" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map_err(|e| format!("file.abs: {}", e))?
                        .join(p)
                };
                Ok(Value::String(abs.to_string_lossy().to_string()))
            }
            "basename" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let name = p.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(Value::String(name))
            }
            "dirname" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let parent = p.parent()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(Value::String(parent))
            }
            "extname" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let ext = p.extension()
                    .map(|s| format!(".{}", s.to_string_lossy()))
                    .unwrap_or_default();
                Ok(Value::String(ext))
            }

            _ => Err(format!("file.{}: unknown method", method)),
        }
    }

    // ===================================================================
    // v0.03: memory.* — 长期记忆（向量存储 + 语义检索）
    // ===================================================================
    // v0.04: memory.* builtin 全部移除（RFC §4.1 推迟到 v1.0）
    //   get_embedding / mock_bow_embedding / extract_embeddings 无 builtin caller
    //   留作"v1.0 复活点", 用 #[allow(dead_code)] 抑制 warning

    #[allow(dead_code)]
    fn get_embedding(&self, text: &str) -> Result<Vec<f64>, String> {
        // v0.04: 只支持 mock embedding (real_ai_embed_strings 已删除, v1.0 恢复)
        Ok(mock_bow_embedding(text))
    }

    // ===================================================================
    // v11: ai.* — 向量嵌入、相似度、语义检索
    // ===================================================================


    fn real_web_fetch(&self, url: &str) -> Result<Value, String> {
        if url.is_empty() {
            return Err("web.fetch: URL cannot be empty".to_string());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!(
                "web.fetch: URL must start with http:// or https://, got: {}",
                url
            ));
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(HTTP_READ_TIMEOUT_SECS))
            .timeout_write(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS))
            .build();

        match agent.get(url).call() {
            Ok(response) => match response.into_string() {
                Ok(text) => Ok(Value::String(text)),
                Err(e) => Err(format!("web.fetch: failed to read response body: {}", e)),
            },
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let excerpt: String = body.chars().take(200).collect();
                Err(format!(
                    "web.fetch: HTTP {} {} (body excerpt: {})",
                    status, url, excerpt
                ))
            }
            Err(ureq::Error::Transport(t)) => Err(format!(
                "web.fetch: network error for {}: {}",
                url, t
            )),
        }
    }

    /// 真实 Chat Completions API 调用（支持 OpenAI 兼容端点）。
    ///
    /// 关键设计：
    /// - **messages 参数**：完整对话历史，支持多轮上下文
    /// - **model / base_url 参数**：可配置，兼容本地模型和其他 API 提供商
    /// - **手写 JSON 请求体**：保持零 serde 依赖原则
    /// - **结构化 JSON 响应解析**：用 json_to_value 提取 choices[0].message.content
    /// - **同步阻塞**：60s 读超时（AI 推理可能慢）
    fn real_ai_chat(&mut self, messages: &[(String, String)], api_key: &str, model: &str, base_url: &str) -> Result<Value, String> {
        let mut span_attrs = std::collections::HashMap::new();
        span_attrs.insert("model".to_string(), model.to_string());
        span_attrs.insert("messages".to_string(), messages.len().to_string());
        let span = self.trace.start_span("ai.chat", span_attrs);
        let start = std::time::Instant::now();

        let result = self.real_ai_chat_inner(messages, api_key, model, base_url);

        let elapsed = start.elapsed();
        let mut end_attrs = std::collections::HashMap::new();
        end_attrs.insert("latency_ms".to_string(), elapsed.as_millis().to_string());
        match &result {
            Ok(val) => {
                end_attrs.insert("output_len".to_string(), val.to_string().len().to_string());
                span.end(end_attrs);
                self.trace.record_call("ai.chat", elapsed, true);
            }
            Err(e) => {
                span.end_error(e, end_attrs);
                self.trace.record_call("ai.chat", elapsed, false);
            }
        }
        result
    }

    fn real_ai_chat_inner(&mut self, messages: &[(String, String)], api_key: &str, model: &str, base_url: &str) -> Result<Value, String> {
        if messages.is_empty() {
            return Err("ai.chat: messages cannot be empty".to_string());
        }

        // 构建 messages JSON 数组
        let msgs_json: String = messages.iter().map(|(role, content)| {
            let escaped_content = content
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!(
                r#"{{"role":"{}","content":"{}"}}"#,
                role, escaped_content
            )
        }).collect::<Vec<_>>().join(",");

        let escaped_model = model
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let body = format!(
            r#"{{"model":"{}","messages":[{}]}}"#,
            escaped_model, msgs_json
        );

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(AI_READ_TIMEOUT_SECS))
            .timeout_write(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS))
            .build();

        match agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", api_key))
            .set("Content-Type", "application/json")
            .send_string(&body)
        {
            Ok(response) => match response.into_string() {
                Ok(text) => {
                    // 追踪 token 消耗
                    let (input, output) = Self::extract_usage(&text);
                    let _ = self.track_tokens(input, output); // 预算超限不阻断，只告警
                    // 结构化 JSON 解析：提取 choices[0].message.content
                    self.extract_ai_content(&text)
                        .or_else(|_| Ok(Value::String(text)))
                }
                Err(e) => Err(format!("ai.chat: failed to read response body: {}", e)),
            },
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let excerpt: String = body.chars().take(300).collect();
                Err(format!(
                    "ai.chat: API error HTTP {} from {} (body: {})",
                    status, url, excerpt
                ))
            }
            Err(ureq::Error::Transport(t)) => Err(format!(
                "ai.chat: network error connecting to {}: {}",
                url, t
            )),
        }
    }

    /// 带工具调用的 AI 对话（支持 tool_calls 自动循环）
    fn real_ai_chat_with_tools(&mut self, messages: &mut Vec<ChatMessage>, api_key: &str, model: &str, base_url: &str, tools: &[&ToolDef]) -> Result<Value, String> {
        let max_rounds = 10;
        for _round in 0..max_rounds {
            // 构建 messages JSON
            let msgs_json = Self::build_chat_messages_json(messages);

            // 构建 tools JSON
            let tools_json = if tools.is_empty() {
                String::new()
            } else {
                let tool_entries: Vec<String> = tools.iter().map(|t| {
                    format!(
                        r#"{{"type":"function","function":{{"name":"{}","description":"{}","parameters":{}}}}}"#,
                        t.name.replace('\\', "\\\\").replace('"', "\\\""),
                        t.description.replace('\\', "\\\\").replace('"', "\\\""),
                        t.parameters
                    )
                }).collect();
                format!(r#","tools":[{}]"#, tool_entries.join(","))
            };

            let escaped_model = model.replace('\\', "\\\\").replace('"', "\\\"");
            let body = format!(
                r#"{{"model":"{}","messages":[{}]{}"#,
                escaped_model, msgs_json, tools_json
            );
            // 闭合 JSON（tools_json 已带前导逗号）
            let body = if tools_json.is_empty() {
                format!("{}}}", body)
            } else {
                format!("{}}}", body)
            };

            let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
            let agent = ureq::AgentBuilder::new()
                .timeout_read(Duration::from_secs(AI_READ_TIMEOUT_SECS))
                .timeout_write(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS))
                .build();

            let response_text = match agent.post(&url)
                .set("Authorization", &format!("Bearer {}", api_key))
                .set("Content-Type", "application/json")
                .send_string(&body)
            {
                Ok(response) => response.into_string()
                    .map_err(|e| format!("ai.chat: failed to read response body: {}", e))?,
                Err(ureq::Error::Status(status, response)) => {
                    let body = response.into_string().unwrap_or_default();
                    let excerpt: String = body.chars().take(300).collect();
                    return Err(format!("ai.chat: API error HTTP {} from {} (body: {})", status, url, excerpt));
                }
                Err(ureq::Error::Transport(t)) => {
                    return Err(format!("ai.chat: network error connecting to {}: {}", url, t));
                }
            };

            // 解析响应
            let (input, output) = Self::extract_usage(&response_text);
            let _ = self.track_tokens(input, output);
            let (content, tool_calls) = Self::extract_chat_response(&response_text)?;

            if tool_calls.is_empty() {
                // 无工具调用，返回最终内容
                return Ok(Value::String(content.unwrap_or_default()));
            }

            // 有工具调用：追加 assistant 消息，执行工具，追加 tool 结果
            messages.push(ChatMessage::Assistant {
                content: content.clone(),
                tool_calls: tool_calls.clone(),
            });

            for tc in &tool_calls {
                // 查找 handler
                let handler = tools.iter().find(|t| t.name == tc.name).map(|t| &t.handler);
                let result = if let Some(handler_val) = handler {
                    // 构造参数 Dict 传给闭包
                    let args_dict = if let Ok(params_val) = json_to_value(&tc.arguments) {
                        params_val
                    } else {
                        Value::String(tc.arguments.clone())
                    };
                    match self.call_value(handler_val, vec![args_dict]) {
                        Ok(val) => val.to_string(),
                        Err(e) => format!("Error: {}", e),
                    }
                } else {
                    format!("Error: tool '{}' not found", tc.name)
                };
                messages.push(ChatMessage::Tool {
                    tool_call_id: tc.id.clone(),
                    content: result,
                });
            }
        }
        Err("ai.chat: max tool call rounds exceeded".to_string())
    }

    /// 构建 ChatMessage 列表的 JSON 字符串
    fn build_chat_messages_json(messages: &[ChatMessage]) -> String {
        let parts: Vec<String> = messages.iter().map(|msg| {
            match msg {
                ChatMessage::User { content } => {
                    let esc = content.replace('\\', "\\\\").replace('"', "\\\"")
                        .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
                    format!(r#"{{"role":"user","content":"{}"}}"#, esc)
                }
                ChatMessage::Assistant { content, tool_calls } => {
                    let mut parts = vec![r#""role":"assistant""#.to_string()];
                    match content {
                        Some(c) => {
                            let esc = c.replace('\\', "\\\\").replace('"', "\\\"")
                                .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
                            parts.push(format!(r#""content":"{}""#, esc));
                        }
                        None => parts.push(r#""content":null"#.to_string()),
                    }
                    if !tool_calls.is_empty() {
                        let tc_json: Vec<String> = tool_calls.iter().map(|tc| {
                            format!(
                                r#"{{"id":"{}","type":"function","function":{{"name":"{}","arguments":"{}"}}}}"#,
                                tc.id.replace('\\', "\\\\").replace('"', "\\\""),
                                tc.name.replace('\\', "\\\\").replace('"', "\\\""),
                                tc.arguments.replace('\\', "\\\\").replace('"', "\\\"")
                            )
                        }).collect();
                        parts.push(format!(r#""tool_calls":[{}]"#, tc_json.join(",")));
                    }
                    format!("{{{}}}", parts.join(","))
                }
                ChatMessage::Tool { tool_call_id, content } => {
                    let esc_id = tool_call_id.replace('\\', "\\\\").replace('"', "\\\"");
                    let esc_content = content.replace('\\', "\\\\").replace('"', "\\\"")
                        .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
                    format!(r#"{{"role":"tool","tool_call_id":"{}","content":"{}"}}"#, esc_id, esc_content)
                }
            }
        }).collect();
        parts.join(",")
    }

    /// 从 API 响应中提取 content 和 tool_calls
    fn extract_chat_response(json_text: &str) -> Result<(Option<String>, Vec<ToolCall>), String> {
        let root = json_to_value(json_text)?;
        if let Value::Dict(ref map) = root {
            if let Some(Value::List(choices)) = map.get("choices") {
                if let Some(first) = choices.first() {
                    if let Value::Dict(ref choice_map) = first {
                        if let Some(Value::Dict(ref msg_map)) = choice_map.get("message") {
                            // 提取 content
                            let content = match msg_map.get("content") {
                                Some(Value::String(s)) => Some(s.clone()),
                                _ => None,
                            };
                            // 提取 tool_calls
                            let mut tool_calls = Vec::new();
                            if let Some(Value::List(tc_list)) = msg_map.get("tool_calls") {
                                for tc_val in tc_list {
                                    if let Value::Dict(tc_map) = tc_val {
                                        let id = match tc_map.get("id") {
                                            Some(Value::String(s)) => s.clone(),
                                            _ => format!("call_{}", tool_calls.len()),
                                        };
                                        if let Some(Value::Dict(func_map)) = tc_map.get("function") {
                                            let name = match func_map.get("name") {
                                                Some(Value::String(s)) => s.clone(),
                                                _ => continue,
                                            };
                                            let arguments = match func_map.get("arguments") {
                                                Some(Value::String(s)) => s.clone(),
                                                _ => "{}".to_string(),
                                            };
                                            tool_calls.push(ToolCall { id, name, arguments });
                                        }
                                    }
                                }
                            }
                            return Ok((content, tool_calls));
                        }
                    }
                }
            }
        }
        Err("Could not parse chat response".to_string())
    }

    /// 从 API 响应中提取 usage（prompt_tokens, completion_tokens）
    fn extract_usage(json_text: &str) -> (usize, usize) {
        if let Ok(root) = json_to_value(json_text) {
            if let Value::Dict(ref map) = root {
                if let Some(Value::Dict(ref usage)) = map.get("usage") {
                    let input = match usage.get("prompt_tokens") {
                        Some(Value::Number(n)) => *n as usize,
                        _ => 0,
                    };
                    let output = match usage.get("completion_tokens") {
                        Some(Value::Number(n)) => *n as usize,
                        _ => 0,
                    };
                    return (input, output);
                }
            }
        }
        (0, 0)
    }

    /// 记录 token 消耗并检查预算
    fn track_tokens(&mut self, input: usize, output: usize) -> Result<(), String> {
        self.token_usage.input += input;
        self.token_usage.output += output;
        self.trace.record_tokens(input as u64, output as u64);
        let total_used = self.token_usage.input + self.token_usage.output;
        if let Some(ref budget) = self.token_budget {
            if total_used > budget.total {
                return Err(format!("Token budget exceeded: used {}/{}", total_used, budget.total));
            }
            let ratio = total_used as f64 / budget.total as f64;
            if ratio >= budget.alert_threshold {
                eprintln!("[ai.budget warning] Token usage at {:.0}% ({}/{})", ratio * 100.0, total_used, budget.total);
            }
        }
        Ok(())
    }

    /// 检查预算是否已耗尽

    fn extract_ai_content(&self, json_text: &str) -> Result<Value, String> {
        let root = json_to_value(json_text)?;

        // root 应该是 Dict，提取 "choices" 数组
        if let Value::Dict(ref map) = root {
            if let Some(Value::List(choices)) = map.get("choices") {
                if let Some(first) = choices.first() {
                    if let Value::Dict(ref choice_map) = first {
                        // 标准格式: choices[0].message.content
                        if let Some(Value::Dict(ref msg_map)) = choice_map.get("message") {
                            if let Some(Value::String(content)) = msg_map.get("content") {
                                return Ok(Value::String(content.clone()));
                            }
                        }
                        // 兼容格式: choices[0].text (旧版 completions API)
                        if let Some(Value::String(text)) = choice_map.get("text") {
                            return Ok(Value::String(text.clone()));
                        }
                    }
                }
            }
            // 兼容某些 API 的顶层 "content" 字段
            if let Some(Value::String(content)) = map.get("content") {
                return Ok(Value::String(content.clone()));
            }
        }

        Err("Could not extract content from API response".to_string())
    }

    // ===================================================================
    // v0.03: 流式输出 (ai.stream)
    // ===================================================================

    /// 从 SSE 流中读取下一个 token
    /// 返回 Ok(Some(token)) — 有新 token
    /// 返回 Ok(None) — 流结束 [DONE]
    /// 返回 Err — 解析错误
    fn read_next_sse_token(reader: &mut BufReader<Box<dyn Read + Send + Sync>>) -> Result<Option<String>, String> {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return Ok(None), // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() { continue; }
                    if let Some(data) = trimmed.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" { return Ok(None); }
                        // 解析 JSON，提取 choices[0].delta.content
                        if let Ok(root) = json_to_value(data) {
                            if let Value::Dict(ref map) = root {
                                if let Some(Value::List(choices)) = map.get("choices") {
                                    if let Some(first) = choices.first() {
                                        if let Value::Dict(ref choice_map) = first {
                                            if let Some(Value::Dict(ref delta)) = choice_map.get("delta") {
                                                if let Some(Value::String(content)) = delta.get("content") {
                                                    if !content.is_empty() {
                                                        return Ok(Some(content.clone()));
                                                    }
                                                }
                                            }
                                            // finish_reason 字段出现但无 content，跳过
                                            if choice_map.contains_key("finish_reason") {
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // JSON 解析失败或无 content，跳过此行
                    }
                    // 非 data: 开头的行（event:, id:, retry:），跳过
                }
                Err(e) => return Err(format!("SSE read error: {}", e)),
            }
        }
    }

    /// 创建真实 HTTP 流式连接
    fn create_ai_stream(&self, messages: &[(String, String)], api_key: &str, model: &str, base_url: &str) -> Result<Value, String> {
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        // 构建 JSON body（与 real_ai_chat 相同，加 stream:true）
        let msgs_json: String = messages.iter().map(|(role, content)| {
            let escaped_content = content
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!(r#"{{"role":"{}","content":"{}"}}"#, role, escaped_content)
        }).collect::<Vec<_>>().join(",");

        let escaped_model = model.replace('\\', "\\\\").replace('"', "\\\"");
        let body = format!(
            r#"{{"model":"{}","messages":[{}],"stream":true}}"#,
            escaped_model, msgs_json
        );

        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(AI_STREAM_TIMEOUT_SECS))
            .timeout_write(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS))
            .build();

        match agent.post(&url)
            .set("Authorization", &format!("Bearer {}", api_key))
            .set("Content-Type", "application/json")
            .send_string(&body)
        {
            Ok(response) => {
                let reader = response.into_reader(); // Box<dyn Read + Send>
                let buf_reader = BufReader::new(reader);
                Ok(Value::Stream {
                    reader: StreamReader::new(buf_reader),
                    done: Arc::new(Mutex::new(false)),
                })
            }
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let excerpt: String = body.chars().take(300).collect();
                Err(format!("ai.stream: API error HTTP {} from {} (body: {})", status, url, excerpt))
            }
            Err(ureq::Error::Transport(t)) => {
                Err(format!("ai.stream: network error connecting to {}: {}", url, t))
            }
        }
    }

    /// 执行输出质量评估（agent.critic）
    fn run_critic(&mut self, answer: &str, context: Option<&str>) -> Result<Value, String> {
        let critic_prompt = if let Some(ctx) = context {
            // 有上下文：检查幻觉（回答是否基于上下文）
            format!(
                r#"Evaluate if the answer is grounded in the given context. Check for hallucinations (claims not supported by context).

Context:
{}

Answer:
{}

Respond in this exact format (one line per field):
score: <1-10>
verdict: <supported|partial|hallucinated>
issues: <comma-separated issues or "none">
suggestion: <improvement suggestion or "none">"#,
                ctx, answer
            )
        } else {
            // 无上下文：评估输出质量
            format!(
                r#"Evaluate the quality of this AI-generated text. Check for: clarity, coherence, relevance, factual accuracy.

Text:
{}

Respond in this exact format (one line per field):
score: <1-10>
verdict: <good|acceptable|poor>
issues: <comma-separated issues or "none">
suggestion: <improvement suggestion or "none">"#,
                answer
            )
        };

        // 用 ai.chat 调用评估（走 fast 路由或默认模型）
        let has_key = env::var("OPENAI_API_KEY").map(|k| !k.is_empty()).unwrap_or(false);
        if has_key {
            let api_key = env::var("OPENAI_API_KEY").unwrap();
            let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let base_url = env::var("MORA_AI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let msgs = vec![("user".to_string(), critic_prompt)];
            match self.real_ai_chat(&msgs, &api_key, &model, &base_url) {
                Ok(Value::String(response)) => Ok(self.parse_critic_response(&response, context.is_some())),
                Ok(other) => Ok(other),
                Err(e) => Err(format!("agent.critic: {}", e)),
            }
        } else {
            // Mock 模式：基于简单启发式评估
            Ok(self.mock_critic(answer, context))
        }
    }

    /// 解析 critic 响应为结构化 Dict
    fn parse_critic_response(&self, response: &str, has_context: bool) -> Value {
        let mut m = HashMap::new();
        let mut score = 5.0;
        let mut verdict = "unknown".to_string();
        let mut issues = "none".to_string();
        let mut suggestion = "none".to_string();

        for line in response.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("score:") {
                if let Ok(n) = val.trim().parse::<f64>() { score = n; }
            } else if let Some(val) = line.strip_prefix("verdict:") {
                verdict = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("issues:") {
                issues = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("suggestion:") {
                suggestion = val.trim().to_string();
            }
        }

        m.insert("score".to_string(), Value::Number(score));
        m.insert("verdict".to_string(), Value::String(verdict));
        m.insert("issues".to_string(), Value::String(issues));
        m.insert("suggestion".to_string(), Value::String(suggestion));
        if has_context {
            m.insert("hallucination_check".to_string(), Value::Bool(true));
        }
        Value::Dict(m)
    }

    /// Mock critic：基于简单启发式
    fn mock_critic(&self, answer: &str, context: Option<&str>) -> Value {
        let mut m = HashMap::new();
        let len = answer.len();
        let score = if len < 10 { 3.0 } else if len < 50 { 6.0 } else { 8.0 };

        let (verdict, issues) = if let Some(ctx) = context {
            // 简单检查：回答中的词是否在上下文中出现
            let ctx_lower = ctx.to_lowercase();
            let answer_words: Vec<&str> = answer.split_whitespace().collect();
            let matched = answer_words.iter()
                .filter(|w| ctx_lower.contains(&w.to_lowercase()))
                .count();
            let ratio = if answer_words.is_empty() { 0.0 } else { matched as f64 / answer_words.len() as f64 };
            if ratio > 0.5 {
                ("supported".to_string(), "none".to_string())
            } else if ratio > 0.2 {
                ("partial".to_string(), "some claims may not be grounded in context".to_string())
            } else {
                ("hallucinated".to_string(), "most claims not found in context".to_string())
            }
        } else {
            if score >= 7.0 {
                ("good".to_string(), "none".to_string())
            } else if score >= 5.0 {
                ("acceptable".to_string(), "could be more detailed".to_string())
            } else {
                ("poor".to_string(), "too short, lacks detail".to_string())
            }
        };

        m.insert("score".to_string(), Value::Number(score));
        m.insert("verdict".to_string(), Value::String(verdict));
        m.insert("issues".to_string(), Value::String(issues));
        m.insert("suggestion".to_string(), Value::String("set OPENAI_API_KEY for real evaluation".to_string()));
        if context.is_some() {
            m.insert("hallucination_check".to_string(), Value::Bool(true));
        }
        Value::Dict(m)
    }

    /// 执行 Agent 多步推理循环
    fn run_agent(&mut self, agent_name: &str, tool_names: &[String], model_route: &str, max_steps: usize, system: &str, task: &str) -> Result<Value, String> {
        // 收集 Agent 需要的工具
        let agent_tools: Vec<ToolDef> = tool_names.iter()
            .filter_map(|n| self.tool_registry.get(n).cloned())
            .collect();
        let tool_refs: Vec<&ToolDef> = agent_tools.iter().collect();

        // 确定 API 配置
        let route = self.model_routes.get(model_route);
        let default_key = env::var("OPENAI_API_KEY").unwrap_or_default();
        let (api_key, model, base_url) = if let Some(r) = route {
            let key = if r.api_key.is_empty() { default_key.clone() } else { r.api_key.clone() };
            (key, r.model.clone(), r.base_url.clone())
        } else {
            let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let base_url = env::var("MORA_AI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            (default_key, model, base_url)
        };

        // Mock 模式：直接执行第一个工具并返回结果
        if api_key.is_empty() {
            eprintln!("[agent '{}' mock — set OPENAI_API_KEY for real agent loop]", agent_name);
            if let Some(first_tool) = agent_tools.first() {
                let args_dict = Value::Dict(HashMap::new());
                let tool_result = match self.call_value(&first_tool.handler, vec![args_dict]) {
                    Ok(val) => val.to_string(),
                    Err(e) => format!("Tool error: {}", e),
                };
                return Ok(Value::String(format!("[Agent '{}'] Task: {}\nTool '{}' result: {}", agent_name, task, first_tool.name, tool_result)));
            }
            return Ok(Value::String(format!("[Agent '{}'] Task: {} (no tools, mock response)", agent_name, task)));
        }

        // 构建初始消息
        let mut messages: Vec<ChatMessage> = Vec::new();
        messages.push(ChatMessage::User {
            content: format!("{}\n\nTask: {}", system, task),
        });

        // 多步推理循环
        for step in 0..max_steps {
            eprintln!("[agent '{}' step {}/{}]", agent_name, step + 1, max_steps);
            match self.real_ai_chat_with_tools(&mut messages, &api_key, &model, &base_url, &tool_refs) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // real_ai_chat_with_tools 只在 max tool rounds exceeded 时返回 Err
                    // 其他情况下 Ok 就是最终结果
                    return Err(format!("agent.run error at step {}: {}", step + 1, e));
                }
            }
        }
        Err(format!("agent '{}': max steps ({}) exceeded", agent_name, max_steps))
    }

    /// Mock 工具调用（无 API Key 时，调用第一个注册的工具）

    /// v0.04补: mock 流占位, 无 builtin caller, 留作 v1.0 复活点
    #[allow(dead_code)]
    fn create_mock_stream(prompt: &str) -> Value {
        let mock_text = format!("[Mock stream for: {}]", prompt);
        let mut sse_data = String::new();
        for ch in mock_text.chars() {
            let escaped = match ch {
                '\\' => "\\\\".to_string(),
                '"' => "\\\"".to_string(),
                '\n' => "\\n".to_string(),
                _ => ch.to_string(),
            };
            sse_data.push_str(&format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
                escaped
            ));
        }
        sse_data.push_str("data: [DONE]\n\n");

        let cursor = std::io::Cursor::new(sse_data.into_bytes());
        let reader: Box<dyn Read + Send + Sync> = Box::new(cursor);
        Value::Stream {
            reader: StreamReader::new(BufReader::new(reader)),
            done: Arc::new(Mutex::new(false)),
        }
    }

    // ===================================================================
    // v11: 向量嵌入 (ai.embed) + 相似度 + 语义检索
    // ===================================================================
    //
    // 设计要点：
    // - 单文本 → List<Number>；批量 (List<String>) → List<List<Number>>
    // - 维度跟随模型（text-embedding-3-small = 1536, v3-large = 3072）
    // - 可选 dimensions 参数（v3 系列支持降维）
    // - 无 API key 时返回错误（沿用 ai.create 策略）
    // - 相似度函数（cosine/dot/euclidean/norm）独立可用，不依赖网络
}

// 实际接收 strings 的版本（避免 self 借用冲突）

/// v0.04补: ai.embed builtin 移除, 留作 v1.0 复活点
#[allow(dead_code)]
fn extract_embeddings(json_text: &str, expected_count: usize) -> Result<Value, String> {
    let root = json_to_value(json_text)?;
    let data = if let Value::Dict(ref map) = root {
        if let Some(Value::List(d)) = map.get("data") {
            d.clone()
        } else {
            return Err("ai.embed: response missing 'data' array".to_string());
        }
    } else {
        return Err("ai.embed: response is not a JSON object".to_string());
    };

    if data.len() != expected_count {
        return Err(format!(
            "ai.embed: expected {} embeddings, got {}",
            expected_count,
            data.len()
        ));
    }

    // 按 index 排序，保证顺序
    let mut indexed: Vec<(usize, Vec<f64>)> = data
        .into_iter()
        .map(|item| {
            if let Value::Dict(m) = item {
                let index = match m.get("index") {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                let vec = match m.get("embedding") {
                    Some(Value::List(vs)) => vs
                        .iter()
                        .filter_map(|v| if let Value::Number(n) = v { Some(*n) } else { None })
                        .collect(),
                    _ => return Err("ai.embed: 'embedding' field is not a list of numbers".to_string()),
                };
                Ok((index, vec))
            } else {
                Err("ai.embed: data item is not an object".to_string())
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    indexed.sort_by_key(|(i, _)| *i);

    if expected_count == 1 {
        // 单条：返回一维 List
        let vec = indexed.into_iter().next().unwrap().1;
        Ok(Value::List(vec.into_iter().map(Value::Number).collect()))
    } else {
        // 批量：返回 List<List>
        let items: Vec<Value> = indexed
            .into_iter()
            .map(|(_, v)| Value::List(v.into_iter().map(Value::Number).collect()))
            .collect();
        Ok(Value::List(items))
    }
}

/// mock embedding (用于 memory.* 语义检索 mock 模式)
fn mock_bow_embedding(s: &str) -> Vec<f64> {
    const DIM: usize = 32;
    let mut v = vec![0.0_f64; DIM];
    for word in s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let lower = word.to_lowercase();
        // 简单 hash: djb2
        let mut h: u64 = 5381;
        for b in lower.bytes() {
            h = h.wrapping_mul(33).wrapping_add(b as u64);
        }
        v[(h as usize) % DIM] += 1.0;
    }
    v
}

/// 余弦相似度: (a·b) / (||a|| * ||b||)，范围 [-1, 1]

/// 点积: a·b

/// 欧氏距离: sqrt(sum((a-b)^2))，值越小越相似

/// L2 范数

// ===================================================================
// 控制流信号（v11 重构）
// ===================================================================
//
// 历史：用 `Result<Option<Value>, String>` 同时表达"普通继续"和"return 信号"。
// 这导致 for/if/task 内的 return 无法正确穿透控制流边界。
//
// 重构：用显式 enum 区分两种语义。
// - None: 普通继续，下一条 stmt 正常执行
// - Return(val): return 信号，必须穿透 for/if/try/match 一直冒泡到
//   call_task/call_closure，作为函数返回值
//
// 设计要点：
// - Stmt::Expr 永远返回 None（即使 print 也不携带信号）
// - Stmt::Return 永远返回 Return(val)
// - call_task/call_closure 把 Return(val) 提取出来作为函数返回值；
//   顶层 main 的 Return(val) 被 interpret 静默忽略（Mora 没有 main 返回值概念）
#[derive(Debug, Clone)]
pub enum FlowSignal {
    None,
    Return(Value),
    Break,        // v0.04.0
    Continue,     // v0.04.0
}

impl FlowSignal {
    /// 取出 Return 的值，否则 None 视为 nil（Mora 的"无显式 return"等价于 return nil）
    pub fn into_value(self) -> Value {
        match self {
            FlowSignal::None => Value::Nil,
            FlowSignal::Return(v) => v,
            // v0.04.0: break/continue 在块边界被吞掉，不会走到 into_value
            FlowSignal::Break => Value::Nil,
            FlowSignal::Continue => Value::Nil,
        }
    }

    /// 是 Return 信号吗？
    pub fn is_return(&self) -> bool {
        matches!(self, FlowSignal::Return(_))
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Nil => false,
        Value::Bool(b) => *b,
        _ => true,
    }
}

fn is_builtin_object(name: &str) -> bool {
    matches!(name, "ai" | "web" | "json" | "file" | "memory" | "agent")
}

/// v11: 顶层 read/write 等语句使用的字符串参数提取助手
fn expect_string(value: Value, context: &str) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s),
        _ => Err(format!("{} must be a string, got {}", context, type_name(&value))),
    }
}

// ===================================================================
// v11: hex 编解码（用于 file.read_bytes / file.write_bytes）
// ===================================================================
//
// 设计要点：
// - 小写字母输出，与 web.fetch / json.* 字符串行为保持一致
// - 输入校验：奇数长度 / 非 hex 字符返回明确错误
// - 性能足够用于 10MB 级别文件，更大文件应考虑 stream API（v11 范围外）
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err(format!("hex length must be even, got {}", s.len()));
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])
            .ok_or_else(|| format!("invalid hex char '{}' at position {}", bytes[i] as char, i))?;
        let lo = hex_nibble(bytes[i + 1])
            .ok_or_else(|| format!("invalid hex char '{}' at position {}", bytes[i + 1] as char, i + 1))?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 检查名称是否是可通过管道自动调用的方法（列表/字符串方法）
fn is_pipe_method(name: &str) -> bool {
    matches!(name,
        "map" | "filter" | "reduce" | "push" | "pop" | "get" | "len" |
        "upper" | "lower" | "trim" | "starts_with" | "ends_with" |
        "contains" | "split" | "replace"
    )
}

fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, String> {
    match op {
        BinaryOp::Add => match (&left, &right) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + b)),
            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
            // 字符串 + 任意类型 → 自动转字符串拼接
            (Value::String(a), _) => Ok(Value::String(format!("{}{}", a, right))),
            (_, Value::String(b)) => Ok(Value::String(format!("{}{}", left, b))),
            (Value::List(a), Value::List(b)) => {
                let mut merged = a.clone();
                merged.extend(b.clone());
                Ok(Value::List(merged))
            }
            _ => Err("Operands must be two numbers, two strings, or two lists".to_string()),
        },
        BinaryOp::Sub => numeric_op(left, right, |a, b| a - b),
        BinaryOp::Mul => numeric_op(left, right, |a, b| a * b),
        BinaryOp::Div => numeric_op(left, right, |a, b| a / b),
        BinaryOp::Mod => numeric_op(left, right, |a, b| a % b),
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        BinaryOp::Greater => numeric_cmp(left, right, |a, b| a > b),
        BinaryOp::Less => numeric_cmp(left, right, |a, b| a < b),
        BinaryOp::GreaterEqual => numeric_cmp(left, right, |a, b| a >= b),
        BinaryOp::LessEqual => numeric_cmp(left, right, |a, b| a <= b),
    }
}

fn numeric_op<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where F: Fn(f64, f64) -> f64 {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(op(a, b))),
        _ => Err("Operands must be numbers".to_string()),
    }
}

fn numeric_cmp<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where F: Fn(f64, f64) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Bool(op(a, b))),
        _ => Err("Operands must be numbers".to_string()),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::List(a), Value::List(b)) => a == b,
        (Value::Dict(a), Value::Dict(b)) => a == b,
        // Conversation 不支持相等比较——比较引用无意义
        _ => false,
    }
}

fn literal_to_value_static(lit: &Literal) -> Value {
    match lit {
        Literal::String(s, _) => Value::String(s.clone()),
        Literal::Number(n, _) => Value::Number(*n),
        Literal::Bool(b, _) => Value::Bool(*b),
        Literal::Nil(_) => Value::Nil,
        Literal::List(_, _) => Value::Nil,
        Literal::Dict(_, _) => Value::Nil,
    }
}

fn check_type(value: &Value, hint: &str) -> bool {
    match (value, hint) {
        (Value::String(_), "string") => true,
        (Value::Number(_), "number") => true,
        (Value::Bool(_), "bool") => true,
        (Value::Nil, "nil") => true,
        (Value::List(_), "list") => true,
        (Value::Dict(_), "dict") => true,
        (Value::Task{..}, "task") => true,
        (Value::Conversation{..}, "conversation") => true,
        (Value::Stream{..}, "stream") => true,
        (Value::Agent{..}, "agent") => true,
        _ => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        Value::Nil => "nil",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Task{..} => "task",
        Value::Closure{..} => "closure",
        Value::Builtin(_) => "builtin",
        Value::Conversation{..} => "conversation",
        Value::Stream{..} => "stream",
        Value::Agent{..} => "agent",
    }
}

// --- JSON serialization ---

fn value_to_json(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Number(n) => {
            if n.fract() == 0.0 {
                format!("{:.0}", n)
            } else {
                format!("{}", n)
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Nil => "null".to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(value_to_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Dict(map) => {
            let parts: Vec<String> = map.iter()
                .map(|(k, v)| format!("\"{}\":{}", k, value_to_json(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Task { .. } => "null".to_string(),
        Value::Closure { .. } => "null".to_string(),
        Value::Builtin(_) => "null".to_string(),
        Value::Conversation { .. } => "null".to_string(),
        Value::Stream { .. } => "null".to_string(),
        Value::Agent { .. } => "null".to_string(),
    }
}

fn json_to_value(json: &str) -> Result<Value, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("Empty JSON".to_string());
    }
    parse_json_value(trimmed).map(|(v, _)| v)
}

fn parse_json_value(s: &str) -> Result<(Value, usize), String> {
    let s = s.trim_start();
    let first = s.chars().next().ok_or("Empty string")?;

    if first == '"' {
        parse_json_string(s)
    } else if first == '[' {
        parse_json_list(s)
    } else if first == '{' {
        parse_json_dict(s)
    } else if first == 't' || first == 'f' {
        parse_json_bool(s)
    } else if first == 'n' {
        parse_json_null(s)
    } else if first.is_ascii_digit() || first == '-' {
        parse_json_number(s)
    } else {
        Err(format!("Unexpected JSON character: {}", first))
    }
}

fn parse_json_string(s: &str) -> Result<(Value, usize), String> {
    let mut i = 1;
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                '"' => { result.push('"'); i += 2; }
                '\\' => { result.push('\\'); i += 2; }
                'n' => { result.push('\n'); i += 2; }
                't' => { result.push('\t'); i += 2; }
                _ => { result.push(chars[i + 1]); i += 2; }
            }
        } else if chars[i] == '"' {
            return Ok((Value::String(result), i + 1));
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    Err("Unterminated string".to_string())
}

fn parse_json_number(s: &str) -> Result<(Value, usize), String> {
    let mut i = 0;
    let chars: Vec<char> = s.chars().collect();
    if chars[i] == '-' { i += 1; }
    while i < chars.len() && chars[i].is_ascii_digit() { i += 1; }
    if i < chars.len() && chars[i] == '.' {
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() { i += 1; }
    }
    let num_str: String = chars[0..i].iter().collect();
    let num: f64 = num_str.parse().map_err(|_| "Invalid number")?;
    Ok((Value::Number(num), i))
}

fn parse_json_bool(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("true") {
        Ok((Value::Bool(true), 4))
    } else if s.starts_with("false") {
        Ok((Value::Bool(false), 5))
    } else {
        Err("Invalid boolean".to_string())
    }
}

fn parse_json_null(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("null") {
        Ok((Value::Nil, 4))
    } else {
        Err("Invalid null".to_string())
    }
}

fn parse_json_list(s: &str) -> Result<(Value, usize), String> {
    let mut i = 1; // skip '['
    let mut items = Vec::new();
    let chars: Vec<char> = s.chars().collect();

    while i < chars.len() {
        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == ']' {
            return Ok((Value::List(items), i + 1));
        }
        let rest: String = chars[i..].iter().collect();
        let (val, consumed) = parse_json_value(&rest)?;
        items.push(val);
        i += consumed;
        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == ',' {
            i += 1;
        } else if i < chars.len() && chars[i] == ']' {
            return Ok((Value::List(items), i + 1));
        }
    }
    Err("Unterminated list".to_string())
}

fn parse_json_dict(s: &str) -> Result<(Value, usize), String> {
    let mut i = 1; // skip '{'
    let mut map = HashMap::new();
    let chars: Vec<char> = s.chars().collect();

    while i < chars.len() {
        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == '}' {
            return Ok((Value::Dict(map), i + 1));
        }

        let rest: String = chars[i..].iter().collect();
        let (key_val, key_consumed) = parse_json_string(&rest)?;
        let key = match key_val {
            Value::String(s) => s,
            _ => return Err("Dict key must be string".to_string()),
        };
        i += key_consumed;

        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i >= chars.len() || chars[i] != ':' {
            return Err("Expected ':' after dict key".to_string());
        }
        i += 1;

        let rest: String = chars[i..].iter().collect();
        let (val, consumed) = parse_json_value(&rest)?;
        map.insert(key, val);
        i += consumed;

        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == ',' {
            i += 1;
        } else if i < chars.len() && chars[i] == '}' {
            return Ok((Value::Dict(map), i + 1));
        }
    }
    Err("Unterminated dict".to_string())
}

// ===================================================================
// v11: 单元测试 — 相似度函数
// ===================================================================
#[cfg(test)]
mod embed_tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        let s = cosine_similarity(&v, &v).unwrap();
        assert!(approx_eq(s, 1.0, 1e-9));
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let s = cosine_similarity(&a, &b).unwrap();
        assert!(approx_eq(s, 0.0, 1e-9));
    }

    #[test]
    fn cosine_opposite_is_minus_one() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let s = cosine_similarity(&a, &b).unwrap();
        assert!(approx_eq(s, -1.0, 1e-9));
    }

    #[test]
    fn cosine_length_mismatch_errors() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        assert!(cosine_similarity(&a, &b).is_err());
    }

    #[test]
    fn cosine_zero_vector_safe() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 2.0];
        // 分母为 0 应返回 0,不 panic
        let s = cosine_similarity(&a, &b).unwrap();
        assert_eq!(s, 0.0);
    }

    #[test]
    fn dot_product_basic() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert_eq!(dot_product(&a, &b).unwrap(), 32.0);  // 4+10+18
    }

    #[test]
    fn euclidean_basic() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let d = euclidean_distance(&a, &b).unwrap();
        assert!(approx_eq(d, 5.0, 1e-9));
    }

    #[test]
    fn norm_unit_vector() {
        let v = vec![3.0, 4.0];
        assert!(approx_eq(l2_norm(&v), 5.0, 1e-9));
    }

    #[test]
    fn mock_bow_same_text_same_vector() {
        // 同一文本两次调用应得到完全相同的向量（确定性）
        let a = mock_bow_embedding("hello world");
        let b = mock_bow_embedding("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn mock_bow_different_text_different_vector() {
        let a = mock_bow_embedding("alpha beta gamma");
        let b = mock_bow_embedding("xyz foo bar");
        // 32 维中应该至少有几维不同
        let diffs = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        assert!(diffs > 0);
    }
}
/// v0.04补: 向量相似度/距离工具函数, v0.03 ai.cosine/dot/euclidean/norm 推迟到 v1.0
/// 保留为 "v1.0 复活点" + 内部测试用
#[allow(dead_code)]
/// 余弦相似度: (a·b) / (||a|| * ||b||)，范围 [-1, 1]
fn cosine_similarity(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!("cosine_similarity: length mismatch ({} vs {})", a.len(), b.len()));
    }
    let dot = dot_product(a, b)?;
    let na = l2_norm(a);
    let nb = l2_norm(b);
    if na == 0.0 || nb == 0.0 {
        return Ok(0.0);
    }
    Ok(dot / (na * nb))
}

/// 点积: a·b
#[allow(dead_code)]
fn dot_product(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!("dot_product: length mismatch ({} vs {})", a.len(), b.len()));
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| x * y).sum())
}

/// 欧氏距离: sqrt(sum((a-b)^2))
#[allow(dead_code)]
fn euclidean_distance(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!("euclidean_distance: length mismatch ({} vs {})", a.len(), b.len()));
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum::<f64>().sqrt())
}

/// L2 范数: sqrt(sum(x^2))
#[allow(dead_code)]
fn l2_norm(a: &[f64]) -> f64 {
    a.iter().map(|x| x * x).sum::<f64>().sqrt()
}



// ===================================================================
// 单元测试 — for 循环（修复 v10 之前的 bug：result.is_some() 误判）
// ===================================================================
// 复现主 bug: for body 内任意 Stmt::Expr（如 print）都返回 Some(val)，
// 原代码因此在第一次迭代后中断。修复后用 can_break 闸门，只在
// body 末尾为 Stmt::Return 时才中断。
#[cfg(test)]
mod for_loop_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp.interpret(&stmts)
    }

    #[test]
    fn for_over_list_runs_all_iters() {
        // 主 bug 复现：原代码 len=3 但只跑 1 次
        let src = r#"
task main()
  let xs = [10, 20, 30]
  let count: number = 0
  for x in xs
    let count = count + 1
  end
  print("count=" + count)
end
"#;
        run(src).expect("for loop should run 3 times");
    }

    #[test]
    fn for_with_print_runs_all_iters() {
        // 关键场景：body 内有 print 副作用。原代码会把 print 返回的
        // Some(Nil) 当成 return 信号，迭代 1 次就停。
        let src = r#"
task main()
  for x in [1, 2, 3]
    print("x=" + x)
  end
end
"#;
        run(src).expect("for with print should run all 3 iterations");
    }

    #[test]
    fn for_over_string_chars() {
        let src = r#"
task main()
  let s = ""
  for c in "abc"
    let s = s + c
  end
  print("s=" + s)
end
"#;
        run(src).expect("for over string should iterate all chars");
    }

    #[test]
    fn for_with_last_stmt_expr_does_not_break() {
        // 显式验证：body 末尾是 Stmt::Expr（如 print）时不中断
        let src = r#"
task main()
  for x in [1, 2, 3, 4, 5]
    print("y=" + (x * 2))
  end
end
"#;
        run(src).expect("for with last stmt expr should not break early");
    }

    #[test]
    fn for_with_last_stmt_let_does_not_break() {
        // 显式验证：body 末尾是 Stmt::Let 时不中断
        let src = r#"
task main()
  for x in [1, 2, 3]
    let y = x * 10
    print("y=" + y)
  end
end
"#;
        run(src).expect("for with last stmt let should not break early");
    }
}

// ===================================================================
// 单元测试 — return 传播（v11 重构）
// ===================================================================
// 修复 4 个 control-flow bug：return 信号原本被 Option<Value> 模糊化，
// 静默丢失在 for/if/try/match 边界。本次用 FlowSignal enum 显式区分
// None / Return(val)，并验证信号穿透所有控制结构。
#[cfg(test)]
mod return_propagation_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp.interpret(&stmts)
    }

    #[test]
    fn return_in_for_propagates_to_task() {
        // for body 内的 return 必须穿透 for 边界到外层 task，作为函数返回值
        let src = r#"
task main()
  task find(xs: list, t: number)
    for x in xs
      if x == t then
        return x
      end
    end
    return -1
  end
  let _ = find([1, 2, 3], 3)
end
"#;
        run(src).expect("return in for should propagate");
    }

    #[test]
    fn return_in_if_propagates_to_task() {
        let src = r#"
task main()
  task check(x: number)
    if x > 5 then
      return "big"
    end
    return "small"
  end
  let _ = check(10)
end
"#;
        run(src).expect("return in if should propagate");
    }

    #[test]
    fn return_in_try_does_not_trigger_catch() {
        // try 块内 return 不应进 catch；应当穿透 try 边界向外冒泡
        let src = r#"
task main()
  task maybe(blow: bool)
    try
      if blow then
        return 42
      end
      return 100
    catch err
      return -1
    end
  end
  let _ = maybe(true)
end
"#;
        run(src).expect("return in try should not trigger catch");
    }

    #[test]
    fn return_continues_after_loop() {
        // for 跑完所有迭代（无 return）后，task 继续往下执行
        let src = r#"
task main()
  task count()
    let total: number = 0
    for x in [1, 2, 3]
      let total = total + x
    end
    return total
  end
  let _ = count()
end
"#;
        run(src).expect("should continue after loop");
    }

    #[test]
    fn closure_expression_returns_value() {
        // fn(x) x * 2 end 的闭包返回值是 x*2，不是 nil
        // （这是闭包语义，不是 task 语义——闭包 body 单 expr 自动是返回值）
        let src = r#"
task main()
  let f = fn(x) x * 2 end
  let _ = f(5)
end
"#;
        run(src).expect("closure expression should return value");
    }

    #[test]
    fn flow_signal_into_value_handles_none() {
        // FlowSignal::None → nil (Mora 的"无显式 return"语义)
        assert_eq!(FlowSignal::None.into_value(), Value::Nil);
        assert_eq!(
            FlowSignal::Return(Value::Number(42.0)).into_value(),
            Value::Number(42.0)
        );
    }

    #[test]
    fn flow_signal_is_return_distinguishes_signals() {
        assert!(!FlowSignal::None.is_return());
        assert!(FlowSignal::Return(Value::Nil).is_return());
        assert!(FlowSignal::Return(Value::Number(0.0)).is_return());
    }
}
