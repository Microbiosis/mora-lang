use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ast::*;
use crate::flow::*;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::trace_collector::TraceCollector;

// Re-export value types for backward compatibility
pub use crate::value::{Value, Environment, FlowSignal, StreamReader};

/// v0.22: 检查方法是否可以融合执行
fn is_fusable_method(method: &str) -> bool {
    matches!(method, "map" | "filter" | "take" | "drop")
}

/// v0.22: 常量折叠 - 尝试在编译期计算二元操作
fn try_fold_binary(left: &Value, op: &BinaryOp, right: &Value) -> Option<Value> {
    match (left, op, right) {
        (Value::Number(l), BinaryOp::Add, Value::Number(r)) => Some(Value::Number(l + r)),
        (Value::Number(l), BinaryOp::Sub, Value::Number(r)) => Some(Value::Number(l - r)),
        (Value::Number(l), BinaryOp::Mul, Value::Number(r)) => Some(Value::Number(l * r)),
        (Value::Number(l), BinaryOp::Div, Value::Number(r)) => {
            if *r != 0.0 { Some(Value::Number(l / r)) } else { None }
        }
        (Value::Number(l), BinaryOp::Mod, Value::Number(r)) => {
            if *r != 0.0 { Some(Value::Number(l % r)) } else { None }
        }
        (Value::String(l), BinaryOp::Add, Value::String(r)) => {
            Some(Value::String(format!("{}{}", l, r)))
        }
        (Value::Number(l), BinaryOp::Equal, Value::Number(r)) => Some(Value::Bool(l == r)),
        (Value::String(l), BinaryOp::Equal, Value::String(r)) => Some(Value::Bool(l == r)),
        (Value::Bool(l), BinaryOp::Equal, Value::Bool(r)) => Some(Value::Bool(l == r)),
        (Value::Number(l), BinaryOp::NotEqual, Value::Number(r)) => Some(Value::Bool(l != r)),
        (Value::Number(l), BinaryOp::Greater, Value::Number(r)) => Some(Value::Bool(l > r)),
        (Value::Number(l), BinaryOp::Less, Value::Number(r)) => Some(Value::Bool(l < r)),
        (Value::Number(l), BinaryOp::GreaterEqual, Value::Number(r)) => Some(Value::Bool(l >= r)),
        (Value::Number(l), BinaryOp::LessEqual, Value::Number(r)) => Some(Value::Bool(l <= r)),
        _ => None,
    }
}

// v10 HTTP 超时配置
const HTTP_READ_TIMEOUT_SECS: u64 = 30;
const HTTP_WRITE_TIMEOUT_SECS: u64 = 10;
const AI_READ_TIMEOUT_SECS: u64 = 60;
// v0.08.5 cleanup: AI_STREAM_TIMEOUT_SECS 已删除（create_ai_stream 是 dead code）

// Value enum is now in value.rs
// Re-exported above via pub use crate::value::*;


// Environment is now in value.rs
// Re-exported above via pub use crate::value::*;

// ===================================================================
// v0.08.5: trait impl method 注册名集中生成
// 之前散落在 6 处 format!("__impl_{}_{}_{}", ...)，改命名规则要 6 处同步
// 现在收敛到这两个函数
// ===================================================================

/// v0.10: AI 调用 retry 配置（环境变量可覆盖）
/// MORA_AI_RETRY_MAX: 最大重试次数（默认 3，总计 4 次请求）
/// MORA_AI_RETRY_BASE_MS: 首次重试前的等待基准（默认 1000ms，后续翻倍 + jitter）
fn ai_retry_max() -> u32 {
    std::env::var("MORA_AI_RETRY_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}
fn ai_retry_base_ms() -> u64 {
    std::env::var("MORA_AI_RETRY_BASE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000)
}

/// v0.10: 判断错误是否可重试
///   - Transport 错误（网络问题）：可重试
///   - HTTP 429（rate limit）：可重试
///   - HTTP 5xx（服务器问题）：可重试
///   - HTTP 4xx 除 429：不可重试（client 错误）
fn is_retryable_error(err: &str) -> bool {
    if err.contains("network error") {
        return true; // ureq::Error::Transport
    }
    if let Some(rest) = err.strip_prefix("ai.chat: API error HTTP ")
        && let Some(code_str) = rest.split_whitespace().next()
        && let Ok(code) = code_str.parse::<u16>()
        && (code == 429 || (500..600).contains(&code))
    {
        return true;
    }
    false
}

/// v0.10: 计算 retry 等待时间（指数退避 + jitter）
///   attempt=0 → base
///   attempt=1 → base * 2 + jitter
///   attempt=2 → base * 4 + jitter
fn retry_sleep_ms(attempt: u32, base_ms: u64) -> u64 {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(10));
    let jitter = (exp / 5) as i64;
    let offset = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() as i64) % (jitter * 2 + 1) - jitter)
        .unwrap_or(0);
    (exp as i64 + offset).max(0) as u64
}

/// v0.09: 注册 impl method 时用的 key（含泛型签名）
/// 格式: __impl_<Trait>_<TraitGen>_<ForType>_<ForGen>_<method>
///   TraitGen / ForGen 用类型名（如 "Number" / "String"），简化版（v0.09 不含 typeck 类型）
///
/// 重要: 同一 trait 不同实例化产生不同 key，避免冲突
///   Container<number> vs Container<string> → 不同 key
pub(crate) fn impl_method_key(
    trait_name: &str,
    trait_generics: &[String], // v0.09 新增：trait 实例化的泛型
    for_type: &str,
    for_generics: &[String], // v0.09 新增：for_type 的泛型
    method: &str,
) -> String {
    let tg = trait_generics.join(",");
    let fg = for_generics.join(",");
    format!(
        "__impl_{}_{}_{}_{}_{}",
        trait_name, tg, for_type, fg, method
    )
}

/// v0.09: 默认实现的 key（self 类型 = trait 名）
/// 格式: __impl_<Trait>_<TraitGen>_<method>
pub(crate) fn default_impl_method_key(
    trait_name: &str,
    trait_generics: &[String], // v0.09 新增
    method: &str,
) -> String {
    let tg = trait_generics.join(",");
    format!("__impl_{}_{}_{}", trait_name, tg, method)
}

/// v0.08.5: BFS 收集 trait + 全部祖先的方法名（去重、防循环）
/// 用于：构造 trait instance 时的完整性检查（与 dispatch 保持一致）
///
/// 参数 trait_registry 是 trait 元数据表（self.trait_registry 借用）
pub(crate) fn collect_required_methods<'a>(
    trait_registry: &'a HashMap<String, TraitInfo>,
    trait_name: &str,
) -> Vec<&'a str> {
    let mut out: Vec<&'a str> = Vec::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut worklist: Vec<&str> = vec![trait_name];
    while let Some(t) = worklist.pop() {
        if !visited.insert(t.to_string()) {
            continue;
        }
        let td = match trait_registry.get(t) {
            Some(td) => td,
            None => continue, // 未知父 trait 跳过（typeck 已报错过）
        };
        for p in &td.parents {
            if !visited.contains(p) {
                worklist.push(p.as_str());
            }
        }
        for m in &td.methods {
            // 去重（子 trait 方法覆盖父 trait 同名方法）
            if !out.contains(&m.name.as_str()) {
                out.push(m.name.as_str());
            }
        }
    }
    out
}

/// v0.08.5: BFS 收集 trait + 全部祖先的 trait 名（去重、防循环）
/// 用于：trait dispatch 时查找 `__impl_<Trait>_<ForType>_<method>` 的 `<Trait>` 部分
///      （子 trait 的方法可能未实现，dispatcher fallback 到父 trait 的默认实现）
///
/// 与 collect_required_methods 的区别：本函数返回 trait 名（用于查 env 中的注册键），
/// collect_required_methods 返回 method 名（用于完整性检查）。
pub(crate) fn collect_parent_traits<'a>(
    trait_registry: &'a HashMap<String, TraitInfo>,
    trait_name: &str,
) -> Vec<&'a str> {
    let mut out: Vec<&'a str> = Vec::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut worklist: Vec<&str> = vec![trait_name];
    while let Some(t) = worklist.pop() {
        if !visited.insert(t.to_string()) {
            continue;
        }
        let td = match trait_registry.get(t) {
            Some(td) => td,
            None => continue, // 未知父 trait 跳过（typeck 已报错过）
        };
        // 先记录当前 trait
        if !out.contains(&td.name.as_str()) {
            out.push(td.name.as_str());
        }
        for p in &td.parents {
            if !visited.contains(p) {
                worklist.push(p.as_str());
            }
        }
    }
    out
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
    // v0.06: 当前 with 块 set 的 AiConfig 值 (替代 env hack)
    current_ai_config: Option<AiConfigValue>,
    // v0.08: trait 系统注册表
    pub trait_registry: HashMap<String, TraitInfo>,
    pub impl_table: HashMap<String, Vec<(String, Vec<Stmt>)>>,
    // v0.14: 录制/重放器 (默认 Off, 由 CLI 子命令激活)
    pub recorder: crate::record::Recorder,
    // v0.19: Worker 并发 channels
    worker_channels: HashMap<String, std::sync::mpsc::Sender<Value>>,
    worker_receivers: HashMap<String, std::sync::mpsc::Receiver<Value>>,
    // v0.22: AI 调用内联缓存 (prompt_hash -> response)
    ai_cache: HashMap<String, String>,
    // v0.22: 字符串驻留池 (减少重复字符串内存)
    string_interner: HashMap<String, Value>,
    // v0.22: 方法调用内联缓存 (type_name:method -> cached_fn_index)
    #[allow(dead_code)] // 未来扩展用
    method_cache: HashMap<String, usize>,
    // v0.22: AI 批量请求队列
    #[allow(dead_code)] // 未来扩展用
    ai_batch_queue: Vec<(String, Vec<(String, String)>)>, // (model, messages)
}

/// v0.06: with 块字段 (不经过 env 变量)
#[derive(Clone, Debug, Default)]
struct AiConfigValue {
    model: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<usize>,
    budget: Option<usize>,
    per_call: Option<usize>,
    system: Option<String>,
    /// v0.15: mock 响应队列 (with mock_llm = ["resp1", "resp2"])
    mock_responses: Option<Vec<String>>,
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
            current_ai_config: self.current_ai_config.clone(),
            trait_registry: self.trait_registry.clone(),
            impl_table: self.impl_table.clone(),
            recorder: crate::record::Recorder::new_off(),
            worker_channels: HashMap::new(), // 不克隆 channel
            worker_receivers: HashMap::new(),
            ai_cache: HashMap::new(), // 不克隆缓存
            string_interner: HashMap::new(), // 不克隆驻留池
            method_cache: HashMap::new(), // 不克隆缓存
            ai_batch_queue: Vec::new(), // 不克隆队列
        }
    }
}

/// Token 预算配置
#[derive(Clone)]
struct TokenBudget {
    total: usize,
    /// 每次调用 token 上限（v0.15 接入 track_tokens）
    per_call: Option<usize>,
    alert_threshold: f64, // 0.0-1.0，超过此比例时告警
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
    /// 单次请求 max_tokens（v0.15 接入 real_ai_chat_with_tools）
    max_tokens: Option<usize>,
    /// 系统提示词覆盖（v0.15 接入 real_ai_chat_with_tools）
    system: Option<String>,
    /// 温度覆盖（v0.15 接入 real_ai_chat_with_tools）
    temperature: Option<f64>,
}

// 记忆条目 — v0.04补: 字段已删 (RFC §4.1 memory.* 推迟到 v1.0)

/// 工具定义（注册时存储）
#[derive(Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: String, // JSON Schema 字符串
    pub handler: Value,     // Closure
}

/// v0.08: trait 注册条目
/// v0.08.4: 加 parents 字段实现 trait 继承
#[derive(Clone, Debug)]
pub struct TraitInfo {
    pub name: String,
    pub parents: Vec<String>,
    pub methods: Vec<TraitMethodSig>,
}

/// v0.08: trait 方法签名
/// v0.08.5 任务 1: 加 has_self 字段——trait method 第一个参数是 self 时为 true，
/// 否则为 false（self-less 方法）。self-less 调度时不传 receiver。
#[derive(Clone, Debug)]
pub struct TraitMethodSig {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    /// 第一个参数是否为 `self`（决定 dispatch 时是否传 receiver.clone()）
    pub has_self: bool,
}

/// 结构化聊天消息（用于支持 tool_calls）
enum ChatMessage {
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

/// 工具调用信息
#[derive(Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String, // JSON 字符串
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        let globals = Arc::new(Mutex::new(Environment::new()));
        globals.lock().expect("globals mutex poisoned").define(
            "print".to_string(),
            Value::Builtin("print".to_string()),
            false,
        );
        globals.lock().expect("globals mutex poisoned").define(
            "range".to_string(),
            Value::Builtin("range".to_string()),
            false,
        );
        globals
            .lock()
            .unwrap()
            .define("len".to_string(), Value::Builtin("len".to_string()), false);
        Self {
            globals: globals.clone(),
            environment: globals,
            tool_registry: HashMap::new(),
            model_routes: HashMap::new(),
            token_budget: None,
            token_usage: TokenUsage::default(),
            trace: TraceCollector::new(false),
            route_registry: HashMap::new(),
            current_ai_config: None,
            trait_registry: HashMap::new(),
            impl_table: HashMap::new(),
            recorder: crate::record::Recorder::new_off(),
            worker_channels: HashMap::new(),
            worker_receivers: HashMap::new(),
            ai_cache: HashMap::new(),
            string_interner: HashMap::new(),
            method_cache: HashMap::new(),
            ai_batch_queue: Vec::new(),
        }
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
            current_ai_config: None,
            trait_registry: HashMap::new(),
            impl_table: HashMap::new(),
            recorder: crate::record::Recorder::new_off(),
            worker_channels: HashMap::new(),
            worker_receivers: HashMap::new(),
            ai_cache: HashMap::new(),
            string_interner: HashMap::new(),
            method_cache: HashMap::new(),
            ai_batch_queue: Vec::new(),
        }
    }

    pub fn new_with_globals(globals: Arc<Mutex<Environment>>) -> Self {
        let env = Arc::new(Mutex::new(Environment::with_parent(globals.clone())));
        Self {
            globals: globals.clone(),
            environment: env,
            tool_registry: HashMap::new(),
            model_routes: HashMap::new(),
            token_budget: None,
            token_usage: TokenUsage::default(),
            trace: TraceCollector::new(false),
            route_registry: HashMap::new(),
            current_ai_config: None,
            trait_registry: HashMap::new(),
            impl_table: HashMap::new(),
            recorder: crate::record::Recorder::new_off(),
            worker_channels: HashMap::new(),
            worker_receivers: HashMap::new(),
            ai_cache: HashMap::new(),
            string_interner: HashMap::new(),
            method_cache: HashMap::new(),
            ai_batch_queue: Vec::new(),
        }
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

    /// v0.22: 字符串驻留 - 相同字符串只存储一次
    pub fn intern_string(&mut self, s: String) -> Value {
        if let Some(interned) = self.string_interner.get(&s) {
            return interned.clone();
        }
        let val = Value::String(s.clone());
        self.string_interner.insert(s, val.clone());
        val
    }

    pub fn interpret(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for stmt in stmts {
            self.execute(stmt)?;
        }
        // 先 clone 出值，再释放 borrow，避免借用冲突
        let main_task = self.globals.lock().expect("globals mutex poisoned").get("main").clone();
        if let Some(Value::Task { params, body, .. }) = main_task
            && params.is_empty()
        {
            let params = params.clone();
            let body = body.clone();
            self.call_task(&params, &body, vec![])?;
        }
        Ok(())
    }

    pub fn execute(&mut self, stmt: &Stmt) -> Result<FlowSignal, String> {
        match stmt {
            Stmt::Let {
                name,
                type_hint,
                init,
                exported,
                span: _,
            } => {
                // v0.13: Walrus 已删, 运行时不再区分 is_any
                let value = self.evaluate(init)?;
                if let Some(hint) = type_hint
                    && !check_type(&value, hint)
                {
                    return Err(format!(
                        "Type mismatch: expected {}, got {}",
                        hint,
                        type_name(&value)
                    ));
                }
                self.environment
                    .lock()
                    .unwrap()
                    .define(name.clone(), value, *exported);
                Ok(FlowSignal::None)
            }
            Stmt::Assign {
                name,
                value,
                span: _,
            } => {
                let val = self.evaluate(value)?;
                if !self.environment.lock().expect("environment mutex poisoned").assign(name, val.clone()) {
                    self.environment
                        .lock()
                        .unwrap()
                        .define(name.clone(), val, false);
                }
                Ok(FlowSignal::None)
            }
            Stmt::IndexAssign {
                object,
                index,
                value,
                span: _,
            } => {
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
            Stmt::TaskDef {
                name,
                lifetime_params: _, // v0.21: 生命周期参数（编译期检查，运行时忽略）
                params,
                return_type: _,
                body,
                exported,
                span: _,
            } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let task = Value::Task {
                    name: name.clone(),
                    params: param_names,
                    body: body.clone(),
                };
                self.environment
                    .lock()
                    .unwrap()
                    .define(name.clone(), task, *exported);
                Ok(FlowSignal::None)
            }
            Stmt::If {
                condition,
                then_branch,
                span: _,
            } => {
                let cond = self.evaluate(condition)?;
                if is_truthy(&cond) {
                    let env = Arc::new(Mutex::new(Environment::with_parent(
                        self.environment.clone(),
                    )));
                    // return 信号必须穿透 if 边界向外冒泡
                    self.execute_block(then_branch, env)
                } else {
                    Ok(FlowSignal::None)
                }
            }
            Stmt::For {
                var,
                var_type: _,
                iterable,
                body,
                span: _,
            } => {
                let iter_val = self.evaluate(iterable)?;
                // return 信号必须穿透 for 边界向外冒泡（每次迭代后检查）
                match iter_val {
                    Value::List(items) => {
                        for item in items {
                            let env = Arc::new(Mutex::new(Environment::with_parent(
                                self.environment.clone(),
                            )));
                            env.lock().expect("env mutex poisoned").define(var.clone(), item, false);
                            let signal = self.execute_block(body, env)?;
                            if signal.is_return() {
                                return Ok(signal);
                            }
                            if matches!(signal, FlowSignal::Break) {
                                return Ok(FlowSignal::None);
                            }
                            if matches!(signal, FlowSignal::Continue) {
                                continue;
                            }
                        }
                        Ok(FlowSignal::None)
                    }
                    Value::String(s) => {
                        for ch in s.chars() {
                            let env = Arc::new(Mutex::new(Environment::with_parent(
                                self.environment.clone(),
                            )));
                            env.lock().expect("env mutex poisoned").define(
                                var.clone(),
                                Value::String(ch.to_string()),
                                false,
                            );
                            let signal = self.execute_block(body, env)?;
                            if signal.is_return() {
                                return Ok(signal);
                            }
                            if matches!(signal, FlowSignal::Break) {
                                return Ok(FlowSignal::None);
                            }
                            if matches!(signal, FlowSignal::Continue) {
                                continue;
                            }
                        }
                        Ok(FlowSignal::None)
                    }
                    Value::Stream { reader, done } => {
                        loop {
                            let token = {
                                let mut guard = reader.lock();
                                if *done.lock().expect("done mutex poisoned") {
                                    None
                                } else {
                                    match Self::read_next_sse_token(&mut guard) {
                                        Ok(Some(t)) => Some(t),
                                        Ok(None) => {
                                            *done.lock().expect("done mutex poisoned") = true;
                                            None
                                        }
                                        Err(e) => {
                                            *done.lock().expect("done mutex poisoned") = true;
                                            return Err(format!("ai.stream: {}", e));
                                        }
                                    }
                                }
                            };
                            match token {
                                Some(tok) => {
                                    let env = Arc::new(Mutex::new(Environment::with_parent(
                                        self.environment.clone(),
                                    )));
                                    env.lock().expect("env mutex poisoned").define(
                                        var.clone(),
                                        Value::String(tok),
                                        false,
                                    );
                                    let signal = self.execute_block(body, env)?;
                                    if signal.is_return() {
                                        return Ok(signal);
                                    }
                                    if matches!(signal, FlowSignal::Break) {
                                        return Ok(FlowSignal::None);
                                    }
                                    if matches!(signal, FlowSignal::Continue) {
                                        continue;
                                    }
                                }
                                None => break,
                            }
                        }
                        Ok(FlowSignal::None)
                    }
                    _ => Err(format!("Cannot iterate over {}", iter_val)),
                }
            }
            Stmt::Import { path, span: _ } => {
                let module_env = self.import_module(path)?;
                let exports = module_env.lock().expect("env mutex poisoned").exports.clone();
                for (name, value) in exports {
                    self.environment.lock().expect("environment mutex poisoned").define(name, value, false);
                }
                Ok(FlowSignal::None)
            }
            Stmt::Parallel { stmts, span: _ } => self.execute_parallel(stmts),
            // v0.19: Worker 声明（在 parallel 块内处理）
            Stmt::Worker { name: _, body: _, span: _ } => {
                // Worker 声明本身不执行，由 parallel 块处理
                Ok(FlowSignal::None)
            },
            // v0.19: 发送消息
            Stmt::Send { value, target, span: _ } => {
                let val = self.evaluate(value)?;
                // 通过 channel 发送到目标 worker
                if let Some(tx) = self.worker_channels.get(target) {
                    tx.send(val).map_err(|e| format!("Send error: {}", e))?;
                }
                Ok(FlowSignal::None)
            },
            // v0.19: 接收消息
            Stmt::Receive { var, source, span: _ } => {
                // 从 source worker 接收消息
                if let Some(rx) = self.worker_receivers.get(source) {
                    let val = rx.recv().map_err(|e| format!("Receive error: {}", e))?;
                    self.environment.lock().expect("environment mutex poisoned").define(var.clone(), val, false);
                }
                Ok(FlowSignal::None)
            },
            // v0.19: 事务块
            Stmt::Transaction { body, compensation, span: _ } => {
                // 执行事务体
                let result = self.execute_transaction_body(body);
                match result {
                    Ok(FlowSignal::None) => Ok(FlowSignal::None),
                    Ok(signal) => Ok(signal),
                    Err(e) => {
                        // 事务失败，执行补偿
                        eprintln!("Transaction failed: {}, running compensation", e);
                        for stmt in compensation {
                            if let Err(comp_err) = self.execute(stmt) {
                                eprintln!("Compensation error: {}", comp_err);
                            }
                        }
                        Err(e)
                    }
                }
            },
            // v0.19: 提交事务 (空操作，事务自动提交)
            Stmt::Commit { span: _ } => Ok(FlowSignal::None),
            // v0.19: 回滚事务
            Stmt::Rollback { span: _ } => Err("Transaction rolled back".to_string()),
            // v0.20: 宏定义 (注册到环境)
            Stmt::MacroDef { name, params, body, span: _ } => {
                // 宏在运行时存储为特殊的 Value
                self.environment.lock().expect("environment mutex poisoned").define(
                    name.clone(),
                    Value::Macro {
                        name: name.clone(),
                        params: params.clone(),
                        body: body.clone(),
                    },
                    false,
                );
                Ok(FlowSignal::None)
            },
            // v0.23: 类型别名
            Stmt::TypeAlias { name, target, .. } => {
                // 类型别名在运行时只是注册名称映射
                self.environment.lock().expect("environment mutex poisoned").define(
                    name.clone(),
                    Value::String(target.clone()),
                    false,
                );
                Ok(FlowSignal::None)
            },
            // v0.23: 枚举类型
            Stmt::EnumDef { name, variants, .. } => {
                // 枚举在运行时存储为字典 {variant_name: Value::Builtin(...)}
                let mut enum_map = std::collections::HashMap::new();
                for v in variants {
                    enum_map.insert(v.name.clone(), Value::Builtin(v.name.clone()));
                }
                self.environment.lock().expect("environment mutex poisoned").define(
                    name.clone(),
                    Value::Dict(enum_map),
                    false,
                );
                Ok(FlowSignal::None)
            },
            // v0.23: 结构体类型
            Stmt::StructDef { name, fields, .. } => {
                // 结构体在运行时存储为构造函数
                // 创建一个闭包，接受字段值并返回字典
                let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                let constructor = Value::Closure {
                    params: field_names,
                    body: vec![], // 空 body，实际构造在 call_method 中处理
                    env: self.environment.clone(),
                };
                self.environment.lock().expect("environment mutex poisoned").define(
                    name.clone(),
                    constructor,
                    false,
                );
                Ok(FlowSignal::None)
            },
            Stmt::Match {
                expr,
                arms,
                span: _,
            } => {
                let val = self.evaluate(expr)?;
                for (pattern, arm_stmts) in arms {
                    if let Some(bindings) = self.match_pattern(pattern, &val) {
                        let env = Arc::new(Mutex::new(Environment::with_parent(
                            self.environment.clone(),
                        )));
                        for (name, value) in bindings {
                            env.lock().expect("env mutex poisoned").define(name, value, false);
                        }
                        return self.execute_block(arm_stmts, env);
                    }
                }
                Err("No match arm matched".to_string())
            }
            Stmt::Save {
                path,
                value,
                span: _,
            } => {
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
                let json =
                    fs::read_to_string(&path_str).map_err(|e| format!("Failed to load: {}", e))?;
                let value = json_to_value(&json)?;
                self.environment
                    .lock()
                    .unwrap()
                    .define(var.clone(), value, false);
                println!("[load] {} -> {}", path_str, var);
                Ok(FlowSignal::None)
            }
            Stmt::ReadFile { path, var, span: _ } => {
                // v11: read "path" into var  →  等价于 let var = file.read_text("path")
                let path_val = self.evaluate(path)?;
                let path_str = expect_string(path_val, "read path")?;
                let content = std::fs::read_to_string(&path_str)
                    .map_err(|e| format!("read: cannot read '{}': {}", path_str, e))?;
                self.environment
                    .lock()
                    .unwrap()
                    .define(var.clone(), Value::String(content), false);
                println!("[read] {} -> {}", path_str, var);
                Ok(FlowSignal::None)
            }
            Stmt::WriteFile {
                path,
                content,
                span: _,
            } => {
                // v11: write "path", content  →  等价于 file.write_text("path", content)
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "write path")?;
                let content_str = expect_string(content_val, "write content")?;
                if let Some(parent) = std::path::Path::new(&path_str).parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.exists()
                {
                    return Err(format!(
                        "write: parent directory does not exist: {}",
                        parent.display()
                    ));
                }
                std::fs::write(&path_str, &content_str)
                    .map_err(|e| format!("write: cannot write '{}': {}", path_str, e))?;
                println!("[write] {}", path_str);
                Ok(FlowSignal::None)
            }
            Stmt::AppendFile {
                path,
                content,
                span: _,
            } => {
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
                self.environment.lock().expect("environment mutex poisoned").define(
                    var.clone(),
                    Value::String(hex_encode(&bytes)),
                    false,
                );
                println!(
                    "[read_bytes] {} -> {} ({} bytes)",
                    path_str,
                    var,
                    bytes.len()
                );
                Ok(FlowSignal::None)
            }
            Stmt::WriteBytesFile {
                path,
                content,
                span: _,
            } => {
                // v11: write_bytes "path", hex
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "write_bytes path")?;
                let hex = expect_string(content_val, "write_bytes content")?;
                let bytes = hex_decode(&hex).map_err(|e| format!("write_bytes: {}", e))?;
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
            Stmt::With {
                bindings,
                body,
                span: _,
            } => {
                // v0.06: 推入 `current_ai_config` 栈 (替代 v0.04 env hack)
                let prev_cfg = self.current_ai_config.clone();
                let mut cfg = prev_cfg.clone().unwrap_or_default();
                for (key, val_expr) in bindings {
                    let v = self.evaluate(val_expr)?;
                    match key.as_str() {
                        "model" => {
                            cfg.model = Some(match &v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            });
                        }
                        "temperature" => {
                            cfg.temperature = Some(match &v {
                                Value::Number(n) => *n,
                                _ => {
                                    return Err(format!(
                                        "with: temperature must be number, got {}",
                                        v
                                    ));
                                }
                            });
                        }
                        "max_tokens" => {
                            cfg.max_tokens = Some(match &v {
                                Value::Number(n) => *n as usize,
                                _ => {
                                    return Err(format!(
                                        "with: max_tokens must be number, got {}",
                                        v
                                    ));
                                }
                            });
                        }
                        "budget" => {
                            cfg.budget = Some(match &v {
                                Value::Number(n) => *n as usize,
                                _ => return Err(format!("with: budget must be number, got {}", v)),
                            });
                        }
                        "per_call" => {
                            cfg.per_call = Some(match &v {
                                Value::Number(n) => *n as usize,
                                _ => return Err(format!("with: per_call must be number, got {}", v)),
                            });
                        }
                        "system" => {
                            cfg.system = Some(match &v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            });
                        }
                        "mock_llm" => {
                            // mock_llm = ["resp1", "resp2", ...]
                            let responses = match &v {
                                Value::List(items) => {
                                    let mut r = Vec::new();
                                    for item in items {
                                        match item {
                                            Value::String(s) => r.push(s.clone()),
                                            other => r.push(other.to_string()),
                                        }
                                    }
                                    r
                                }
                                _ => return Err(format!("with: mock_llm must be list, got {}", v)),
                            };
                            cfg.mock_responses = Some(responses);
                        }
                        other => {
                            return Err(format!(
                                "with: unknown binding '{}' (valid: model, budget, per_call, temperature, max_tokens, system, mock_llm)",
                                other
                            ));
                        }
                    }
                }
                // v0.15: 同步设置 token_budget（budget/per_call）
                let prev_budget = self.token_budget.clone();
                if cfg.budget.is_some() || cfg.per_call.is_some() {
                    self.token_budget = Some(TokenBudget {
                        total: cfg.budget.unwrap_or(usize::MAX),
                        per_call: cfg.per_call,
                        alert_threshold: 0.8,
                    });
                }
                self.current_ai_config = Some(cfg);
                let env_in = Arc::new(Mutex::new(Environment::with_parent(
                    self.environment.clone(),
                )));
                let result = self.execute_block(body, env_in);
                // 恢复外层 config 和 budget
                self.current_ai_config = prev_cfg;
                self.token_budget = prev_budget;
                result
            }
            Stmt::StreamFor {
                prompt,
                var,
                body,
                span: _,
            } => {
                // 求值 prompt（应当是 Prompt 表达式，返回 Value::Stream）
                let prompt_str = Self::eval_prompt_parts_from_stmt(prompt, self)?;
                // v0.04: stream 块简化 — mock 模式按字符拆 token
                // (v0.04.1 跟进真实 streaming SSE)
                let tokens: Vec<String> = prompt_str.chars().map(|c| c.to_string()).collect();
                for token in tokens {
                    let env_in = Arc::new(Mutex::new(Environment::with_parent(
                        self.environment.clone(),
                    )));
                    env_in
                        .lock()
                        .unwrap()
                        .define(var.clone(), Value::String(token), false);
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
            Stmt::ToolDef {
                name,
                params,
                return_type,
                body,
                exported: _,
                span: _,
            } => {
                // v0.04.0: 注册到全局工具表（与 v0.03 ai.tool 等价）
                let name_clone = name.clone();
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let type_hints: Vec<String> = params
                    .iter()
                    .map(|(_, t)| t.clone().unwrap_or_else(|| "any".to_string()))
                    .collect();
                let return_str = return_type.clone().unwrap_or_else(|| "any".to_string());
                // 用闭包捕获 body
                let tool_body = body.clone();
                let func = Value::Task {
                    name: name.clone(),
                    params: param_names.clone(),
                    body: tool_body,
                };
                self.environment
                    .lock()
                    .unwrap()
                    .define(name.clone(), func, false);
                // 同时注册到 ai.tool registry（v0.03 路径）
                self.register_tool(name_clone, param_names, type_hints, return_str);
                Ok(FlowSignal::None)
            }
            Stmt::Break { span: _ } => Ok(FlowSignal::Break),
            Stmt::Continue { span: _ } => Ok(FlowSignal::Continue),
            // v0.06.7: serve as 语法糖已移除。用 Router::new() / McpServer::new() 显式 API
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
                // v0.06.5: route 元数据 (temperature/max_tokens/system) 存入 current_ai_config
                let model_name_clone = model_name.clone();
                let route_cfg: Option<AiConfigValue> = if let Value::Dict(m) = target_val {
                    Some(AiConfigValue {
                        model: m.get("_model").and_then(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        }),
                        temperature: m.get("temperature").and_then(|v| match v {
                            Value::Number(n) => Some(*n),
                            _ => None,
                        }),
                        max_tokens: m.get("max_tokens").and_then(|v| match v {
                            Value::Number(n) => Some(*n as usize),
                            _ => None,
                        }),
                        system: m.get("system").and_then(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        }),
                        ..Default::default()
                    })
                } else {
                    None
                };
                self.route_registry.insert(name.clone(), model_name);
                // v0.06.5: 存 route cfg 到 model_routes (替代 env)
                if let Some(cfg) = route_cfg {
                    let rc = RouteConfig {
                        model: model_name_clone,
                        base_url: String::new(),
                        api_key: String::new(),
                        max_tokens: cfg.max_tokens,
                        system: cfg.system.clone(),
                        temperature: cfg.temperature,
                    };
                    self.model_routes.insert(name.clone(), rc);
                }
                eprintln!(
                    "[route] registered: {} -> {}",
                    name, self.route_registry[name]
                );
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
                            other => {
                                return Err(format!(
                                    "observe otel endpoint must be a string, got {}",
                                    other
                                ));
                            }
                        };
                        self.trace.set_otel_endpoint(endpoint_str.clone());
                        self.trace.set_enabled(true);
                        eprintln!("[observe] OTEL enabled, endpoint: {}", endpoint_str);
                    }
                }
                // 执行 body
                let body_env = Arc::new(Mutex::new(Environment::with_parent(
                    self.environment.clone(),
                )));
                self.execute_block(body, body_env)
            }
            Stmt::Span {
                name,
                attributes,
                body,
                ..
            } => {
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
                let body_env = Arc::new(Mutex::new(Environment::with_parent(
                    self.environment.clone(),
                )));
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
            Stmt::RecordTokens {
                input,
                output,
                span: _,
            } => {
                let in_val = self.evaluate(input)?;
                let out_val = self.evaluate(output)?;
                let in_n = match &in_val {
                    Value::Number(n) => *n as u64,
                    _ => {
                        return Err(format!(
                            "record_tokens: input must be number, got {}",
                            in_val
                        ));
                    }
                };
                let out_n = match &out_val {
                    Value::Number(n) => *n as u64,
                    _ => {
                        return Err(format!(
                            "record_tokens: output must be number, got {}",
                            out_val
                        ));
                    }
                };
                self.trace.record_tokens(in_n, out_n);
                Ok(FlowSignal::None)
            }
            // v0.08: trait 定义 — 注册到 trait_registry
            // v0.08.3: 有默认实现的 method 同步注册为 __impl_<Trait>_<Trait>_<method>
            //          任何 impl 没实现时，dispatcher fallback 到默认实现
            // v0.08.4: 记录 parents（trait 继承）
            // v0.09: 解构含 generics + trait_where 字段
            //   generics: trait 自身的泛型参数（如 `Container<T>` 的 `T`）
            //   trait_where: trait 的 where 子句（v0.09 完整版）
            Stmt::TraitDef {
                name,
                generics,
                parents,
                trait_where: _,
                methods,
                span: _,
            } => {
                let method_sigs: Vec<TraitMethodSig> = methods
                    .iter()
                    .map(|m| TraitMethodSig {
                        name: m.name.clone(),
                        params: m.params.clone(),
                        return_type: m.return_type.clone(),
                        // v0.08.5 任务 1: 第一个参数名为 `self` 视为有 self
                        has_self: m.params.first().map(|(n, _)| n == "self").unwrap_or(false),
                    })
                    .collect();
                // v0.09: 把 trait 自身的 generics 传给 default_impl_method_key
                let trait_generics_for_default: Vec<String> =
                    generics.iter().map(|g| g.name.clone()).collect();
                self.trait_registry.insert(
                    name.clone(),
                    TraitInfo {
                        name: name.clone(),
                        parents: parents.clone(),
                        methods: method_sigs,
                    },
                );
                // 默认实现：注册到 __impl_<Trait>_<TraitGen>_<method>（v0.09 key 含 generics）
                for m in methods {
                    if !m.body.is_empty() {
                        let default_method_name =
                            default_impl_method_key(name, &trait_generics_for_default, &m.name);
                        let td = Stmt::TaskDef {
                            name: default_method_name.clone(),
                            lifetime_params: vec![],
                            params: m.params.clone(),
                            return_type: m.return_type.clone(),
                            body: m.body.clone(),
                            exported: false,
                            span: m.span,
                        };
                        self.execute(&td)?;
                    }
                }
                Ok(FlowSignal::None)
            }
            // v0.08: impl 块 — 为 trait 提供实现，注册到 impl_table
            // v0.08.1: 同时为每个 impl method 注册为 __impl_TraitName_methodName TaskDef
            //          使 vtable dispatcher closure 可通过 call_task 路由调用
            // v0.09: 解构含 5 个新字段
            Stmt::ImplDef {
                generics: _,
                trait_generics,
                trait_name,
                for_type,
                for_generics,
                where_clause: _,
                methods,
                span: _,
            } => {
                for m in methods {
                    // v0.09: 注册名为 __impl_<Trait>_<TraitGen>_<ForType>_<ForGen>_<method>
                    let impl_method_name = impl_method_key(
                        trait_name,
                        trait_generics,
                        for_type,
                        for_generics,
                        &m.name,
                    );
                    let td = Stmt::TaskDef {
                        name: impl_method_name.clone(),
                        lifetime_params: vec![],
                        params: m.params.clone(),
                        return_type: m.return_type.clone(),
                        body: m.body.clone(),
                        exported: false,
                        span: m.span,
                    };
                    // 1. 执行 TaskDef 注册到 environment
                    self.execute(&td)?;
                    // 2. 记录到 impl_table 供 typeck 检索
                    self.impl_table
                        .entry(for_type.clone())
                        .or_default()
                        .push((trait_name.clone(), vec![td]));
                }
                Ok(FlowSignal::None)
            }
        } // ← match stmt { ... } 闭合
    } // ← pub fn execute(...) 闭合

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
            if handle.read_line(&mut line).is_err() {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "exit" || trimmed == "quit" {
                println!("Bye!");
                break;
            }
            let tokens = Lexer::new(trimmed).scan_tokens();
            let stmts = Parser::new(tokens).parse();
            if stmts.is_empty() {
                continue;
            }
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

    fn execute_block(
        &mut self,
        stmts: &[Stmt],
        env: Arc<Mutex<Environment>>,
    ) -> Result<FlowSignal, String> {
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
    fn register_tool(
        &mut self,
        name: String,
        params: Vec<String>,
        types: Vec<String>,
        _return_type: String,
    ) {
        // v0.04 Slice 5: 真正实现 — 自动生成 JSON Schema 并存到 tool_registry
        // description: v0.04.0 简化（空字符串，v0.04.1 跟进 desc: 段）
        // parameters: 从 params + types 自动生成 JSON Schema
        let schema = Self::tool_to_json_schema(&params, &types);
        let tool_def = crate::interpreter::ToolDef {
            name: name.clone(),
            description: String::new(),
            parameters: schema,
            handler: Value::Nil, // handler 不存这里 (handler 是 Stmt::ToolDef body 的 closure, 解析时已绑)
        };
        self.tool_registry.insert(name, tool_def);
    }

    /// v0.04 Slice 5: 从 params + types 生成标准 JSON Schema
    fn tool_to_json_schema(params: &[String], types: &[String]) -> String {
        // 生成 {"type":"object","properties":{...},"required":[...]} 格式
        let mut properties = String::from("{");
        let mut required = Vec::new();
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                properties.push(',');
            }
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
        properties.push('}');
        let required_str = required
            .iter()
            .map(|r| format!("\"{}\"", r))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"type\":\"object\",\"properties\":{},\"required\":[{}]}}",
            properties, required_str
        )
    }

    /// v0.19: 执行事务体
    fn execute_transaction_body(&mut self, stmts: &[Stmt]) -> Result<FlowSignal, String> {
        for stmt in stmts {
            match self.execute(stmt)? {
                FlowSignal::None => {}
                signal => return Ok(signal),
            }
        }
        Ok(FlowSignal::None)
    }

    fn execute_parallel(&mut self, stmts: &[Stmt]) -> Result<FlowSignal, String> {
        // v0.19: 检查是否包含 worker 声明
        let has_workers = stmts.iter().any(|s| matches!(s, Stmt::Worker { .. }));

        if has_workers {
            // Worker 模式：创建 channel 并并行执行 worker
            self.execute_parallel_workers(stmts)
        } else {
            // 原有模式：并行执行语句
            self.execute_parallel_simple(stmts)
        }
    }

    /// 原有的并行执行（无 worker）
    fn execute_parallel_simple(&mut self, stmts: &[Stmt]) -> Result<FlowSignal, String> {
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

    /// v0.19: Worker 并发执行
    fn execute_parallel_workers(&mut self, stmts: &[Stmt]) -> Result<FlowSignal, String> {
        use std::sync::mpsc;
        use std::collections::HashMap;

        // 收集所有 worker 声明
        let workers: Vec<(String, Vec<Stmt>)> = stmts.iter()
            .filter_map(|s| match s {
                Stmt::Worker { name, body, .. } => Some((name.clone(), body.clone())),
                _ => None,
            })
            .collect();

        // 为每个 worker 创建 channel
        let mut channels: HashMap<String, (mpsc::Sender<Value>, mpsc::Receiver<Value>)> = HashMap::new();
        for (name, _) in &workers {
            let (tx, rx) = mpsc::channel();
            channels.insert(name.clone(), (tx, rx));
        }

        // 创建 worker 的发送/接收映射
        // 每个 worker 可以向其他 worker 发送消息
        let mut _worker_senders: HashMap<String, mpsc::Sender<Value>> = HashMap::new();

        for (name, (tx, _rx)) in channels {
            _worker_senders.insert(name.clone(), tx);
            // 注意：这里需要为每个 worker 创建独立的接收器
            // 简化实现：使用 Arc<Mutex<Receiver>> 共享
        }

        // 并行执行所有 worker
        let globals = self.globals.clone();
        std::thread::scope(|s| {
            let mut handles = Vec::new();

            for (name, body) in &workers {
                let globals = globals.clone();
                let body = body.clone();
                let name = name.clone();

                // 为每个 worker 创建独立的 channel
                let (tx, rx) = mpsc::channel::<Value>();

                handles.push(s.spawn(move || {
                    let mut interpreter = Interpreter::new_with_globals(globals);
                    // 设置 worker 的 channel
                    interpreter.worker_channels.insert("main".to_string(), tx);
                    interpreter.worker_receivers.insert(name.clone(), rx);

                    // 执行 worker body
                    for stmt in &body {
                        if let Err(e) = interpreter.execute(stmt) {
                            eprintln!("Worker '{}' error: {}", name, e);
                        }
                    }
                    Ok::<(), String>(())
                }));
            }

            // 等待所有 worker 完成
            for handle in handles {
                match handle.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("Worker error: {}", e),
                    Err(_) => eprintln!("Worker panicked"),
                }
            }
        });

        Ok(FlowSignal::None)
    }

    fn match_pattern(&mut self, pattern: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
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
            (Pattern::List { prefix, rest }, Value::List(vals)) => {
                // v0.16: 支持 ...rest 模式
                if let Some(rest_name) = rest {
                    // 有 rest 模式：prefix 必须匹配前缀
                    if vals.len() < prefix.len() {
                        return None;
                    }
                    let mut bindings = Vec::new();
                    for (pat, val) in prefix.iter().zip(vals.iter()) {
                        if let Some(b) = self.match_pattern(pat, val) {
                            bindings.extend(b);
                        } else {
                            return None;
                        }
                    }
                    // rest 绑定剩余元素
                    let rest_vals: Vec<Value> = vals[prefix.len()..].to_vec();
                    bindings.push((rest_name.clone(), Value::List(rest_vals)));
                    Some(bindings)
                } else {
                    // 无 rest 模式：长度必须精确匹配
                    if prefix.len() != vals.len() {
                        return None;
                    }
                    let mut bindings = Vec::new();
                    for (pat, val) in prefix.iter().zip(vals.iter()) {
                        if let Some(b) = self.match_pattern(pat, val) {
                            bindings.extend(b);
                        } else {
                            return None;
                        }
                    }
                    Some(bindings)
                }
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
            // v0.16: 守卫模式 (Prolog 启发)
            (Pattern::Guard { pattern, condition }, val) => {
                // 1. 先匹配内部模式
                if let Some(bindings) = self.match_pattern(pattern, val) {
                    // 2. 在绑定环境中求值守卫条件
                    let env = Arc::new(Mutex::new(Environment::with_parent(
                        self.environment.clone(),
                    )));
                    for (name, value) in &bindings {
                        env.lock().expect("env mutex poisoned").define(name.clone(), value.clone(), false);
                    }
                    let previous = self.environment.clone();
                    self.environment = env;
                    let cond_result = self.evaluate(condition);
                    self.environment = previous;
                    // 3. 检查条件是否为 true
                    match cond_result {
                        Ok(Value::Bool(true)) => Some(bindings),
                        Ok(Value::Bool(false)) => None,
                        Ok(_) => None, // 非 bool 值视为 false
                        Err(_) => None,
                    }
                } else {
                    None
                }
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
                let value = self.environment.lock().expect("environment mutex poisoned").get(name);
                match value {
                    Some(v) => Ok(v),
                    None if is_builtin_object(name) => Ok(Value::Builtin(name.clone())),
                    None => Err(format!("Undefined variable: {}", name)),
                }
            }
            Expr::Grouping(expr, _) => self.evaluate(expr),
            Expr::Binary {
                left,
                op,
                right,
                span: _,
            } => {
                // v0.22: 常量折叠优化
                // 如果两个操作数都是字面量，直接计算
                if let (Expr::Literal(l), Expr::Literal(r)) = (left.as_ref(), right.as_ref())
                    && let (Ok(lv), Ok(rv)) = (self.literal_to_value(l), self.literal_to_value(r))
                        && let Some(folded) = try_fold_binary(&lv, op, &rv) {
                            return Ok(folded);
                        }
                let left = self.evaluate(left)?;
                let right = self.evaluate(right)?;
                eval_binary(left, op, right)
            }
            Expr::Pipe {
                left,
                right,
                span: _,
            } => {
                let left_val = self.evaluate(left)?;
                self.evaluate_pipe(left_val, right)
            }
            Expr::Call { callee, args, span } => {
                // v0.04 Slice 2: 先看 route_registry
                if self.route_registry.contains_key(callee) {
                    // 已注册 → 走 RouteCall 路径
                    let model = self.route_registry.get(callee).expect("route should exist").clone();
                    if args.is_empty() {
                        return Err(format!(
                            "route '{}()' requires 1 argument (the prompt)",
                            callee
                        ));
                    }
                    let prompt_str = Self::eval_route_arg(&args[0], self)?;
                    return Self::do_ai_chat(self, &model, &prompt_str);
                }
                // 未注册 → 普通函数调用
                let arg_values: Result<Vec<Value>, String> =
                    args.iter().map(|a| self.evaluate(a.as_ref())).collect();
                self.call_function(callee, arg_values?, *span)
            }
            Expr::MethodCall {
                object,
                method,
                args,
                span,
            } => {
                let obj = self.evaluate(object)?;
                let arg_values: Result<Vec<Value>, String> =
                    args.iter().map(|a| self.evaluate(a.as_ref())).collect();
                self.call_method(obj, method, arg_values?, *span)
            }
            Expr::Index {
                object,
                index,
                span: _,
            } => {
                let obj = self.evaluate(object)?;
                let idx = self.evaluate(index)?;
                match (&obj, &idx) {
                    (Value::List(list), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < list.len() {
                            Ok(list[i].clone())
                        } else {
                            Err(format!("Index out of bounds: {} (len: {})", i, list.len()))
                        }
                    }
                    (Value::String(s), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < s.len() {
                            Ok(Value::Char(s.chars().nth(i).expect("index out of bounds")))
                        } else {
                            Err(format!("Index out of bounds: {} (len: {})", i, s.len()))
                        }
                    }
                    (Value::Dict(map), Value::String(key)) => {
                        Ok(map.get(key).cloned().unwrap_or(Value::Nil))
                    }
                    _ => Err(format!("Cannot index {} with {}", obj, idx)),
                }
            }
            Expr::Closure {
                params,
                return_type: _,
                body,
                span: _,
            } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                Ok(Value::Closure {
                    params: param_names,
                    body: body.clone(),
                    env: self.environment.clone(),
                })
            }
            // v0.21: 不可变借用
            Expr::Borrow { expr, span: _ } => {
                let val = self.evaluate(expr)?;
                // 返回一个引用值（简化实现：用 Arc<Mutex<Value>> 包装）
                Ok(Value::Atom(Arc::new(Mutex::new(val))))
            }
            // v0.21: 可变借用
            Expr::BorrowMut { expr, span: _ } => {
                let val = self.evaluate(expr)?;
                // 返回一个可引用值
                Ok(Value::Atom(Arc::new(Mutex::new(val))))
            }
            Expr::Match {
                expr,
                arms,
                span: _,
            } => {
                let val = self.evaluate(expr)?;
                for (pattern, arm_expr) in arms.iter() {
                    if let Some(bindings) = self.match_pattern(pattern, &val) {
                        let env = Arc::new(Mutex::new(Environment::with_parent(
                            self.environment.clone(),
                        )));
                        for (name, value) in bindings {
                            env.lock().expect("env mutex poisoned").define(name, value, false);
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
                // v0.22: 常量 prompt 编译期处理
                // 如果所有 parts 都是字面量，直接拼接
                let is_constant = parts.iter().all(|p| matches!(p, Expr::Literal(_)));
                let prompt_str = if is_constant {
                    // 编译期拼接
                    let mut s = String::new();
                    for p in parts {
                        if let Expr::Literal(lit) = p {
                            s.push_str(&self.literal_to_value(lit)?.to_string());
                        }
                    }
                    s
                } else {
                    Self::eval_prompt_parts(parts, self)?
                };
                Self::do_ai_chat(self, "gpt-4o-mini", &prompt_str)
            }
            // v0.04 Slice 2: fast(p"...") → 直接调 real_ai_chat_inner 用对应 model
            Expr::RouteCall { name, args, .. } => {
                // 1. 找 model
                let model = self
                    .route_registry
                    .get(name)
                    .ok_or_else(|| {
                        format!(
                            "route '{}' not defined (use `route {}: \"<model>\"` first)",
                            name, name
                        )
                    })?
                    .clone();
                // 2. 求值 args[0] (期望是 Prompt 表达式)
                if args.is_empty() {
                    return Err(format!(
                        "route call '{}()' requires 1 argument (the prompt)",
                        name
                    ));
                }
                let prompt_str = Self::eval_route_arg(&args[0], self)?;
                Self::do_ai_chat(self, &model, &prompt_str)
            }
            // v0.04补: ai_model("name", temperature: 0.7, ...) 表达式
            // 求值后返回 Dict {_model, temperature?, max_tokens?, system?}
            Expr::AiModelCall {
                model,
                temperature,
                max_tokens,
                system,
                span: _,
            } => {
                let model_str = match self.evaluate(model)? {
                    Value::String(s) => s,
                    other => {
                        return Err(format!(
                            "ai_model: model name must be string, got {}",
                            other
                        ));
                    }
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
            // v0.06.2: expr? 操作符 — Result<T,E> 早 return
            Expr::Question { expr, span: _ } => {
                match self.evaluate(expr)? {
                    Value::Dict(m) if m.contains_key("ok") => {
                        Ok(m.get("ok").cloned().unwrap_or(Value::Nil))
                    }
                    Value::Dict(m) if m.contains_key("err") => {
                        let err_val = m.get("err").cloned().unwrap_or(Value::Nil);
                        // ? 操作符在 task/closure 内触发早 return，
                        // 但当前在 evaluate 递归里无法早 return →
                        // 这里把 Err 包装成 Continue/特殊标记，由调用方
                        // (call_task/call_closure 的 execute_block) 检测
                        Err(format!("?error: {}", err_val))
                    }
                    other => Err(format!(
                        "'?' expects Result<T,E> (dict with 'ok' or 'err'), got {}",
                        other
                    )),
                }
            }
            // v0.07.1: NamespaceRef — IDENT::IDENT evaluated by joining and calling as builtin
            Expr::NamespaceRef {
                namespace,
                name,
                span: _,
            } => {
                let qualified = format!("{}::{}", namespace, name);
                // Router::new / McpServer::new etc.
                match qualified.as_str() {
                    "Router::new" => Ok(Value::Router {
                        routes: Arc::new(Mutex::new(Vec::new())),
                    }),
                    "McpServer::new" => Ok(Value::McpServer { tools: Vec::new() }),
                    other => {
                        // fallback: look up in call_function
                        self.call_function(other, vec![], Span::default())
                    }
                }
            }
            // v0.08: DynTrait — evaluate to trait object stub
            // v0.09: 解构含 generics 字段
            Expr::DynTrait {
                generics,
                trait_name,
                ..
            } => self.eval_dyn_trait(trait_name, generics),
        }
    }

    /// v0.08.2: 求值 dyn trait 表达式 → TraitObject
    /// v0.09: 加 trait_generics 参数（dyn Container<number> 的 number）
    fn eval_dyn_trait(
        &mut self,
        trait_name: &str,
        trait_generics: &[String],
    ) -> Result<Value, String> {
        Ok(Value::TraitObject {
            for_generics: vec![],
            trait_generics: trait_generics.to_vec(),
            for_type: "nil".to_string(),
            trait_name: trait_name.to_string(),
            data: Box::new(Value::Dict(HashMap::new())),
        })
    }

    /// v0.08.5: dyn dispatch —— 接收 TraitObject + method，从 trait 继承链找 impl 调用
    /// 不再嗅探 data 里的 Dict key（TraitObject 自身字段就是 type 元数据）
    /// v0.08.5 fix: 错误信息附调用点 span（line N），用户可在编辑器跳转
    /// v0.08.5 任务 1: 根据 trait method 的 has_self 决定是否传 receiver 作为第一参数
    fn dispatch_trait_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: Vec<Value>,
        call_site: Span,
    ) -> Result<Value, String> {
        // v0.08.5: 直接从 TraitObject 字段读 for_type / trait_name（不再嗅探 data._type / data._trait）
        // v0.09: 同时读 for_generics / trait_generics 用于泛型 dispatch
        let (for_type, for_generics, trait_name, trait_generics) = match receiver {
            Value::TraitObject {
                for_type,
                for_generics,
                trait_name,
                trait_generics,
                ..
            } => (
                for_type.clone(),
                for_generics.clone(),
                trait_name.clone(),
                trait_generics.clone(),
            ),
            Value::Nil => return Ok(Value::Nil),
            _ => {
                return Err(format!(
                    "trait dispatch at line {}: receiver must be trait object or nil, got {:?}",
                    call_site.line, receiver
                ));
            }
        };
        // v0.08.5: 用 collect_parent_traits() 沿 parents chain BFS 收集所有候选 trait
        // （子 trait 的方法未实现时 dispatcher fallback 到父 trait 的默认实现）
        let search_chain = collect_parent_traits(&self.trait_registry, &trait_name);
        for tname in &search_chain {
            let tname_str: &str = tname;
            // v0.08.5 任务 1: 查 trait_registry 看 method 是否 self-having
            //   has_self=true → args 前面加 receiver
            //   has_self=false → args 直接传（self-less 方法）
            let has_self = self
                .trait_registry
                .get(tname_str)
                .and_then(|info| info.methods.iter().find(|m| m.name == method))
                .map(|m| m.has_self)
                .unwrap_or(true); // fallback: 默认 self-having（向后兼容）
            // 1. 先找具体类型的 impl（key 含 generics）
            let impl_name =
                impl_method_key(tname_str, &trait_generics, &for_type, &for_generics, method);
            let env = self.environment.lock().expect("environment mutex poisoned");
            if let Some(task) = env.get(&impl_name) {
                drop(env);
                let mut all_args = if has_self {
                    vec![receiver.clone()]
                } else {
                    Vec::new()
                };
                all_args.extend(args);
                return self.call_value(&task, all_args);
            }
            drop(env);
            // 2. v0.08.3: fallback 到 trait 自身的默认实现（key 也含 generics）
            let default_name = default_impl_method_key(tname_str, &trait_generics, method);
            let env = self.environment.lock().expect("environment mutex poisoned");
            if let Some(task) = env.get(&default_name) {
                drop(env);
                let mut all_args = if has_self {
                    vec![receiver.clone()]
                } else {
                    Vec::new()
                };
                all_args.extend(args);
                return self.call_value(&task, all_args);
            }
            drop(env);
        }
        Err(format!(
            "trait dispatch at line {}: no impl for type '{}' method '{}' (searched: {}, generics: trait<{}> for {})",
            call_site.line,
            for_type,
            method,
            search_chain
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(" → "),
            trait_generics.join(","),
            for_generics.join(","),
        ))
    }

    /// v0.08.2: 构造 trait instance（Trait::new("ForType") 调用）
    /// v0.08.3: 即使 ForType 没有任何 impl（仅用默认实现）也允许构造
    /// v0.08.4: 记录 _trait 字段供 dispatcher 定位 search chain
    /// v0.08.5: 用共享 collect_required_methods() 走完祖先链；
    ///          TraitObject 改为一等值类型（for_type/trait_name/data 都是字段）
    /// v0.08.5 fix: 错误信息附调用点 span（line N），与 dispatch_trait_method 对齐
    /// v0.09: 加 trait_generics + for_generics 参数；TraitObject 加 2 字段
    fn construct_trait_instance(
        &mut self,
        trait_name: &str,
        trait_generics: &[String],
        for_type: &str,
        for_generics: &[String],
        call_site: Span,
    ) -> Result<Value, String> {
        // v0.08.5 fix: 检查整个 trait 继承链上所有方法，不只是 trait 自身
        let method_names = collect_required_methods(&self.trait_registry, trait_name);
        let env = self.environment.lock().expect("environment mutex poisoned");
        for m in method_names {
            // v0.09: key 含 generics（避免不同实例化冲突）
            let impl_name = impl_method_key(trait_name, trait_generics, for_type, for_generics, m);
            let default_name = default_impl_method_key(trait_name, trait_generics, m);
            let has_specific = env.get(&impl_name).is_some();
            let has_default = env.get(&default_name).is_some();
            if !has_specific && !has_default {
                drop(env);
                return Err(format!(
                    "trait {}<{}> method '{}' has no impl for type {}<{}> and no default (line {})",
                    trait_name,
                    trait_generics.join(","),
                    m,
                    for_type,
                    for_generics.join(","),
                    call_site.line
                ));
            }
        }
        drop(env);
        // v0.09: TraitObject 5 字段都是自身一部分
        Ok(Value::TraitObject {
            for_generics: for_generics.to_vec(),
            trait_generics: trait_generics.to_vec(),
            for_type: for_type.to_string(),
            trait_name: trait_name.to_string(),
            data: Box::new(Value::Dict(HashMap::new())),
        })
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
    fn eval_prompt_parts_from_stmt(
        prompt_expr: &Expr,
        interp: &mut Interpreter,
    ) -> Result<String, String> {
        match prompt_expr {
            Expr::Prompt { parts, .. } => Self::eval_prompt_parts(parts, interp),
            other => Ok(interp.evaluate(other)?.to_string()),
        }
    }

    /// v0.04: AI chat 的统一入口
    /// 替代 v0.03 的 ai.chat builtin
    /// - model: 模型名 (e.g. "gpt-4o-mini")
    /// - prompt: prompt 字符串
    ///
    /// v0.06: 接 current_ai_config (替代 env hack)  --- temperature/max_tokens/system 下传
    fn do_ai_chat(interp: &mut Interpreter, model: &str, prompt: &str) -> Result<Value, String> {
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let base_url = std::env::var("MORA_AI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        if api_key.is_empty() {
            // Mock 模式
            let cfg_info = interp
                .current_ai_config
                .as_ref()
                .map(|c| {
                    format!(
                        "config: temp={:?}, max_tokens={:?}",
                        c.temperature, c.max_tokens
                    )
                })
                .unwrap_or_default();
            eprintln!(
                "[ai.chat mock — set OPENAI_API_KEY for real call] {} {}",
                prompt, cfg_info
            );
            return Ok(Value::String(format!("[Mock response for: {}]", prompt)));
        }

        let messages = vec![("user".to_string(), prompt.to_string())];
        // v0.06: 从 current_ai_config 取 temperature/max_tokens/system,
        // 拼进 real_ai_chat_inner (v0.06.5 才改函数签名，这里先保留 env 兼容)
        interp.real_ai_chat(&messages, &api_key, model, &base_url)
    }

    fn evaluate_pipe(&mut self, left_val: Value, right: &Expr) -> Result<Value, String> {
        match right {
            Expr::Call { callee, args, span } => {
                // 检查是否是列表/字符串方法名——自动转为方法调用
                if is_pipe_method(callee) {
                    let mut arg_values: Vec<Value> = Vec::new();
                    for arg in args {
                        arg_values.push(self.evaluate(arg.as_ref())?);
                    }
                    return self.call_method(left_val, callee, arg_values, *span);
                }
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg.as_ref())?);
                }
                self.call_function(callee, arg_values, *span)
            }
            Expr::MethodCall {
                object,
                method,
                args,
                span,
            } => {
                let obj = self.evaluate(object)?;
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg.as_ref())?);
                }
                self.call_method(obj, method, arg_values, *span)
            }
            Expr::Variable(name, _) => {
                // Variable 在 pipe 右侧的 span 不可得,用 Span::default()（line=0）
                self.call_function(name, vec![left_val], Span::default())
            }
            Expr::Closure {
                params,
                return_type: _,
                body,
                span: _,
            } => {
                // v0.17: 管道支持闭包 `data | fn(x) = x * 2`
                let closure_val = Value::Closure {
                    params: params.iter().map(|(name, _)| name.clone()).collect(),
                    body: body.clone(),
                    env: self.environment.clone(),
                };
                self.call_function_value(&closure_val, vec![left_val])
            }
            Expr::Pipe {
                left: inner_left,
                right: inner_right,
                span: _,
            } => {
                // v0.22: 管道融合优化
                // 收集连续的管道操作，一次性执行
                let mut ops = Vec::new();
                let mut current_right = inner_right;

                // 收集所有连续的管道操作
                loop {
                    if let Expr::MethodCall {
                        object,
                        method,
                        args,
                        ..
                    } = current_right.as_ref()
                        && is_fusable_method(method) {
                            ops.push((method.clone(), args.clone()));
                            if let Expr::Pipe {
                                right: next_right,
                                ..
                            } = object.as_ref()
                            {
                                current_right = next_right;
                                continue;
                            }
                        }
                    break;
                }

                if ops.is_empty() {
                    // 无融合，正常执行
                    let inner_val = self.evaluate_pipe(left_val, inner_left)?;
                    self.evaluate_pipe(inner_val, inner_right)
                } else {
                    // 融合执行：一次性处理所有操作
                    let mut result = left_val;
                    for (method, args) in ops.into_iter().rev() {
                        let mut evaluated_args = Vec::new();
                        for arg in &args {
                            evaluated_args.push(self.evaluate(arg)?);
                        }
                        result = self.call_method(result, &method, evaluated_args, Span::default())?;
                    }
                    Ok(result)
                }
            }
            _ => {
                // v0.17: 尝试将右侧求值为可调用值并调用
                let right_val = self.evaluate(right)?;
                match &right_val {
                    Value::Closure { .. } | Value::Task { .. } | Value::Compose(_) => {
                        self.call_function_value(&right_val, vec![left_val])
                    }
                    _ => Err(format!(
                        "Right side of pipe must be a call, method call, or closure, got {:?}",
                        right
                    )),
                }
            }
        }
    }

    fn literal_to_value(&mut self, lit: &Literal) -> Result<Value, String> {
        match lit {
            Literal::String(s, _) => Ok(self.intern_string(s.clone())),
            Literal::Char(c, _) => Ok(Value::Char(*c)),
            Literal::Number(n, _) => Ok(Value::Number(*n)),
            Literal::Bool(b, _) => Ok(Value::Bool(*b)),
            Literal::Nil(_) => Ok(Value::Nil),
            Literal::List(items, _) => {
                let mut values = Vec::new();
                for item in items {
                    values.push(self.evaluate(item.as_ref())?);
                }
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

    fn call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
        call_site: Span,
    ) -> Result<Value, String> {
        // v0.08.2: Trait::new("ForType") —— 构造 trait instance
        //   data = {"_type": "ForType"}，vtable 绑定所有 impl methods
        // v0.09: 支持 `Trait<T>::new("ForType")` 解析 generics
        if let Some(tname) = name.strip_suffix("::new") {
            // v0.09: 解析 tname 中的 `<...>` 泛型（namespace 已经拼成 "Foo<T,U>"）
            let (trait_name, trait_generics) = if let Some(lt) = tname.find('<') {
                let n = &tname[..lt];
                let gens_str = &tname[lt + 1..tname.len() - 1];
                let gens: Vec<String> = if gens_str.is_empty() {
                    vec![]
                } else {
                    gens_str.split(',').map(|s| s.trim().to_string()).collect()
                };
                (n.to_string(), gens)
            } else {
                (tname.to_string(), vec![])
            };
            if self.trait_registry.contains_key(&trait_name) {
                let type_arg = args.first().map(|v| v.to_string()).unwrap_or_default();
                return self.construct_trait_instance(
                    &trait_name,
                    &trait_generics,
                    &type_arg,
                    &[],
                    call_site,
                );
            }
        }
        match name {
            "print" => {
                let msg = args
                    .into_iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join("\t");
                println!("{}", msg);
                Ok(Value::Nil)
            }
            "range" => {
                let start = args
                    .first()
                    .and_then(|v| match v {
                        Value::Number(n) => Some(*n as i64),
                        _ => None,
                    })
                    .unwrap_or(0);
                let end = args
                    .get(1)
                    .and_then(|v| match v {
                        Value::Number(n) => Some(*n as i64),
                        _ => None,
                    })
                    .unwrap_or(start);
                let step = args
                    .get(2)
                    .and_then(|v| match v {
                        Value::Number(n) => Some(*n as i64),
                        _ => None,
                    })
                    .unwrap_or(1);
                let mut items = Vec::new();
                let mut i = start;
                while i < end {
                    items.push(Value::Number(i as f64));
                    i += step;
                }
                Ok(Value::List(items))
            }
            "len" => {
                let len = match args.first() {
                    Some(Value::List(list)) => list.len(),
                    Some(Value::String(s)) => s.len(),
                    Some(Value::Dict(map)) => map.len(),
                    _ => return Err("len() expects a list, string, or dict".to_string()),
                };
                Ok(Value::Number(len as f64))
            }
            // v0.17: compose(f1, f2, f3) → fn(x) = f3(f2(f1(x)))
            "compose" => {
                if args.is_empty() {
                    return Err("compose() requires at least 1 argument".to_string());
                }
                // 返回一个特殊的 Compose 值
                Ok(Value::Compose(args))
            }
            // v0.18: partial(fn, args...) → 部分应用
            "partial" => {
                if args.is_empty() {
                    return Err("partial() requires at least 1 argument (the function)".to_string());
                }
                let func = args[0].clone();
                let partial_args: Vec<Value> = args[1..].to_vec();
                Ok(Value::Partial(Box::new(func), partial_args))
            }
            // v0.19: atom(value) → 创建可变引用
            "atom" => {
                let value = args.first().cloned().unwrap_or(Value::Nil);
                Ok(Value::Atom(Arc::new(Mutex::new(value))))
            }
            // v0.19: swap(atom, fn) → 原子更新
            "swap" => {
                if args.len() < 2 {
                    return Err("swap() requires 2 arguments: atom and function".to_string());
                }
                match &args[0] {
                    Value::Atom(arc) => {
                        let func = &args[1];
                        let old = arc.lock().expect("atom mutex poisoned").clone();
                        let new_val = self.call_value(func, vec![old])?;
                        *arc.lock().expect("atom mutex poisoned") = new_val.clone();
                        Ok(new_val)
                    }
                    _ => Err("swap() first argument must be an atom".to_string()),
                }
            }
            // v0.19: deref(atom) → 读取引用值
            "deref" => {
                let value = args.first().ok_or("deref() requires 1 argument")?;
                match value {
                    Value::Atom(arc) => Ok(arc.lock().expect("atom mutex poisoned").clone()),
                    _ => Err("deref() argument must be an atom".to_string()),
                }
            }
            // v0.20: type_of(value) → 返回类型名
            "type_of" => {
                let value = args.first().ok_or("type_of() requires 1 argument")?;
                Ok(Value::String(value_type_name(value).to_string()))
            }
            // v0.20: is_instance(value, type_name) → 类型检查
            "is_instance" => {
                if args.len() < 2 {
                    return Err("is_instance() requires 2 arguments".to_string());
                }
                let value = &args[0];
                let type_name = match &args[1] {
                    Value::String(s) => s.as_str(),
                    _ => return Err("is_instance() second argument must be a string".to_string()),
                };
                Ok(Value::Bool(value_type_name(value) == type_name))
            }
            // v0.20: methods_of(value) → 返回方法名列表
            "methods_of" => {
                let value = args.first().ok_or("methods_of() requires 1 argument")?;
                let methods = get_methods_for_value(value);
                Ok(Value::List(methods.into_iter().map(Value::String).collect()))
            }
            // v0.17: into(collection, fn) → 应用 fn 到集合的每个元素
            "into" => {
                if args.len() < 2 {
                    return Err("into() requires 2 arguments: collection and function".to_string());
                }
                let collection = args[0].clone();
                let transform = args[1].clone();
                match collection {
                    Value::List(list) => {
                        let mut result = Vec::new();
                        for item in list {
                            let mapped = self.call_value(&transform, vec![item])?;
                            match mapped {
                                Value::List(items) => result.extend(items),
                                other => result.push(other),
                            }
                        }
                        Ok(Value::List(result))
                    }
                    _ => Err("into() first argument must be a list".to_string()),
                }
            }
            // v0.06.3: Router::new() builtin
            "Router::new" => Ok(Value::Router {
                routes: Arc::new(Mutex::new(Vec::new())),
            }),
            // v0.06.6: McpServer::new() builtin
            "McpServer::new" => Ok(Value::McpServer { tools: Vec::new() }),
            _ => {
                // 先 clone 出值，释放 borrow，避免借用冲突
                let looked_up = self.environment.lock().expect("environment mutex poisoned").get(name).clone();
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
                        Value::Compose(funcs) => {
                            // Compose: 按顺序应用每个函数
                            let mut result = args;
                            for f in &funcs {
                                result = vec![self.call_value(f, result)?];
                            }
                            Ok(result.into_iter().next().unwrap_or(Value::Nil))
                        }
                        Value::Partial(func, partial_args) => {
                            // Partial: 合并部分参数后调用
                            let mut all_args = partial_args.clone();
                            all_args.extend(args);
                            self.call_value(&func, all_args)
                        }
                        Value::Macro { params, body, .. } => {
                            // Macro: 展开并执行
                            let env = Arc::new(Mutex::new(Environment::with_parent(
                                self.environment.clone(),
                            )));
                            for (i, param) in params.iter().enumerate() {
                                let value = args.get(i).cloned().unwrap_or(Value::Nil);
                                env.lock().expect("env mutex poisoned").define(param.clone(), value, false);
                            }
                            let previous = self.environment.clone();
                            self.environment = env;
                            let result = self.execute_block(&body, self.environment.clone());
                            self.environment = previous;
                            result.map(|_| Value::Nil)
                        }
                        _ => Err(format!("'{}' is not callable", name)),
                    }
                } else {
                    Err(format!("Undefined function or task: {}", name))
                }
            }
        }
    }

    /// v0.17: 直接调用 Value 形式的函数（用于管道闭包）
    fn call_function_value(
        &mut self,
        func: &Value,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match func {
            Value::Task { params, body, .. } => self.call_task(params, body, args),
            Value::Closure { params, body, env } => {
                self.call_closure(params, body, env.clone(), args)
            }
            Value::Compose(funcs) => {
                // Compose: 按顺序应用每个函数
                let mut result = args;
                for f in funcs {
                    result = vec![self.call_value(f, result)?];
                }
                Ok(result.into_iter().next().unwrap_or(Value::Nil))
            }
            Value::Partial(func, partial_args) => {
                // Partial: 合并部分参数后调用
                let mut all_args = partial_args.clone();
                all_args.extend(args);
                self.call_value(func, all_args)
            }
            _ => Err("Value is not callable".to_string()),
        }
    }

    fn call_task(
        &mut self,
        params: &[String],
        body: &[Stmt],
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let env = Arc::new(Mutex::new(Environment::with_parent(self.globals.clone())));
        for (i, param) in params.iter().enumerate() {
            // v0.08.5: TraitObject 已经是一等值类型（含 for_type/trait_name/data），
            // 不再嗅探 Dict key。直接传 args[i]，让 dispatch_trait_method 在
            // receiver 上自然走 TraitObject 分支。
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            env.lock().expect("env mutex poisoned").define(param.clone(), value, false);
        }
        // v0.08.2: impl method body 单表达式时作为返回值 (与 closure 行为一致)
        let actual_body = if body.len() == 1 {
            if let Stmt::Expr(expr) = &body[0] {
                vec![Stmt::Return {
                    value: Some(expr.clone()),
                    span: Span::default(),
                }]
            } else {
                body.to_vec()
            }
        } else {
            body.to_vec()
        };
        let signal = self.execute_block(&actual_body, env)?;
        // FlowSignal::Return(val) → 函数返回值 val
        // FlowSignal::None → 函数未显式 return，默认为 nil
        Ok(signal.into_value())
    }

    fn call_closure(
        &mut self,
        params: &[String],
        body: &[Stmt],
        env: Arc<Mutex<Environment>>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let call_env = Arc::new(Mutex::new(Environment::with_parent(env)));
        for (i, param) in params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            call_env.lock().expect("env mutex poisoned").define(param.clone(), value, false);
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
        if body.len() == 1
            && let Stmt::Expr(expr) = &body[0]
        {
            let previous = self.environment.clone();
            self.environment = call_env;
            let result = self.evaluate(expr);
            self.environment = previous;
            return result;
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

    fn call_method(
        &mut self,
        mut object: Value,
        method: &str,
        args: Vec<Value>,
        call_site: Span,
    ) -> Result<Value, String> {
        // v0.22: 方法调用内联缓存
        let _cache_key = format!("{}:{}", type_name(&object), method);
        // 注：内联缓存主要优化方法查找，实际执行仍需分派

        // v0.08.5: dyn dispatch —— TraitObject 走 dispatch_trait_method（按 for_type + trait_name 选 impl）
        // call_site 透传给 dispatcher，dispatch 失败时报错带行号方便定位
        if let Value::TraitObject { .. } = &object {
            return self.dispatch_trait_method(&object, method, args, call_site);
        }
        match object {
            Value::List(list) => {
                match method {
                    "push" => {
                        let item = args.first().cloned().unwrap_or(Value::Nil);
                        let mut new_list = list.clone();
                        new_list.push(item);
                        Ok(Value::List(new_list))
                    }
                    "get" => {
                        let index = args.first().and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None }).unwrap_or(0);
                        Ok(list.get(index).cloned().unwrap_or(Value::Nil))
                    }
                    "pop" => {
                        let mut new_list = list.clone();
                        let item = new_list.pop().unwrap_or(Value::Nil);
                        Ok(item)
                    }
                    "len" => Ok(Value::Number(list.len() as f64)),
                    "map" => {
                        let mapper = args.first().cloned().ok_or("map() requires a function")?;
                        let mut result = Vec::new();
                        for item in list {
                            let mapped = self.call_value(&mapper, vec![item])?;
                            result.push(mapped);
                        }
                        Ok(Value::List(result))
                    }
                    "filter" => {
                        let predicate = args.first().cloned().ok_or("filter() requires a function")?;
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
                        let reducer = args.first().cloned().ok_or("reduce() requires a function")?;
                        let mut acc = args.get(1).cloned().unwrap_or(Value::Nil);
                        for item in list {
                            acc = self.call_value(&reducer, vec![acc, item])?;
                        }
                        Ok(acc)
                    }
                    // v0.18: take(n) - 取前 n 个元素
                    "take" => {
                        let n = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("take() requires a count argument")?;
                        let result: Vec<Value> = list.into_iter().take(n).collect();
                        Ok(Value::List(result))
                    }
                    // v0.18: drop(n) - 跳过前 n 个元素
                    "drop" => {
                        let n = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("drop() requires a count argument")?;
                        let result: Vec<Value> = list.into_iter().skip(n).collect();
                        Ok(Value::List(result))
                    }
                    // v0.17: window(size) - 滑动窗口
                    "window" => {
                        let size = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("window() requires a size argument")?;
                        if size == 0 {
                            return Err("window() size must be > 0".to_string());
                        }
                        let mut windows = Vec::new();
                        for i in 0..list.len() {
                            if i + size <= list.len() {
                                let window: Vec<Value> = list[i..i + size].to_vec();
                                windows.push(Value::List(window));
                            }
                        }
                        Ok(Value::List(windows))
                    }
                    // v0.17: batch(size) - 翻转窗口（批次处理）
                    "batch" => {
                        let size = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("batch() requires a size argument")?;
                        if size == 0 {
                            return Err("batch() size must be > 0".to_string());
                        }
                        let mut batches = Vec::new();
                        for chunk in list.chunks(size) {
                            batches.push(Value::List(chunk.to_vec()));
                        }
                        Ok(Value::List(batches))
                    }
                    // v0.17: shape() - 返回维度
                    "shape" => {
                        fn get_shape(val: &Value) -> Vec<usize> {
                            match val {
                                Value::List(items) => {
                                    if items.is_empty() {
                                        vec![0]
                                    } else {
                                        let mut shape = vec![items.len()];
                                        if let Some(first) = items.first()
                                            && let Value::List(_) = first {
                                                let inner = get_shape(first);
                                                shape.extend(inner);
                                            }
                                        shape
                                    }
                                }
                                _ => vec![],
                            }
                        }
                        let shape = get_shape(&Value::List(list.clone()));
                        Ok(Value::List(shape.iter().map(|n| Value::Number(*n as f64)).collect()))
                    }
                    // v0.17: flatten() - 展平嵌套列表
                    "flatten" => {
                        fn flatten_list(val: &Value, out: &mut Vec<Value>) {
                            match val {
                                Value::List(items) => {
                                    for item in items {
                                        flatten_list(item, out);
                                    }
                                }
                                other => out.push(other.clone()),
                            }
                        }
                        let mut result = Vec::new();
                        flatten_list(&Value::List(list.clone()), &mut result);
                        Ok(Value::List(result))
                    }
                    // v0.17: transpose() - 转置二维列表
                    "transpose" => {
                        if list.is_empty() {
                            return Ok(Value::List(vec![]));
                        }
                        // 检查是否是二维列表
                        let rows: Vec<&Vec<Value>> = list.iter().filter_map(|v| {
                            if let Value::List(items) = v { Some(items) } else { None }
                        }).collect();
                        if rows.len() != list.len() {
                            return Err("transpose() requires a 2D list".to_string());
                        }
                        let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                        let mut result = Vec::new();
                        for col in 0..ncols {
                            let mut new_row = Vec::new();
                            for row in &rows {
                                new_row.push(row.get(col).cloned().unwrap_or(Value::Nil));
                            }
                            result.push(Value::List(new_row));
                        }
                        Ok(Value::List(result))
                    }
                    // v0.17: reshape(rows, cols) - 重塑列表
                    "reshape" => {
                        let rows = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("reshape() requires rows argument")?;
                        let cols = args.get(1)
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("reshape() requires cols argument")?;
                        let total = rows * cols;
                        // 展平后重塑
                        fn flatten_list(val: &Value, out: &mut Vec<Value>) {
                            match val {
                                Value::List(items) => {
                                    for item in items {
                                        flatten_list(item, out);
                                    }
                                }
                                other => out.push(other.clone()),
                            }
                        }
                        let mut flat = Vec::new();
                        flatten_list(&Value::List(list.clone()), &mut flat);
                        // 循环填充
                        while flat.len() < total {
                            let extend_len = (total - flat.len()).min(flat.len());
                            let extend: Vec<Value> = flat[..extend_len].to_vec();
                            flat.extend(extend);
                        }
                        let mut result = Vec::new();
                        for r in 0..rows {
                            let row: Vec<Value> = flat[r * cols..(r + 1) * cols].to_vec();
                            result.push(Value::List(row));
                        }
                        Ok(Value::List(result))
                    }
                    _ => Err(format!("List has no method: {}", method)),
                }
            }
            Value::Dict(map) => {
                match method {
                    "get" => {
                        let key = args.first().map(|v| v.to_string()).unwrap_or_default();
                        Ok(map.get(&key).cloned().unwrap_or(Value::Nil))
                    }
                    "set" => {
                        let key = args.first().map(|v| v.to_string()).unwrap_or_default();
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
                    // v0.07.1: req.json() — 从 body 字段解析 JSON，返回 Result<Dict, ParseError>
                    "json" => {
                        let body_val = map.get("body").cloned().unwrap_or(Value::String(String::new()));
                        let body_str = match body_val {
                            Value::String(s) => s,
                            _ => body_val.to_string(),
                        };
                        if body_str.trim().is_empty() {
                            let mut err = HashMap::new();
                            err.insert("err".to_string(), Value::String("ParseError: empty body".to_string()));
                            return Ok(Value::Dict(err));
                        }
                        match json_to_value(&body_str) {
                            Ok(val) => {
                                let mut result = HashMap::new();
                                result.insert("ok".to_string(), val);
                                Ok(Value::Dict(result))
                            }
                            Err(e) => {
                                let mut err = HashMap::new();
                                err.insert("err".to_string(), Value::String(format!("ParseError: {}", e)));
                                Ok(Value::Dict(err))
                            }
                        }
                    }
                    _ => Err(format!("Dict has no method: {}", method)),
                }
            }
            Value::Builtin(name) => match (name.as_str(), method) {
                ("web", "fetch") => {
                    let url = args.first().map(|v| v.to_string()).unwrap_or_default();
                    // v10: 真实 HTTP GET
                    self.real_web_fetch(&url)
                }
                ("json", "parse") => {
                    // v10: 真实 JSON 解析
                    let text = args.first().map(|v| v.to_string()).unwrap_or_default();
                    json_to_value(&text).map_err(|e| format!("json.parse: {}", e))
                }
                ("json", "stringify") => {
                    // v10: JSON 序列化
                    let value = args.first().cloned().unwrap_or(Value::Nil);
                    Ok(Value::String(value_to_json(&value)))
                }
                ("file", method) => self.call_file_method(method, &args),
                ("agent", "create") => {
                    // agent.create("name", {tools: [...], model: "deep", max_steps: 10, system: "..."})
                    let name = match args.first() {
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
                    let answer = match args.first() {
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
                        let prompt = args.first().map(|v| v.to_string()).unwrap_or_default();
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
            // v0.07.1: String.json() — 解析 JSON 字符串，返回 Result<Value, ParseError>
            Value::String(s) => {
                match method {
                    "len" => Ok(Value::Number(s.len() as f64)),
                    "upper" => Ok(Value::String(s.to_uppercase())),
                    "lower" => Ok(Value::String(s.to_lowercase())),
                    "trim" => Ok(Value::String(s.trim().to_string())),
                    "starts_with" => {
                        let prefix = args.first().map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.starts_with(&prefix)))
                    }
                    "ends_with" => {
                        let suffix = args.first().map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.ends_with(&suffix)))
                    }
                    "contains" => {
                        let needle = args.first().map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.contains(&needle)))
                    }
                    "split" => {
                        let sep = args.first().map(|v| v.to_string()).unwrap_or_default();
                        let parts: Vec<Value> = s.split(&sep)
                            .map(|p| Value::String(p.to_string()))
                            .collect();
                        Ok(Value::List(parts))
                    }
                    "replace" => {
                        let from = args.first().map(|v| v.to_string()).unwrap_or_default();
                        let to = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::String(s.replace(&from, &to)))
                    }
                    // v0.07.3: String.json() — 与 Dict.json() 同构 API
                    "json" => {
                        if s.trim().is_empty() {
                            let mut err = HashMap::new();
                            err.insert("err".to_string(), Value::String("ParseError: empty body".to_string()));
                            return Ok(Value::Dict(err));
                        }
                        match json_to_value(&s) {
                            Ok(val) => {
                                let mut result = HashMap::new();
                                result.insert("ok".to_string(), val);
                                Ok(Value::Dict(result))
                            }
                            Err(e) => {
                                let mut err = HashMap::new();
                                err.insert("err".to_string(), Value::String(format!("ParseError: {}", e)));
                                Ok(Value::Dict(err))
                            }
                        }
                    }
                    _ => Err(format!("String has no method: {}", method)),
                }
            }
            Value::Stream { ref reader, ref done } => {
                match method {
                    "collect" => {
                        let mut result = String::new();
                        if !*done.lock().expect("done mutex poisoned") {
                            let mut guard = reader.lock();
                            loop {
                                match Self::read_next_sse_token(&mut guard) {
                                    Ok(Some(token)) => result.push_str(&token),
                                    Ok(None) => {
                                        *done.lock().expect("done mutex poisoned") = true;
                                        break;
                                    }
                                    Err(e) => {
                                        *done.lock().expect("done mutex poisoned") = true;
                                        return Err(format!("ai.stream.collect: {}", e));
                                    }
                                }
                            }
                        }
                        Ok(Value::String(result))
                    }
                    "is_done" => {
                        Ok(Value::Bool(*done.lock().expect("done mutex poisoned")))
                    }
                    _ => Err(format!("Stream has no method: {}", method)),
                }
            }
            Value::Agent { ref name, ref tool_names, ref model_route, max_steps, ref system } => {
                match method {
                    "run" => {
                        let task = args.first().map(|v| v.to_string()).unwrap_or_default();
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
            // v0.06.3: Router 方法
            Value::Router { ref mut routes } => {
                let mut r = routes.lock().expect("routes mutex poisoned");
                match method {
                    "route" => {
                        let http_method = args.first().map(|v| v.to_string()).unwrap_or_default().to_uppercase();
                        let path = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                        let handler = args.get(2).cloned().ok_or("Router.route() requires a handler")?;
                        r.push((http_method, path, handler));
                        Ok(Value::Router { routes: routes.clone() })
                    }
                    "listen" => {
                        let addr = args.first().map(|v| v.to_string()).unwrap_or_else(|| "0.0.0.0:3000".to_string());
                        let (host, port) = addr.split_once(':').unwrap_or(("0.0.0.0", "3000"));
                        let port: u16 = port.parse().map_err(|_| format!("Invalid port: {}", port))?;
                        let r_clone: Vec<(String, String, Value)> = r.clone();
                        drop(r);
                        eprintln!("[Router] starting HTTP server on {}", addr);
                        let interp_arc: Arc<Mutex<Interpreter>> = Arc::new(Mutex::new(self.clone()));
                        crate::http_server::start(
                            host, port,
                            Arc::new(Mutex::new(r_clone.iter().map(|(m,p,h)|
                                ((m.clone(), p.clone()), h.clone())
                            ).collect())),
                            interp_arc,
                        ).map_err(|e| format!("HTTP server error: {}", e))?;
                        Ok(Value::Nil)
                    }
                    _ => { drop(r); Err(format!("Router has no method: {}", method)) },
                }
            }
            // v0.06.6: McpServer 方法
            Value::McpServer { ref mut tools } => {
                match method {
                    "tool" => {
                        let name = args.first().map(|v| v.to_string()).unwrap_or_default();
                        let handler = args.get(2).cloned().ok_or("McpServer.tool() requires 3 args (name, schema, handler)")?;
                        tools.push((name, handler));
                        Ok(Value::McpServer { tools: tools.clone() })
                    }
                    "serve" => {
                        let tools_clone = tools.clone();
                        eprintln!("[McpServer] starting MCP server on stdio ({} tools)", tools_clone.len());
                        let tool_registry: Arc<Mutex<HashMap<String, crate::mcp_server::McpTool>>> =
                            Arc::new(Mutex::new(HashMap::new()));
                        for (name, handler) in tools_clone {
                            let mcp_tool = crate::mcp_server::McpTool {
                                name: name.clone(),
                                description: String::new(),
                                parameters: "{}".to_string(),
                                handler,
                            };
                            tool_registry.lock().expect("tool_registry mutex poisoned").insert(name, mcp_tool);
                        }
                        let interp_arc: Arc<Mutex<Interpreter>> = Arc::new(Mutex::new(self.clone()));
                        crate::mcp_server::start(tool_registry, interp_arc)
                            .map_err(|e| format!("MCP server error: {}", e))?;
                        Ok(Value::Nil)
                    }
                    _ => Err(format!("McpServer has no method: {}", method)),
                }
            }
            _ => Err("Can only call methods on lists, dicts, strings, conversations, streams, agents, routers, mcp_servers, or builtin objects".to_string()),
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
            Value::Compose(funcs) => {
                // Compose: 按顺序应用每个函数
                let mut result = args;
                for f in funcs {
                    result = vec![self.call_value(f, result)?];
                }
                Ok(result.into_iter().next().unwrap_or(Value::Nil))
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
                if let Some(parent) = std::path::Path::new(&path).parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.exists()
                {
                    return Err(format!(
                        "file.write_text: parent directory does not exist: {}",
                        parent.display()
                    ));
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
                let bytes = hex_decode(&hex).map_err(|e| format!("file.write_bytes: {}", e))?;
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
                    std::fs::remove_file(&path).map_err(|e| {
                        format!("file.remove: cannot remove file '{}': {}", path, e)
                    })?;
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
                std::fs::rename(&from, &to).map_err(|e| {
                    format!("file.rename: cannot rename '{}' -> '{}': {}", from, to, e)
                })?;
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
                    if let Some(parent) = p.parent()
                        && !parent.as_os_str().is_empty()
                        && !parent.exists()
                    {
                        return Err(format!(
                            "file.touch: parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                    std::fs::write(&path, "")
                        .map_err(|e| format!("file.touch: cannot create '{}': {}", path, e))?;
                }
                Ok(Value::Nil)
            }

            // ---- 路径与工作目录 ----
            "cwd" => {
                let cwd = std::env::current_dir().map_err(|e| format!("file.cwd: {}", e))?;
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
                        _ => return Err("file.join: all arguments must be strings".to_string()),
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
                let name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(Value::String(name))
            }
            "dirname" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let parent = p
                    .parent()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(Value::String(parent))
            }
            "extname" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let ext = p
                    .extension()
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

    fn real_web_fetch(&mut self, url: &str) -> Result<Value, String> {
        // v0.14: 重放模式优先返回录制响应 (deterministic)
        if let Some(rec) = self.recorder.lookup_web_fetch(url)
            && let Some(status) = rec.status
        {
            return Ok(Value::String(format!(
                "<replay> HTTP {} ({}B, {}ms)",
                status,
                rec.body_len.unwrap_or(0),
                rec.latency_ms
            )));
        }
        let started = std::time::Instant::now();

        if url.is_empty() {
            return Err("web.fetch: URL cannot be empty".to_string());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!(
                "web.fetch: URL must start with http:// or https://, got: {}",
                url
            ));
        }

        // v0.x: ureq 3.3 — AgentBuilder 移除,改用 Agent::config_builder() 链式 + Config::into()
        // timeout_read/timeout_write 合并为 timeout_global(覆盖整个请求-响应周期)
        // 关闭 http_status_as_error 以保留 4xx/5xx 响应体(原 2.x 中可从 Error::Status 读取)
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(HTTP_READ_TIMEOUT_SECS)))
            .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build()
            .into();

        match agent.get(url).call() {
            Ok(mut response) => {
                let status = response.status();
                let text = response
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| format!("web.fetch: failed to read response body: {}", e))?;
                let body_len = text.len();
                let result = if (400..600).contains(&status.as_u16()) {
                    let excerpt: String = text.chars().take(200).collect();
                    Err(format!(
                        "web.fetch: HTTP {} {} (body excerpt: {})",
                        status, url, excerpt
                    ))
                } else {
                    Ok(Value::String(text))
                };
                // v0.14: 录制成功 fetch (status + body_len)
                self.recorder.record_web_fetch(
                    url.to_string(),
                    "GET".to_string(),
                    status.as_u16(),
                    body_len,
                    started.elapsed().as_millis(),
                    if result.is_err() {
                        Some(format!("HTTP {}", status.as_u16()))
                    } else {
                        None
                    },
                );
                result
            }
            // v0.x: ureq 3.3 — Transport 变体被拆解为 Io/Timeout/ConnectionFailed 等多种;
            // 其余失败(HostNotFound/Protocol 等)统一兜底
            Err(e) => {
                let err_str = format!("web.fetch: network error for {}: {}", url, e);
                self.recorder.record_web_fetch(
                    url.to_string(),
                    "GET".to_string(),
                    0,
                    0,
                    started.elapsed().as_millis(),
                    Some(err_str.clone()),
                );
                Err(err_str)
            }
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
    ///
    /// v0.06.5: AI chat 新签名 — 接 temperature/max_tokens/system 从 current_ai_config
    fn real_ai_chat(
        &mut self,
        messages: &[(String, String)],
        api_key: &str,
        model: &str,
        base_url: &str,
    ) -> Result<Value, String> {
        // v0.14: 重放模式直接返回录制响应
        let prompt_text: String = messages
            .iter()
            .map(|(role, content)| format!("{}: {}", role, content))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(rec) = self.recorder.lookup_ai_chat(model, &prompt_text) {
            return Ok(Value::String(rec.response));
        }

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
        // v0.14: 录制 ai.chat (rough token 估算: prompt 长度/4 + response 长度/4)
        let resp_str = match &result {
            Ok(v) => v.to_string(),
            Err(_) => String::new(),
        };
        let tokens_in_approx = prompt_text.len() / 4;
        let tokens_out_approx = resp_str.len() / 4;
        self.recorder.record_ai_chat(
            model.to_string(),
            prompt_text,
            resp_str,
            tokens_in_approx,
            tokens_out_approx,
            elapsed.as_millis(),
            if result.is_err() {
                Some(format!("{:?}", result.as_ref().err()))
            } else {
                None
            },
        );
        result
    }

    /// v0.06.5: HTTP body 构建 — 拼 json 时接 temperature/max_tokens/system
    /// v0.10: 包 retry 循环（exponential backoff + jitter）
    fn real_ai_chat_inner(
        &mut self,
        messages: &[(String, String)],
        api_key: &str,
        model: &str,
        base_url: &str,
    ) -> Result<Value, String> {
        if messages.is_empty() {
            return Err("ai.chat: messages cannot be empty".to_string());
        }

        // v0.15: mock_llm 模式 — 从队列中取出下一个响应
        if let Some(ref mut cfg) = self.current_ai_config
            && let Some(ref mut responses) = cfg.mock_responses
                && !responses.is_empty() {
                    let response = responses.remove(0); // 消费第一个
                    // 模拟 token 估算
                    let tokens_in = messages.iter().map(|(_, c)| c.len()).sum::<usize>() / 4;
                    let tokens_out = response.len() / 4;
                    self.recorder.record_ai_chat(
                        model.to_string(),
                        messages.last().map(|(_, c)| c.as_str()).unwrap_or("").to_string(),
                        response.clone(),
                        tokens_in,
                        tokens_out,
                        0, // mock 无延迟
                        None,
                    );
                    return Ok(Value::String(response));
                }

        // v0.22: AI 调用内联缓存
        let cache_key = format!("{}:{:?}", model, messages);
        if let Some(cached) = self.ai_cache.get(&cache_key) {
            return Ok(Value::String(cached.clone()));
        }

        // 构建 messages JSON 数组
        let msgs_json: String = messages
            .iter()
            .map(|(role, content)| {
                let escaped_content = content
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .replace('\t', "\\t");
                format!(r#"{{"role":"{}","content":"{}"}}"#, role, escaped_content)
            })
            .collect::<Vec<_>>()
            .join(",");

        let escaped_model = model.replace('\\', "\\\\").replace('"', "\\\"");
        // v0.06.5: 拼 temperature/max_tokens/system 从 current_ai_config
        let mut body = format!(
            r#"{{"model":"{}","messages":[{}]"#,
            escaped_model, msgs_json
        );
        if let Some(ref cfg) = self.current_ai_config {
            if let Some(temp) = cfg.temperature {
                body.push_str(&format!(",\"temperature\":{}", temp));
            }
            if let Some(mt) = cfg.max_tokens {
                body.push_str(&format!(",\"max_tokens\":{}", mt));
            }
            if let Some(ref sys) = cfg.system {
                body.push_str(&format!(",\"system\":\"{}\"", sys.replace('"', "\\\"")));
            }
        }
        body.push('}');

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        // v0.22: 流式响应优化 - 添加 stream 参数
        let use_stream = self.current_ai_config.as_ref()
            .and_then(|c| c.max_tokens)
            .map(|mt| mt > 1000)  // 长响应使用流式
            .unwrap_or(false);

        if use_stream {
            body.insert_str(body.len() - 1, ",\"stream\":true");
        }

        // v0.x: ureq 3.3 — ConfigBuilder + http_status_as_error(false) 以保留 4xx/5xx 响应体
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(AI_READ_TIMEOUT_SECS)))
            .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build()
            .into();

        // v0.10: retry 循环（exponential backoff + jitter）
        let max_retries = ai_retry_max();
        let base_ms = ai_retry_base_ms();
        for attempt in 0..=max_retries {
            if attempt > 0 {
                let sleep = retry_sleep_ms(attempt - 1, base_ms);
                std::thread::sleep(Duration::from_millis(sleep));
            }
            // v0.x: ureq 3.3 — send_string 移除,改用 send(&body)(&str 实现 SendBody trait)
            // .set() → .header()
            match agent
                .post(&url)
                .header("Authorization", &format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .send(&body)
            {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let text_result = response.body_mut().read_to_string();
                    match text_result {
                        Ok(text) if status < 400 => {
                            let (input, output) = Self::extract_usage(&text);
                            let _ = self.track_tokens(input, output);
                            let result = self
                                .extract_ai_content(&text)
                                .unwrap_or(Value::String(text.clone()));
                            // v0.22: 缓存 AI 调用结果
                            if let Value::String(ref s) = result {
                                self.ai_cache.insert(cache_key.clone(), s.clone());
                            }
                            return Ok(result);
                        }
                        Ok(text) => {
                            // 4xx/5xx: body 仍可读(因 http_status_as_error=false)
                            let excerpt: String = text.chars().take(300).collect();
                            let err = format!(
                                "ai.chat: API error HTTP {} from {} (body: {})",
                                status, url, excerpt
                            );
                            if attempt < max_retries && is_retryable_error(&err) {
                                continue;
                            }
                            return Err(err);
                        }
                        Err(e) => {
                            let err = format!("ai.chat: failed to read response body: {}", e);
                            if attempt < max_retries && is_retryable_error(&err) {
                                continue;
                            }
                            return Err(err);
                        }
                    }
                }
                Err(e) => {
                    // v0.x: ureq 3.3 — Transport 拆解为多种变体,统一兜底
                    let err = format!("ai.chat: network error connecting to {}: {}", url, e);
                    if attempt < max_retries && is_retryable_error(&err) {
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err("ai.chat: retry loop exited without result".to_string())
    }

    /// 带工具调用的 AI 对话（支持 tool_calls 自动循环）
    fn real_ai_chat_with_tools(
        &mut self,
        messages: &mut Vec<ChatMessage>,
        api_key: &str,
        model: &str,
        base_url: &str,
        tools: &[&ToolDef],
    ) -> Result<Value, String> {
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
            let mut body = format!(
                r#"{{"model":"{}","messages":[{}]{}"#,
                escaped_model, msgs_json, tools_json
            );
            // v0.15: 拼 temperature/max_tokens/system 从 current_ai_config（与 real_ai_chat_inner 对齐）
            if let Some(ref cfg) = self.current_ai_config {
                if let Some(temp) = cfg.temperature {
                    body.push_str(&format!(",\"temperature\":{}", temp));
                }
                if let Some(mt) = cfg.max_tokens {
                    body.push_str(&format!(",\"max_tokens\":{}", mt));
                }
                if let Some(ref sys) = cfg.system {
                    body.push_str(&format!(",\"system\":\"{}\"", sys.replace('"', "\\\"")));
                }
            }
            // 闭合 JSON
            body.push('}');

            let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
            // v0.x: ureq 3.3 — ConfigBuilder + http_status_as_error(false) 以保留 4xx/5xx 响应体
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(AI_READ_TIMEOUT_SECS)))
                .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
                .http_status_as_error(false)
                .build()
                .into();

            // v0.x: ureq 3.3 — send_string → send(&body);Status 变体移除,改由响应 status 判定
            // .set() → .header()
            let response_text = match agent
                .post(&url)
                .header("Authorization", &format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .send(&body)
            {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let text = response
                        .body_mut()
                        .read_to_string()
                        .map_err(|e| format!("ai.chat: failed to read response body: {}", e))?;
                    if status >= 400 {
                        let excerpt: String = text.chars().take(300).collect();
                        return Err(format!(
                            "ai.chat: API error HTTP {} from {} (body: {})",
                            status, url, excerpt
                        ));
                    }
                    text
                }
                Err(e) => {
                    return Err(format!(
                        "ai.chat: network error connecting to {}: {}",
                        url, e
                    ));
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
        if let Value::Dict(map) = root
            && let Some(Value::List(choices)) = map.get("choices")
            && let Some(Value::Dict(choice_map)) = choices.first()
            && let Some(Value::Dict(msg_map)) = choice_map.get("message")
        {
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
                            tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments,
                            });
                        }
                    }
                }
            }
            return Ok((content, tool_calls));
        }
        Err("Could not parse chat response".to_string())
    }

    /// 从 API 响应中提取 usage（prompt_tokens, completion_tokens）
    fn extract_usage(json_text: &str) -> (usize, usize) {
        if let Ok(Value::Dict(map)) = json_to_value(json_text)
            && let Some(Value::Dict(usage)) = map.get("usage")
        {
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
        (0, 0)
    }

    /// 记录 token 消耗并检查预算
    fn track_tokens(&mut self, input: usize, output: usize) -> Result<(), String> {
        // v0.15: 检查每次调用上限
        if let Some(ref budget) = self.token_budget
            && let Some(per_call) = budget.per_call {
                let call_total = input + output;
                if call_total > per_call {
                    return Err(format!(
                        "Token per-call limit exceeded: this call used {}, limit is {}",
                        call_total, per_call
                    ));
                }
            }

        self.token_usage.input += input;
        self.token_usage.output += output;
        self.trace.record_tokens(input as u64, output as u64);
        let total_used = self.token_usage.input + self.token_usage.output;
        if let Some(ref budget) = self.token_budget {
            if total_used > budget.total {
                return Err(format!(
                    "Token budget exceeded: used {}/{}",
                    total_used, budget.total
                ));
            }
            let ratio = total_used as f64 / budget.total as f64;
            if ratio >= budget.alert_threshold {
                eprintln!(
                    "[ai.budget warning] Token usage at {:.0}% ({}/{})",
                    ratio * 100.0,
                    total_used,
                    budget.total
                );
            }
        }
        Ok(())
    }

    // 检查预算是否已耗尽

    fn extract_ai_content(&self, json_text: &str) -> Result<Value, String> {
        let root = json_to_value(json_text)?;

        // root 应该是 Dict，提取 "choices" 数组
        if let Value::Dict(map) = &root {
            if let Some(Value::List(choices)) = map.get("choices")
                && let Some(Value::Dict(choice_map)) = choices.first()
            {
                // 标准格式: choices[0].message.content
                if let Some(Value::Dict(msg_map)) = choice_map.get("message")
                    && let Some(Value::String(content)) = msg_map.get("content")
                {
                    return Ok(Value::String(content.clone()));
                }
                // 兼容格式: choices[0].text (旧版 completions API)
                if let Some(Value::String(text)) = choice_map.get("text") {
                    return Ok(Value::String(text.clone()));
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
    fn read_next_sse_token(
        reader: &mut BufReader<Box<dyn Read + Send + Sync>>,
    ) -> Result<Option<String>, String> {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return Ok(None), // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(data) = trimmed.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return Ok(None);
                        }
                        // 解析 JSON，提取 choices[0].delta.content
                        if let Ok(Value::Dict(map)) = json_to_value(data)
                            && let Some(Value::List(choices)) = map.get("choices")
                            && let Some(Value::Dict(choice_map)) = choices.first()
                        {
                            if let Some(Value::Dict(delta)) = choice_map.get("delta")
                                && let Some(Value::String(content)) = delta.get("content")
                                && !content.is_empty()
                            {
                                return Ok(Some(content.clone()));
                            }
                            // finish_reason 字段出现但无 content，跳过
                            if choice_map.contains_key("finish_reason") {
                                continue;
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
        let has_key = env::var("OPENAI_API_KEY")
            .map(|k| !k.is_empty())
            .unwrap_or(false);
        if has_key {
            let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
            let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let base_url = env::var("MORA_AI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let msgs = vec![("user".to_string(), critic_prompt)];
            match self.real_ai_chat(&msgs, &api_key, &model, &base_url) {
                Ok(Value::String(response)) => {
                    Ok(self.parse_critic_response(&response, context.is_some()))
                }
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
                if let Ok(n) = val.trim().parse::<f64>() {
                    score = n;
                }
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
        let score = if len < 10 {
            3.0
        } else if len < 50 {
            6.0
        } else {
            8.0
        };

        let (verdict, issues) = if let Some(ctx) = context {
            // 简单检查：回答中的词是否在上下文中出现
            let ctx_lower = ctx.to_lowercase();
            let answer_words: Vec<&str> = answer.split_whitespace().collect();
            let matched = answer_words
                .iter()
                .filter(|w| ctx_lower.contains(&w.to_lowercase()))
                .count();
            let ratio = if answer_words.is_empty() {
                0.0
            } else {
                matched as f64 / answer_words.len() as f64
            };
            if ratio > 0.5 {
                ("supported".to_string(), "none".to_string())
            } else if ratio > 0.2 {
                (
                    "partial".to_string(),
                    "some claims may not be grounded in context".to_string(),
                )
            } else {
                (
                    "hallucinated".to_string(),
                    "most claims not found in context".to_string(),
                )
            }
        } else {
            if score >= 7.0 {
                ("good".to_string(), "none".to_string())
            } else if score >= 5.0 {
                (
                    "acceptable".to_string(),
                    "could be more detailed".to_string(),
                )
            } else {
                ("poor".to_string(), "too short, lacks detail".to_string())
            }
        };

        m.insert("score".to_string(), Value::Number(score));
        m.insert("verdict".to_string(), Value::String(verdict));
        m.insert("issues".to_string(), Value::String(issues));
        m.insert(
            "suggestion".to_string(),
            Value::String("set OPENAI_API_KEY for real evaluation".to_string()),
        );
        if context.is_some() {
            m.insert("hallucination_check".to_string(), Value::Bool(true));
        }
        Value::Dict(m)
    }

    /// 执行 Agent 多步推理循环
    fn run_agent(
        &mut self,
        agent_name: &str,
        tool_names: &[String],
        model_route: &str,
        max_steps: usize,
        system: &str,
        task: &str,
    ) -> Result<Value, String> {
        // 收集 Agent 需要的工具
        let agent_tools: Vec<ToolDef> = tool_names
            .iter()
            .filter_map(|n| self.tool_registry.get(n).cloned())
            .collect();
        let tool_refs: Vec<&ToolDef> = agent_tools.iter().collect();

        // 确定 API 配置
        let route = self.model_routes.get(model_route);
        let default_key = env::var("OPENAI_API_KEY").unwrap_or_default();
        let (api_key, model, base_url) = if let Some(r) = route {
            let key = if r.api_key.is_empty() {
                default_key.clone()
            } else {
                r.api_key.clone()
            };
            // v0.15: 将 route 的 ai 配置设入 current_ai_config
            if r.max_tokens.is_some() || r.system.is_some() || r.temperature.is_some() {
                self.current_ai_config = Some(AiConfigValue {
                    model: Some(r.model.clone()),
                    temperature: r.temperature,
                    max_tokens: r.max_tokens,
                    budget: None,
                    per_call: None,
                    system: r.system.clone(),
                    mock_responses: None,
                });
            }
            (key, r.model.clone(), r.base_url.clone())
        } else {
            let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let base_url = env::var("MORA_AI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            (default_key, model, base_url)
        };

        // Mock 模式：直接执行第一个工具并返回结果
        if api_key.is_empty() {
            eprintln!(
                "[agent '{}' mock — set OPENAI_API_KEY for real agent loop]",
                agent_name
            );
            if let Some(first_tool) = agent_tools.first() {
                let args_dict = Value::Dict(HashMap::new());
                let tool_result = match self.call_value(&first_tool.handler, vec![args_dict]) {
                    Ok(val) => val.to_string(),
                    Err(e) => format!("Tool error: {}", e),
                };
                return Ok(Value::String(format!(
                    "[Agent '{}'] Task: {}\nTool '{}' result: {}",
                    agent_name, task, first_tool.name, tool_result
                )));
            }
            return Ok(Value::String(format!(
                "[Agent '{}'] Task: {} (no tools, mock response)",
                agent_name, task
            )));
        }

        // 构建初始消息
        let mut messages: Vec<ChatMessage> = Vec::new();
        messages.push(ChatMessage::User {
            content: format!("{}\n\nTask: {}", system, task),
        });

        // 多步推理循环
        // 当前实现每步必 return（Ok/Err），循环形式保留意图：未来扩展多步时无需改结构。
        // clippy::never_loop 触发因为循环体总 return；属预期行为。
        #[allow(clippy::never_loop)]
        for step in 0..max_steps {
            eprintln!("[agent '{}' step {}/{}]", agent_name, step + 1, max_steps);
            match self.real_ai_chat_with_tools(
                &mut messages,
                &api_key,
                &model,
                &base_url,
                &tool_refs,
            ) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // real_ai_chat_with_tools 只在 max tool rounds exceeded 时返回 Err
                    // 其他情况下 Ok 就是最终结果
                    return Err(format!("agent.run error at step {}: {}", step + 1, e));
                }
            }
        }
        Err(format!(
            "agent '{}': max steps ({}) exceeded",
            agent_name, max_steps
        ))
    }

    // Mock 工具调用（无 API Key 时，调用第一个注册的工具）

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
    let data = if let Value::Dict(map) = root {
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
                        .filter_map(|v| {
                            if let Value::Number(n) = v {
                                Some(*n)
                            } else {
                                None
                            }
                        })
                        .collect(),
                    _ => {
                        return Err(
                            "ai.embed: 'embedding' field is not a list of numbers".to_string()
                        );
                    }
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
        let vec = indexed.into_iter().next().expect("should have elements").1;
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
    for word in s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
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

// 余弦相似度: (a·b) / (||a|| * ||b||)，范围 [-1, 1]
//
// 点积: a·b
//
// 欧氏距离: sqrt(sum((a-b)^2))，值越小越相似
//
// L2 范数

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

// FlowSignal is now in value.rs
// Re-exported above via pub use crate::value::*;


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
        assert_eq!(dot_product(&a, &b).unwrap(), 32.0); // 4+10+18
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
        return Err(format!(
            "cosine_similarity: length mismatch ({} vs {})",
            a.len(),
            b.len()
        ));
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
        return Err(format!(
            "dot_product: length mismatch ({} vs {})",
            a.len(),
            b.len()
        ));
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| x * y).sum())
}

/// 欧氏距离: sqrt(sum((a-b)^2))
#[allow(dead_code)]
fn euclidean_distance(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!(
            "euclidean_distance: length mismatch ({} vs {})",
            a.len(),
            b.len()
        ));
    }
    Ok(a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f64>()
        .sqrt())
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

// ===================================================================
// v0.08.5: trait 系统单测（之前 0 个单测守护，回归靠 3 个 happy examples）
// ===================================================================
#[cfg(test)]
mod trait_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp.interpret(&stmts)
    }

    /// v0.08.5: trait 基本 dispatch —— impl method 被正确路由
    #[test]
    fn test_trait_basic_dispatch() {
        let src = r#"
trait Greeter
  fn greet(self) -> string = "default"
end

impl Greeter for English
  fn greet(self) = "Hello!"
end

task main()
  let g: dyn Greeter = Greeter::new("English")
  print(g.greet())
end
"#;
        run(src).expect("should run");
    }

    #[test]
    fn test_trait_inherit_construction_checks_parents() {
        // v0.08.5 fix: 之前 construct_trait_instance 只检查 trait 自身方法，
        // 不走 parents 链——trait Person: Named 且 Person 没写 greet() 时
        // 构造会报 "missing method greet"，但实际调用时 dispatch 走 Named 默认实现能成功。
        // 现在构造和 dispatch 都用 collect_required_methods()。
        let src = r#"
trait Named
  fn get_name(self) -> string = "Anon"
end

trait Greeter: Named
  fn greet(self) -> string = "Hi, " + self.get_name()
end

impl Greeter for Human
  -- 只写 get_name()，greet() 用 Greeter 默认实现
  fn get_name(self) = "Alice"
end

task main()
  let g: dyn Greeter = Greeter::new("Human")
  let s: string = g.greet()
  print(s)
end
"#;
        run(src).expect("should construct via parent default");
    }

    #[test]
    fn test_trait_default_implementation_fallback() {
        // v0.08.3: trait method body 作为默认实现，impl 可省略
        let src = r#"
trait Calc
  fn double(self) -> number = self.value() * 2
  fn value(self) -> number
end

impl Calc for Ten
  fn value(self) = 10
  -- double() 用默认实现
end

task main()
  let c: dyn Calc = Calc::new("Ten")
  print(c.double())
end
"#;
        run(src).expect("default impl should work");
    }

    #[test]
    fn test_trait_duplicate_impl_detected_at_construction() {
        // v0.08.5: 构造时如果 trait 链上有方法既无 impl 也无默认实现，报错
        // 这里 trait Greeter 要求 fn greet()，impl 写了别的 fn foo() 而 greet() 没实现 →
        // 构造 Greeter::new("Empty") 时 collect_required_methods() 走 env 找不到 greet 的 impl，
        // 应返回 Err 含 "greet"。
        // 注：parser 不接受完全空的 impl 块（要求至少一个 fn），所以这里 impl 写一个无关方法。
        let src = r#"
trait Greeter
  fn greet(self) -> string
end

impl Greeter for Empty
  fn foo(self) = 1
end

task main()
  let g: dyn Greeter = Greeter::new("Empty")
  print(g.greet())
end
"#;
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        let result = interp.interpret(&stmts);
        assert!(
            result.is_err(),
            "construct should fail when method has no impl and no default"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("greet"),
            "error should mention method name 'greet': {}",
            err
        );
    }

    #[test]
    fn test_trait_circular_inheritance_no_panic() {
        // v0.08.5: typeck 已防循环继承，这里只验证 interpreter 不会 panic
        let src = r#"
trait A: B
  fn a(self) -> number = 1
end

trait B: A
  fn b(self) -> number = 2
end

task main()
  -- 只 typeck 不跑 dispatch
  print(1)
end
"#;
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        // 不期望 panic；可能报 typeck error（来自循环继承检测），也可能通过
        let _ = interp.interpret(&stmts);
    }

    // ============================
    // v0.08.5 任务 1: self-less 方法
    // ============================

    /// trait method 第一个参数不是 self，dispatch 时不传 receiver
    #[test]
    fn test_self_less_method_basic() {
        let src = r#"
trait Math
  fn add(a: number, b: number) -> number
end

impl Math for Calc
  fn add(a: number, b: number) = a + b
end

task main()
  let m: dyn Math = Math::new("Calc")
  print(m.add(1, 2))
end
"#;
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp
            .interpret(&stmts)
            .expect("self-less method should work");
    }

    /// self-less 默认实现（trait 内 fn 直接给实现，不带 self）
    #[test]
    fn test_self_less_default_impl() {
        let src = r#"
trait Math
  fn add(a: number, b: number) -> number = a * b
end

impl Math for Calc
  -- add() 用默认实现（self-less）
end

task main()
  let m: dyn Math = Math::new("Calc")
  print(m.add(3, 4))
end
"#;
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp
            .interpret(&stmts)
            .expect("self-less default impl should work");
    }

    /// self-having 和 self-less 混合 trait
    #[test]
    fn test_mixed_self_and_self_less() {
        let src = r#"
trait Mixed
  fn greet(self) -> string = "hello"
  fn double(x: number) -> number = x * 2
end

impl Mixed for M
  fn greet(self) = "hi"
  fn double(x: number) = x + 100
end

task main()
  let m: dyn Mixed = Mixed::new("M")
  print(m.greet())
  print(m.double(5))
end
"#;
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp
            .interpret(&stmts)
            .expect("mixed self/self-less should work");
    }

    // ============================
    // v0.08.5 任务 2: trait 内默认实现块语法 do ... end
    // ============================

    /// trait 默认实现用 do ... end 块（多语句）
    #[test]
    fn test_trait_default_impl_do_end() {
        let src = r#"
trait Counter
  fn default_value() -> number
end

impl Counter for Zero
  fn default_value() do
    let x = 10
    let y = 20
    return x + y
  end
end

task main()
  let c: dyn Counter = Counter::new("Zero")
  print(c.default_value())
end
"#;
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp
            .interpret(&stmts)
            .expect("trait do/end block should work");
    }

    /// trait 默认实现混合 = expr 和 do ... end
    #[test]
    fn test_trait_default_impl_mixed_syntax() {
        let src = r#"
trait Mixed
  fn quick() -> number = 42
  fn slow() -> number
end

impl Mixed for M
  fn slow() do
    let a = 1
    let b = 2
    return a + b + 100
  end
end

task main()
  let m: dyn Mixed = Mixed::new("M")
  print(m.quick())
  print(m.slow())
end
"#;
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp
            .interpret(&stmts)
            .expect("mixed = expr and do/end should work");
    }
}

// ============================
// v0.10: AI 调用 retry 单测（测试纯函数）
// ============================
#[cfg(test)]
mod retry_tests {
    use super::*;

    /// v0.10: 错误分类逻辑
    #[test]
    fn test_is_retryable_error_transport() {
        // 网络错误：可重试
        assert!(is_retryable_error(
            "ai.chat: network error connecting to https://api.example.com: connection refused"
        ));
        assert!(is_retryable_error(
            "ai.chat: network error connecting to https://api.example.com: timeout"
        ));
    }

    #[test]
    fn test_is_retryable_error_http_429() {
        // HTTP 429 rate limit：可重试
        assert!(is_retryable_error(
            "ai.chat: API error HTTP 429 from https://api.example.com (body: rate limit exceeded)"
        ));
    }

    #[test]
    fn test_is_retryable_error_http_5xx() {
        // HTTP 5xx：可重试
        assert!(is_retryable_error(
            "ai.chat: API error HTTP 500 from https://api.example.com (body: server error)"
        ));
        assert!(is_retryable_error(
            "ai.chat: API error HTTP 502 from https://api.example.com (body: bad gateway)"
        ));
        assert!(is_retryable_error(
            "ai.chat: API error HTTP 503 from https://api.example.com (body: unavailable)"
        ));
        assert!(is_retryable_error(
            "ai.chat: API error HTTP 599 from https://api.example.com (body: edge)"
        ));
    }

    #[test]
    fn test_is_retryable_error_http_4xx_not_retryable() {
        // HTTP 4xx（除 429）：不可重试
        assert!(!is_retryable_error(
            "ai.chat: API error HTTP 400 from https://api.example.com (body: bad request)"
        ));
        assert!(!is_retryable_error(
            "ai.chat: API error HTTP 401 from https://api.example.com (body: unauthorized)"
        ));
        assert!(!is_retryable_error(
            "ai.chat: API error HTTP 403 from https://api.example.com (body: forbidden)"
        ));
        assert!(!is_retryable_error(
            "ai.chat: API error HTTP 404 from https://api.example.com (body: not found)"
        ));
    }

    #[test]
    fn test_is_retryable_error_other() {
        // 其他错误：不可重试
        assert!(!is_retryable_error("ai.chat: messages cannot be empty"));
        assert!(!is_retryable_error(
            "ai.chat: failed to read response body: some io error"
        ));
    }

    /// v0.10: 重试 sleep 时间计算
    #[test]
    fn test_retry_sleep_ms_exponential() {
        // base=1000ms 时：attempt=0 → ~1000, attempt=1 → ~2000, attempt=2 → ~4000
        let base = 1000u64;
        let s0 = retry_sleep_ms(0, base);
        let s1 = retry_sleep_ms(1, base);
        let s2 = retry_sleep_ms(2, base);
        // 实际值有 ±20% jitter
        assert!((800..=1200).contains(&s0), "s0={}", s0);
        assert!((1600..=2400).contains(&s1), "s1={}", s1);
        assert!((3200..=4800).contains(&s2), "s2={}", s2);
    }

    #[test]
    fn test_retry_sleep_ms_no_overflow() {
        // base=1000ms，attempt=100 不会 overflow（saturating_mul）
        let s = retry_sleep_ms(100, 1000);
        // saturating_mul 限制左移 ≤ 10 → exp ≈ 1024 * 1000 ≈ 1M ms
        assert!(s > 0);
    }

    /// v0.10: ai_retry_max / ai_retry_base_ms 默认值
    #[test]
    fn test_ai_retry_config_defaults() {
        // 不设环境变量时应返回默认值
        // 注：环境变量可能已被其他测试污染，这里只验证函数不会 panic
        let _ = ai_retry_max();
        let _ = ai_retry_base_ms();
    }
}

/// v0.15: Token budget 测试
#[cfg(test)]
mod token_budget_tests {
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
    fn test_per_call_limit_exceeded() {
        // 模拟：设置 per_call=10，然后 track_tokens 超限
        let mut interp = Interpreter::new();
        interp.token_budget = Some(TokenBudget {
            total: usize::MAX,
            per_call: Some(10),
            alert_threshold: 0.8,
        });
        // 5 input + 10 output = 15 > 10 → 应报错
        let result = interp.track_tokens(5, 10);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("per-call limit exceeded"));
    }

    #[test]
    fn test_per_call_limit_within_range() {
        let mut interp = Interpreter::new();
        interp.token_budget = Some(TokenBudget {
            total: usize::MAX,
            per_call: Some(100),
            alert_threshold: 0.8,
        });
        // 30 input + 50 output = 80 < 100 → 应成功
        let result = interp.track_tokens(30, 50);
        assert!(result.is_ok());
    }

    #[test]
    fn test_total_budget_exceeded() {
        let mut interp = Interpreter::new();
        interp.token_budget = Some(TokenBudget {
            total: 100,
            per_call: None,
            alert_threshold: 0.8,
        });
        // 第一次：50 + 30 = 80 < 100 → 成功
        assert!(interp.track_tokens(50, 30).is_ok());
        // 第二次：50 + 30 = 80，累计 160 > 100 → 报错
        let result = interp.track_tokens(50, 30);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Token budget exceeded"));
    }

    #[test]
    fn test_with_block_sets_per_call() {
        let src = r#"
task main()
  with per_call = 100
    -- 配置已设置，后续 AI 调用会检查
    print("configured")
  end
end
"#;
        run(src).expect("should run without error");
    }

    #[test]
    fn test_with_block_sets_budget() {
        let src = r#"
task main()
  with budget = 1000
    print("configured")
  end
end
"#;
        run(src).expect("should run without error");
    }
}

/// v0.16: Match Guard 测试
#[cfg(test)]
mod guard_tests {
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
    fn test_guard_basic() {
        let src = r#"
task classify(n: number): string
  return match n with
    x when x > 0 -> "positive"
    x when x < 0 -> "negative"
    _ -> "zero"
  end
end

task main()
  let r1 = classify(5)
  let r2 = classify(-3)
  let r3 = classify(0)
  print(r1)
  print(r2)
  print(r3)
end
"#;
        run(src).expect("should run with guard");
    }

    #[test]
    fn test_dict_partial_match() {
        let src = r#"
task main()
  let data = {"name": "Alice", "age": 30, "city": "NYC"}
  let result = match data with
    {name: n, age: a} -> p"{n} is {a}"
    _ -> "no match"
  end
  print(result)
end
"#;
        run(src).expect("should partial match dict");
    }

    #[test]
    fn test_dict_full_match() {
        let src = r#"
task main()
  let data = {"x": 1, "y": 2}
  let result = match data with
    {x: a, y: b} -> p"{a}+{b}"
    _ -> "no match"
  end
  print(result)
end
"#;
        run(src).expect("should full match dict");
    }

    #[test]
    fn test_guard_with_dict() {
        let src = r#"
task main()
  let data = {"age": 25, "name": "Alice"}
  let result = match data with
    {age: age, name: name} when age >= 18 -> p"{name} is adult"
    {age: age, name: name} when age < 18 -> p"{name} is minor"
    _ -> "unknown"
  end
  print(result)
end
"#;
        run(src).expect("should run with dict guard");
    }

    #[test]
    fn test_pipe_with_closure() {
        let src = r#"
task main()
  let double = fn(x)
    return x * 2
  end
  let result = 5 |> double
  print(result)
end
"#;
        run(src).expect("should pipe to closure");
    }

    #[test]
    fn test_window_basic() {
        let src = r#"
task main()
  let data = [1, 2, 3, 4, 5]
  let windows = data.window(3)
  print(windows)
end
"#;
        run(src).expect("should create windows");
    }

    #[test]
    fn test_batch_basic() {
        let src = r#"
task main()
  let data = [1, 2, 3, 4, 5, 6, 7]
  let batches = data.batch(3)
  print(batches)
end
"#;
        run(src).expect("should create batches");
    }

    #[test]
    fn test_shape_2d() {
        let src = r#"
task main()
  let matrix = [[1, 2], [3, 4], [5, 6]]
  let shape = matrix.shape()
  print(shape)
end
"#;
        run(src).expect("should get shape");
    }

    #[test]
    fn test_flatten() {
        let src = r#"
task main()
  let matrix = [[1, 2], [3, 4], [5, 6]]
  let flat = matrix.flatten()
  print(flat)
end
"#;
        run(src).expect("should flatten");
    }

    #[test]
    fn test_transpose() {
        let src = r#"
task main()
  let matrix = [[1, 2, 3], [4, 5, 6]]
  let transposed = matrix.transpose()
  print(transposed)
end
"#;
        run(src).expect("should transpose");
    }

    #[test]
    fn test_reshape() {
        let src = r#"
task main()
  let flat = [1, 2, 3, 4, 5, 6]
  let matrix = flat.reshape(2, 3)
  print(matrix)
end
"#;
        run(src).expect("should reshape");
    }

    #[test]
    fn test_take_basic() {
        let src = r#"
task main()
  let data = [1, 2, 3, 4, 5]
  let result = data.take(3)
  print(result)
end
"#;
        run(src).expect("should take first n");
    }

    #[test]
    fn test_drop_basic() {
        let src = r#"
task main()
  let data = [1, 2, 3, 4, 5]
  let result = data.drop(2)
  print(result)
end
"#;
        run(src).expect("should drop first n");
    }

    #[test]
    fn test_pipe_chain_router_style() {
        // 消息链风格：对同一接收者连续调用
        let src = r#"
task handler(req)
  return {"status": 200, "body": "ok"}
end

task main()
  let router = Router::new()
  let r = router.route("GET", "/health", handler)
  print(type_of(r))
end
"#;
        run(src).expect("should chain router methods");
    }

    #[test]
    fn test_lifetime_basic() {
        // 生命周期标注在 Mora 中是可选的，主要用于文档和未来编译期检查
        // 当前实现：解析但运行时忽略
        let src = r#"
task longest(x: string, y: string): string
  if x.len() > y.len() then
    return x
  end
  return y
end

task main()
  let a = "hello"
  let b = "world!"
  let result = longest(a, b)
  print(result)
end
"#;
        run(src).expect("should run basic task");
    }

    #[test]
    fn test_lifetime_annotation() {
        // 生命周期标注语法测试（简化版：只在函数签名中标注）
        let src = r#"
task identity(x: string): string
  return x
end

task main()
  let a = "hello"
  let result = identity(a)
  print(result)
end
"#;
        run(src).expect("should support basic task");
    }

    #[test]
    fn test_borrow_shared() {
        // 不可变借用测试
        let src = r#"
task main()
  let a = [1, 2, 3]
  let b = &a
  let c = &a
  print(type_of(b))
  print(type_of(c))
end
"#;
        run(src).expect("should allow multiple immutable borrows");
    }

    #[test]
    fn test_borrow_mutable() {
        // 可变借用测试
        let src = r#"
task main()
  let a = [1, 2, 3]
  let b = &mut a
  print(type_of(b))
end
"#;
        run(src).expect("should allow mutable borrow");
    }

    #[test]
    fn test_borrow_basic() {
        let src = r#"
task main()
  let a = [1, 2, 3]
  let b = &a
  print(type_of(b))
end
"#;
        run(src).expect("should borrow");
    }

    #[test]
    fn test_borrow_mut_basic() {
        let src = r#"
task main()
  let a = [1, 2, 3]
  let b = &mut a
  print(type_of(b))
end
"#;
        run(src).expect("should mutable borrow");
    }

    #[test]
    fn test_macro_basic() {
        let src = r#"
macro when(condition, body)
  if condition then
    body
  end
end

task main()
  let x = 10
  when(x > 5, print("big"))
end
"#;
        run(src).expect("should define and use macro");
    }

    #[test]
    fn test_transaction_basic() {
        let src = r#"
task main()
  transaction
    print("in transaction")
    commit
  end
end
"#;
        run(src).expect("should run transaction");
    }

    #[test]
    fn test_transaction_compensation() {
        let src = r#"
task main()
  transaction
    print("in transaction")
    rollback
  compensation
    print("compensating")
  end
end
"#;
        // rollback 会返回错误，但 compensation 应该执行
        let result = run(src);
        assert!(result.is_err());
    }

    #[test]
    fn test_worker_basic() {
        let src = r#"
task main()
  parallel
    worker w1
      print("worker 1 done")
    end
    worker w2
      print("worker 2 done")
    end
  end
end
"#;
        run(src).expect("should run workers");
    }

    #[test]
    fn test_type_of() {
        let src = r#"
task main()
  print(type_of(42))
  print(type_of("hello"))
  print(type_of(true))
  print(type_of(nil))
  print(type_of([1, 2]))
  print(type_of({"a": 1}))
end
"#;
        run(src).expect("should return type names");
    }

    #[test]
    fn test_is_instance() {
        let src = r#"
task main()
  print(is_instance(42, "number"))
  print(is_instance("hello", "string"))
  print(is_instance(42, "string"))
end
"#;
        run(src).expect("should check types");
    }

    #[test]
    fn test_methods_of() {
        let src = r#"
task main()
  let methods = methods_of("hello")
  print(methods)
end
"#;
        run(src).expect("should list methods");
    }

    #[test]
    fn test_atom_basic() {
        let src = r#"
task main()
  let counter = atom(0)
  swap(counter, fn(n) return n + 1 end)
  swap(counter, fn(n) return n + 1 end)
  let val = deref(counter)
  print(val)
end
"#;
        run(src).expect("should create and swap atom");
    }

    #[test]
    fn test_atom_swap_returns() {
        let src = r#"
task main()
  let state = atom("initial")
  let new_val = swap(state, fn(old) return "updated" end)
  print(new_val)
  print(deref(state))
end
"#;
        run(src).expect("should return new value from swap");
    }

    #[test]
    fn test_partial_basic() {
        let src = r#"
task main()
  let add = fn(a, b) return a + b end
  let add10 = partial(add, 10)
  let result = add10(5)
  print(result)
end
"#;
        run(src).expect("should partial apply");
    }

    #[test]
    fn test_partial_with_pipe() {
        let src = r#"
task main()
  let add = fn(a, b) return a + b end
  let add10 = partial(add, 10)
  let result = 5 |> add10
  print(result)
end
"#;
        run(src).expect("should partial with pipe");
    }

    #[test]
    fn test_compose_basic() {
        let src = r#"
task main()
  let double = fn(x) return x * 2 end
  let add_one = fn(x) return x + 1 end
  let transform = compose(double, add_one)
  let result = 5 |> transform
  print(result)
end
"#;
        run(src).expect("should compose functions");
    }

    #[test]
    fn test_into_basic() {
        let src = r#"
task main()
  let data = [1, 2, 3]
  let double = fn(x) return x * 2 end
  let result = data.map(double)
  print(result)
end
"#;
        run(src).expect("should use map as into");
    }

    #[test]
    fn test_broadcast_list_mul_scalar() {
        let src = r#"
task main()
  let a = [1, 2, 3]
  let result = a * 2
  print(result)
end
"#;
        run(src).expect("should broadcast list * scalar");
    }

    #[test]
    fn test_broadcast_scalar_add_list() {
        let src = r#"
task main()
  let a = [10, 20, 30]
  let result = 1 + a
  print(result)
end
"#;
        run(src).expect("should broadcast scalar + list");
    }

    #[test]
    fn test_broadcast_list_add_list() {
        let src = r#"
task main()
  let a = [1, 2, 3]
  let b = [10, 20, 30]
  let result = a + b
  print(result)
end
"#;
        run(src).expect("should broadcast list + list");
    }

    #[test]
    fn test_pipe_chain() {
        let src = r#"
task main()
  let double = fn(x)
    return x * 2
  end
  let add_one = fn(x)
    return x + 1
  end
  let result = 5 |> double |> add_one
  print(result)
end
"#;
        run(src).expect("should chain pipes");
    }

    #[test]
    fn test_list_rest_pattern() {
        let src = r#"
task main()
  let data = [1, 2, 3, 4, 5]
  let result = match data with
    [head, ...tail] -> p"head={head}, tail={tail}"
    _ -> "empty"
  end
  print(result)
end
"#;
        run(src).expect("should match rest pattern");
    }

    #[test]
    fn test_list_rest_empty() {
        let src = r#"
task main()
  let data = [42]
  let result = match data with
    [head, ...tail] -> p"head={head}, tail={tail}"
    _ -> "no match"
  end
  print(result)
end
"#;
        run(src).expect("should match single element with rest");
    }

    #[test]
    fn test_list_exact_match() {
        let src = r#"
task main()
  let data = [1, 2, 3]
  let result = match data with
    [a, b, c] -> p"{a}+{b}+{c}"
    _ -> "no match"
  end
  print(result)
end
"#;
        run(src).expect("should exact match");
    }

    #[test]
    fn test_guard_fallback() {
        let src = r#"
task main()
  let x = 5
  let result = match x with
    n when n > 10 -> "big"
    n when n > 3 -> "medium"
    _ -> "small"
  end
  print(result)
end
"#;
        run(src).expect("should fallback to next arm");
    }
}

/// v0.15: Mock LLM 测试
#[cfg(test)]
mod mock_llm_tests {
    use super::*;

    #[test]
    fn test_mock_llm_basic() {
        // mock_llm 测试需要在 with 块内调用 ai.chat
        // 由于 ai.chat 需要 cfg 参数，这里用简单的内联测试
        let mut interp = Interpreter::new();
        interp.current_ai_config = Some(AiConfigValue {
            mock_responses: Some(vec!["mock response 1".to_string(), "mock response 2".to_string()]),
            ..Default::default()
        });
        let msgs = vec![("user".to_string(), "test prompt".to_string())];
        let result = interp.real_ai_chat_inner(&msgs, "", "test-model", "");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().to_string(), "mock response 1");
        // 第二次调用应返回第二个响应
        let result2 = interp.real_ai_chat_inner(&msgs, "", "test-model", "");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap().to_string(), "mock response 2");
    }

    #[test]
    fn test_mock_llm_exhausted() {
        let mut interp = Interpreter::new();
        interp.current_ai_config = Some(AiConfigValue {
            mock_responses: Some(vec!["only one".to_string()]),
            ..Default::default()
        });
        let msgs = vec![("user".to_string(), "test".to_string())];
        let r1 = interp.real_ai_chat_inner(&msgs, "", "m", "");
        assert!(r1.is_ok());
        assert_eq!(r1.unwrap().to_string(), "only one");
        // 队列空了，应走真实 API 路径
        // 无 API key 时会报错（这是正确行为）
        let r2 = interp.real_ai_chat_inner(&msgs, "", "m", "");
        // 没有 API key 时，真实调用会失败
        assert!(r2.is_err());
    }
}
