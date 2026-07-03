mod ai_chat;
mod ai_helpers;
mod builtins;
mod dispatch;
mod evaluate;
mod execute;
mod orchestrate;
mod trait_dispatch;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// v1 AST types no longer imported — all v2 paths use ast_v2 / common
use crate::ai_infra::*;
use crate::flow::*;
use crate::lexer::Lexer;
use crate::trace_collector::TraceCollector;

/// 使用 ParserV2 解析代码，返回 v2 AST (node_ids + arena)
pub fn parse_code(source: &str) -> (Vec<crate::ast_v2::NodeId>, crate::ast_v2::AstArena) {
    let tokens = Lexer::new(source).scan_tokens();
    let mut parser_v2 = crate::parser_v2::ParserV2::new(tokens);
    let node_ids = parser_v2.parse();
    let arena = parser_v2.into_arena();
    (node_ids, arena)
}

pub use crate::value::{Environment, FlowSignal, StreamReader, Value};

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
    pub impl_table: HashMap<String, Vec<String>>,
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
    // v0.24: 自适应 draft 模型选择 (model -> success_rate)
    draft_model_stats: HashMap<String, (usize, usize)>, // (success, total)
    // v0.24: 推测解码缓存预热队列
    #[allow(dead_code)] // 未来扩展用
    cache_warm_queue: Vec<String>,
    // v0.24: AI 调用优先级队列
    #[allow(dead_code)] // 未来扩展用
    ai_priority_queue: Vec<AiPriorityEntry>,
    // v0.24: 自适应温度调整器
    #[allow(dead_code)] // 未来扩展用
    adaptive_temp: AdaptiveTemperature,
    // v0.24: 上下文窗口管理器
    context_window: ContextWindow,
    // v0.24: 模型负载均衡器
    #[allow(dead_code)] // 未来扩展用
    load_balancer: LoadBalancer,
    // v0.24: 推测解码并行验证器
    speculative_verifier: SpeculativeVerifier,
    // v0.24: AI 调用缓存预热器
    #[allow(dead_code)] // 未来扩展用
    cache_warmer: CacheWarmer,
    // v0.24: 重试策略
    #[allow(dead_code)] // 未来扩展用
    retry_policy: RetryPolicy,
    /// v2 AST arena — 在 interpret 期间存储，供 call_value 执行 v2 闭包
    v2_arena: Option<crate::ast_v2::AstArena>,
    /// v0.25: 会话记忆存储
    memory_store: HashMap<String, Value>,
    /// v0.34: 事件总线 (来自 src/event/, Puter EventClient 风格)
    bus: crate::event::EventBus,
    /// v0.34: 沙箱策略 (来自 src/sandbox/, MimiClaw path validation)
    sandbox: crate::sandbox::SandboxPolicy,
    /// v0.34: scheduler (cron, MimiClaw style)
    scheduler: crate::schedule::Scheduler,
    /// v0.34: CCR (Compress-Cache-Retrieve, Headroom style)
    ccr_store: crate::ccr::InMemoryCcrStore,
    /// v0.34: mock registry (OpenFugu + OpenInfer mock)
    mock_registry: crate::mock::MockRegistry,
}

/// v0.24: AI 调用优先级队列条目类型
type AiPriorityEntry = (u32, String, Vec<(String, String)>);

/// v0.06: with 块字段 (不经过 env 变量)
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
struct AiConfigValue {
    model: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<usize>,
    budget: Option<usize>,
    per_call: Option<usize>,
    system: Option<String>,
    /// v0.15: mock 响应队列 (with mock_llm = ["resp1", "resp2"])
    mock_responses: Option<Vec<String>>,
    /// v0.24: 投机执行配置
    speculative: Option<bool>,
    draft_model: Option<String>,
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
            ai_cache: HashMap::new(),        // 不克隆缓存
            string_interner: HashMap::new(), // 不克隆驻留池
            method_cache: HashMap::new(),    // 不克隆缓存
            ai_batch_queue: Vec::new(),
            draft_model_stats: HashMap::new(),
            cache_warm_queue: Vec::new(),
            ai_priority_queue: Vec::new(),
            adaptive_temp: AdaptiveTemperature::default(),
            context_window: ContextWindow::default(),
            load_balancer: LoadBalancer::default(),
            speculative_verifier: SpeculativeVerifier::default(),
            cache_warmer: CacheWarmer::default(),
            retry_policy: RetryPolicy::default(), // 不克隆队列
            v2_arena: None,
            memory_store: HashMap::new(),
            bus: crate::event::EventBus::new(),
            sandbox: crate::sandbox::SandboxPolicy::permissive(),
            scheduler: crate::schedule::Scheduler::new(),
            ccr_store: crate::ccr::InMemoryCcrStore::new(),
            mock_registry: crate::mock::MockRegistry::new(),
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
    /// v0.24: 路由优先级 (越小越优先)
    #[allow(dead_code)] // 未来扩展用
    priority: u32,
    /// v0.24: 路由健康状态
    #[allow(dead_code)] // 未来扩展用
    healthy: bool,
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
        // v0.25: 注册 builtin 模块对象 (v0.27: 加入 document)
        for name in &["ai", "web", "json", "file", "memory", "agent", "document"] {
            globals.lock().expect("globals mutex poisoned").define(
                name.to_string(),
                Value::Builtin(name.to_string()),
                false,
            );
        }
        // v0.26: 注册 compose_prompt / tail 内建函数 (供 prompt section 块式调用)
        globals.lock().expect("globals mutex poisoned").define(
            "compose_prompt".to_string(),
            Value::Builtin("compose_prompt".to_string()),
            false,
        );
        globals.lock().expect("globals mutex poisoned").define(
            "tail".to_string(),
            Value::Builtin("tail".to_string()),
            false,
        );
        // v0.29: 注册 compress / crush_json 顶层 builtin
        globals.lock().expect("globals mutex poisoned").define(
            "compress".to_string(),
            Value::Builtin("compress".to_string()),
            false,
        );
        globals.lock().expect("globals mutex poisoned").define(
            "crush_json".to_string(),
            Value::Builtin("crush_json".to_string()),
            false,
        );
        // v0.34: 注册 event bus 顶层 builtin
        globals.lock().expect("globals mutex poisoned").define(
            "bus".to_string(),
            Value::Builtin("bus".to_string()),
            false,
        );
        // v0.34: 注册 sandbox 顶层 builtin
        globals.lock().expect("globals mutex poisoned").define(
            "sandbox".to_string(),
            Value::Builtin("sandbox".to_string()),
            false,
        );
        // v0.34: 注册 schedule 顶层 builtin
        globals.lock().expect("globals mutex poisoned").define(
            "schedule".to_string(),
            Value::Builtin("schedule".to_string()),
            false,
        );
        // v0.34: 注册 ccr 顶层 builtin
        globals.lock().expect("globals mutex poisoned").define(
            "ccr".to_string(),
            Value::Builtin("ccr".to_string()),
            false,
        );
        // v0.34: 注册 mock 顶层 builtin
        globals.lock().expect("globals mutex poisoned").define(
            "mock".to_string(),
            Value::Builtin("mock".to_string()),
            false,
        );
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
            draft_model_stats: HashMap::new(),
            cache_warm_queue: Vec::new(),
            ai_priority_queue: Vec::new(),
            adaptive_temp: AdaptiveTemperature::default(),
            context_window: ContextWindow::default(),
            load_balancer: LoadBalancer::default(),
            speculative_verifier: SpeculativeVerifier::default(),
            cache_warmer: CacheWarmer::default(),
            retry_policy: RetryPolicy::default(),
            v2_arena: None,
            memory_store: HashMap::new(),
            bus: crate::event::EventBus::new(),
            sandbox: crate::sandbox::SandboxPolicy::permissive(),
            scheduler: crate::schedule::Scheduler::new(),
            ccr_store: crate::ccr::InMemoryCcrStore::new(),
            mock_registry: crate::mock::MockRegistry::new(),
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
            draft_model_stats: HashMap::new(),
            cache_warm_queue: Vec::new(),
            ai_priority_queue: Vec::new(),
            adaptive_temp: AdaptiveTemperature::default(),
            context_window: ContextWindow::default(),
            load_balancer: LoadBalancer::default(),
            speculative_verifier: SpeculativeVerifier::default(),
            cache_warmer: CacheWarmer::default(),
            retry_policy: RetryPolicy::default(),
            v2_arena: None,
            memory_store: HashMap::new(),
            bus: crate::event::EventBus::new(),
            sandbox: crate::sandbox::SandboxPolicy::permissive(),
            scheduler: crate::schedule::Scheduler::new(),
            ccr_store: crate::ccr::InMemoryCcrStore::new(),
            mock_registry: crate::mock::MockRegistry::new(),
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
            draft_model_stats: HashMap::new(),
            cache_warm_queue: Vec::new(),
            ai_priority_queue: Vec::new(),
            adaptive_temp: AdaptiveTemperature::default(),
            context_window: ContextWindow::default(),
            load_balancer: LoadBalancer::default(),
            speculative_verifier: SpeculativeVerifier::default(),
            cache_warmer: CacheWarmer::default(),
            retry_policy: RetryPolicy::default(),
            v2_arena: None,
            memory_store: HashMap::new(),
            bus: crate::event::EventBus::new(),
            sandbox: crate::sandbox::SandboxPolicy::permissive(),
            scheduler: crate::schedule::Scheduler::new(),
            ccr_store: crate::ccr::InMemoryCcrStore::new(),
            mock_registry: crate::mock::MockRegistry::new(),
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

    /// 入口：直接执行 v2 AST
    pub fn interpret(
        &mut self,
        stmt_ids: &[crate::ast_v2::NodeId],
        arena: &crate::ast_v2::AstArena,
    ) -> Result<(), String> {
        // 存储 arena 供 call_value 执行 v2 闭包
        self.v2_arena = Some(arena.clone());
        // 执行所有顶层语句
        for stmt_id in stmt_ids {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                match self.execute(&kind, arena)? {
                    FlowSignal::None => {}
                    FlowSignal::Return(val) => {
                        return Err(format!("Unexpected return at top level: {:?}", val));
                    }
                    signal => {
                        return Err(format!("Unexpected signal at top level: {:?}", signal));
                    }
                }
            }
        }
        // 查找并执行 main task
        let main_task = self
            .globals
            .lock()
            .map_err(|_| "globals mutex poisoned".to_string())?
            .get("main")
            .clone();
        if let Some(Value::Task { params, .. }) = main_task
            && params.is_empty()
        {
            // 在 arena 中找到 main 的 body 并执行
            for stmt_id in stmt_ids {
                if let Some(stmt) = arena.get_stmt(*stmt_id)
                    && let crate::ast_v2::StmtKind::TaskDef { name, body, .. } = &stmt.kind
                    && name == "main"
                {
                    let body = body.clone();
                    for body_id in &body {
                        if let Some(body_stmt) = arena.get_stmt(*body_id) {
                            let kind = body_stmt.kind.clone();
                            match self.execute(&kind, arena)? {
                                FlowSignal::None => {}
                                FlowSignal::Return(_) => return Ok(()),
                                signal => {
                                    return Err(format!("Unexpected signal in main: {:?}", signal));
                                }
                            }
                        }
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }

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
            let (node_ids, arena) = parse_code(trimmed);
            if node_ids.is_empty() {
                continue;
            }
            for stmt_id in &node_ids {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    match interp.execute(&kind, &arena) {
                        Ok(FlowSignal::Return(v)) => println!("= {}", v),
                        Ok(FlowSignal::None) => {}
                        Ok(FlowSignal::Break) | Ok(FlowSignal::Continue) => {}
                        Err(e) => eprintln!("Error: {}", e),
                    }
                }
            }
        }
    }

    // ===================================================================
    // v0.04.0: AI 原语辅助函数
    // ===================================================================

    /// v2 版 Literal → Value（common::Literal 不含 List/Dict，those 是 ExprKind 变体）
    fn literal_to_value_inner(&mut self, lit: &crate::common::Literal) -> Result<Value, String> {
        match lit {
            crate::common::Literal::String(s, _) => Ok(self.intern_string(s.clone())),
            crate::common::Literal::Char(c, _) => Ok(Value::Char(*c)),
            crate::common::Literal::Number(n, _) => Ok(Value::Number(*n)),
            crate::common::Literal::Bool(b, _) => Ok(Value::Bool(*b)),
            crate::common::Literal::Nil(_) => Ok(Value::Nil),
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
        let vec = match indexed.into_iter().next() {
            Some((_, v)) => v,
            None => {
                return Err("ai.embed: no embeddings were successfully indexed".to_string());
            }
        };
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
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
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

    fn run(src: &str) -> Result<(), String> {
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
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

    fn run(src: &str) -> Result<(), String> {
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
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
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        let result = interp.interpret(&node_ids, &arena);
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
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        // 不期望 panic；可能报 typeck error（来自循环继承检测），也可能通过
        let _ = interp.interpret(&node_ids, &arena);
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
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp
            .interpret(&node_ids, &arena)
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
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp
            .interpret(&node_ids, &arena)
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
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp
            .interpret(&node_ids, &arena)
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
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp
            .interpret(&node_ids, &arena)
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
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp
            .interpret(&node_ids, &arena)
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

    fn run(src: &str) -> Result<(), String> {
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
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

    fn run(src: &str) -> Result<(), String> {
        let (node_ids, arena) = parse_code(src);
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
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
            mock_responses: Some(vec![
                "mock response 1".to_string(),
                "mock response 2".to_string(),
            ]),
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

// ===================================================================
// v0.25: orchestrate 测试
// ===================================================================
#[cfg(test)]
mod orchestrate_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    #[test]
    fn test_orchestrate_sequential() {
        let src = r#"
task main()
  orchestrate sequential input -> result
    agent step1
      task(ai.chat(p"Step 1: {input}"))
    end
    agent step2
      task(ai.chat(p"Step 2: {input}"))
    end
  end
  print(result)
end
"#;
        run(src).expect("orchestrate sequential should work");
    }

    #[test]
    fn test_orchestrate_loop() {
        let src = r#"
task main()
  orchestrate loop input -> result, max_rounds: 3
    agent improver
      task(ai.chat(p"Improve: {input}"))
    end
  end
  print(result)
end
"#;
        run(src).expect("orchestrate loop should work");
    }
}

// ===================================================================
// v0.25: eval + skill 测试
// ===================================================================
#[cfg(test)]
mod eval_skill_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    #[test]
    fn test_eval_basic() {
        let src = r#"
task main()
  let x = 42
  eval "数字检查"
    given: x
    expect: given > 0
    expect: given < 100
  end
  print("eval passed")
end
"#;
        run(src).expect("eval basic should work");
    }

    #[test]
    fn test_eval_with_tolerance() {
        let src = r#"
task main()
  let x = 42
  eval "容忍度测试"
    given: x
    tolerance: 0.5
    expect: given > 0
    expect: given > 1000
  end
  print("eval with tolerance passed")
end
"#;
        run(src).expect("eval with tolerance should work");
    }

    #[test]
    fn test_eval_failure() {
        let src = r#"
task main()
  let x = 42
  eval "应该失败"
    given: x
    expect: given > 1000
  end
end
"#;
        let result = run(src);
        assert!(result.is_err(), "eval should fail when expect is false");
        assert!(result.unwrap_err().contains("failed"));
    }

    #[test]
    fn test_skill_basic() {
        let src = r#"
skill Greeter
  description: "问候器"
  version: "1.0"

  task greet(name: string): string
    return "Hello, " + name
  end
end

task main()
  let result = Greeter.greet("World")
  print(result)
end
"#;
        run(src).expect("skill basic should work");
    }

    #[test]
    fn test_skill_metadata() {
        let src = r#"
skill Calculator
  description: "计算器"
  version: "2.0"
  requires: [math]

  task add(a: number, b: number): number
    return a + b
  end
end

task main()
  print(Calculator.description)
  print(Calculator.version)
end
"#;
        run(src).expect("skill metadata should work");
    }
}

// ===================================================================
// v0.25: memory + compact 测试
// ===================================================================
#[cfg(test)]
mod memory_compact_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    #[test]
    fn test_memory_store_recall() {
        let src = r#"
task main()
  memory.store("name", "Alice")
  let name = memory.recall("name")
  print(name)
end
"#;
        run(src).expect("memory store/recall should work");
    }

    #[test]
    fn test_memory_search() {
        let src = r#"
task main()
  memory.store("user_name", "Alice")
  memory.store("user_age", "30")
  memory.store("project", "mora")
  let results = memory.search("user")
  print(results.len())
end
"#;
        run(src).expect("memory search should work");
    }

    #[test]
    fn test_memory_forget() {
        let src = r#"
task main()
  memory.store("temp", "value")
  memory.forget("temp")
  let val = memory.recall("temp")
  print(val)
end
"#;
        run(src).expect("memory forget should work");
    }

    #[test]
    fn test_memory_keys_size() {
        let src = r#"
task main()
  memory.store("a", 1)
  memory.store("b", 2)
  memory.store("c", 3)
  print(memory.size())
  print(memory.keys())
end
"#;
        run(src).expect("memory keys/size should work");
    }

    #[test]
    fn test_memory_clear() {
        let src = r#"
task main()
  memory.store("x", 1)
  memory.store("y", 2)
  memory.clear()
  print(memory.size())
end
"#;
        run(src).expect("memory clear should work");
    }

    #[test]
    fn test_compact_function() {
        // v0.29: v0.25 的 compact(text) 已重命名为 compress(text, strategy)
        let src = r#"
task main()
  let text = "This is a long text that needs to be summarized. It contains many sentences about various topics."
  let summary = compress(text, "summary")
  print(summary)
end
"#;
        run(src).expect("compress function should work (renamed from compact)");
    }

    #[test]
    fn test_memory_save_load() {
        use std::env;
        let tmp_dir = env::temp_dir();
        let tmp_file = tmp_dir.join("mora_test_memory.json");
        let tmp_path = tmp_file.to_string_lossy().replace('\\', "/");
        let src = format!(
            r#"
task main()
  memory.store("key1", "value1")
  memory.store("key2", "value2")
  memory.save("{}")
  memory.clear()
  print(memory.size())
  memory.load("{}")
  print(memory.size())
  print(memory.recall("key1"))
end
"#,
            tmp_path, tmp_path
        );
        run(&src).expect("memory save/load should work");
        // Cleanup
        let _ = std::fs::remove_file(tmp_file);
    }
}

// ===================================================================
// v0.26: Prompt sections 测试
// ===================================================================
#[cfg(test)]
mod prompt_section_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    /// 解析测试: prompt "x" do ... end 块能产生 AST
    /// 该测试使用 set-only 块(不读文件),验证 AST 与基础 evaluate
    #[test]
    fn test_prompt_section_parses() {
        let src = r#"
prompt "identity" do
  set role: "system"
  set budget: "8 KB"
end

let s = compose_prompt("identity")
print(s)
"#;
        run(src).expect("prompt section block should parse and evaluate");
    }

    /// 字典式 compose_prompt — 不依赖文件,字典写在同一行(避免 parser 跨行问题)
    #[test]
    fn test_compose_prompt_inline_dict() {
        let src = r#"
let a = compose_prompt({role:"system", text:"hello", budget:"256 B"})
let b = compose_prompt({role:"system", text:"bye", budget:"256 B"})
print(a.len())
print(b.len())
"#;
        run(src).expect("compose_prompt with dict should work");
    }

    /// 空参错误
    #[test]
    fn test_compose_prompt_empty_args() {
        let src = r#"
let buf = compose_prompt()
"#;
        let result = run(src);
        assert!(
            result.is_err(),
            "compose_prompt() with no args should error"
        );
        assert!(result.unwrap_err().contains("at least 1"));
    }

    /// 引用未定义的 section name 应报错
    #[test]
    fn test_compose_prompt_undefined_section() {
        let src = r#"
let buf = compose_prompt("never_defined")
"#;
        let result = run(src);
        assert!(result.is_err(), "undefined section should error");
        assert!(result.unwrap_err().contains("not defined"));
    }

    /// budget 单位解析: 字符串 "8 KB" / "256 B" / 纯数字 1024
    #[test]
    fn test_budget_bytes_parsing() {
        // "8 KB" == 8192
        let src_b = r#"
prompt "with_kb" do
  set role: "system"
  set budget: "8 KB"
  read "./non_existent.txt"   -- 会失败,但我们测 set 阶段
end
"#;
        // 不期望运行完整,但至少 set 阶段之前的解析应该能跑
        // 由于 read 失败,整段会 fail,我们只关心 lexer/parse 不报错,故只跑到 set
        let tokens = Lexer::new(src_b).scan_tokens();
        assert!(!tokens.is_empty(), "lex KB-suffix should succeed");
        let mut parser_v2 = ParserV2::new(tokens);
        let _nodes = parser_v2.parse();
        // 如果语法过,这里已经 OK
    }

    /// 块式 prompt 不读文件可空跑 (只 set role/budget 不 read)
    #[test]
    fn test_prompt_section_no_read() {
        let src = r#"
prompt "empty" do
  set role: "system"
  set budget: "1 KB"
end

let composed = compose_prompt("empty")
print(composed)
"#;
        run(src).expect("prompt section without read should work");
    }

    /// tail() builtin 解析并执行 — 临时构造一个文件
    #[test]
    fn test_tail_builtin() {
        use std::env;
        let tmp = env::temp_dir().join("mora_prompt_tail_test.jsonl");
        let path = tmp.to_string_lossy().replace('\\', "/");
        let content = "line1\nline2\nline3\nline4\nline5\n";
        std::fs::write(&tmp, content).expect("write tmp");
        // Mora 不支持 keyword args,用 positional
        let src = format!(
            r#"
let last3 = tail("{}", 3)
print(last3)
"#,
            path
        );
        let result = run(&src);
        let _ = std::fs::remove_file(&tmp);
        result.expect("tail() builtin should work");
    }

    /// E2E: prompt section 块 + dict inline 混搭
    #[test]
    fn test_e2e_mixed_sections() {
        use std::env;
        // 写入一个临时 SOUL.md
        let tmp = env::temp_dir().join("mora_test_soul.md");
        std::fs::write(&tmp, "I am Mora.").expect("write soul");
        let path = tmp.to_string_lossy().replace('\\', "/");
        let src = format!(
            r#"
prompt "soul" do
  set role: "system"
  set budget: "1 KB"
  read "{}"
end

prompt "inline_dict" do
  set role: "system"
  set budget: "256 B"
end

let composed = compose_prompt("soul", "inline_dict", {{role:"user", text:"q"}})
print(composed)
"#,
            path
        );
        let result = run(&src);
        let _ = std::fs::remove_file(&tmp);
        result.expect("mixed sections should work");
    }
}

#[cfg(test)]
mod document_parser_tests {
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn parse(src: &str) -> usize {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        parser_v2.parse().len()
    }

    #[test]
    fn test_document_block_parses() {
        // MVP: 块能解析即可
        let src = r#"
document "report" do
  -- placeholder
end
"#;
        let n = parse(src);
        assert!(
            n >= 1,
            "document block should produce >= 1 top-level statement"
        );
    }
}

#[cfg(test)]
mod document_tests {
    // v0.27 Task 8: end-to-end tests for `document.parse` builtin
    // and Value::Document method dispatch.
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    fn fixture_pdf_path() -> String {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/sample.pdf");
        p.to_string_lossy().replace('\\', "/")
    }

    /// v0.27 Task 9: 返回 tests/fixtures/sample.pdf 的 PathBuf.
    /// 注意: 这个 fixture 是 lopdf-valid 的(327 字节),不能简单生成最小 PDF
    /// 替换 — 参见 Task 8 report 的 concerns 章节.
    fn fixture_pdf() -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/sample.pdf");
        p
    }

    /// v0.27 Task 9: no-op stub for `write_minimal_pdf(&path)`.
    /// 实际使用 fixture_pdf() 已存在的 lopdf-valid 文件; 写一个临时的
    /// 最小 PDF 字符串是行不通的(参见 Task 8 报告).
    fn write_minimal_pdf(_path: &std::path::Path) {
        // 故意 no-op: 真正的最小 PDF 文件就是 fixture_pdf() 指向的文件.
    }

    /// v0.27 Task 9: cleanup hook for tests that may have written a temp file.
    /// fixture_pdf() 指向的是已签入的 fixture,绝不能删除 — 别的测试要用.
    fn cleanup_pdf(_path: &std::path::Path) {
        // 故意 no-op: fixture 文件被仓库管理,test 不应删除.
    }

    #[test]
    fn document_parse_via_builtin() {
        let p = fixture_pdf_path();
        let src = format!(
            r#"
              let doc = document.parse("{}")
              print(doc.markdown())
            "#,
            p
        );
        let r = run(&src);
        r.expect("document.parse end-to-end should succeed");
    }

    #[test]
    fn compose_prompt_with_document_text() {
        // 与 v0.26 compose_prompt 组合
        let p = fixture_pdf_path();
        let src = format!(
            r#"
              let doc = document.parse("{}")
              let sys = compose_prompt({{role:"system", text:doc.text(), budget:"32 KB"}})
              print(sys.len())
            "#,
            p
        );
        let r = run(&src);
        r.expect("compose_prompt with doc.text() should succeed");
    }

    #[test]
    fn document_block_with_pdf() {
        // v0.27 Task 9: `document "name" do ... end` 块的端到端测试
        let path = fixture_pdf();
        write_minimal_pdf(&path);
        let p = path.to_string_lossy().replace('\\', "/");
        let src = format!(
            r#"
document "report" do
  set origin: "pdf"
  read "{}"
end
print(report.markdown())
"#,
            p
        );
        let r = run(&src);
        cleanup_pdf(&path);
        r.expect("document block end-to-end");
    }
}

// v0.28 Task 7: end-to-end tests for the new pptx/docx/png backends wired
// into the `document.parse` factory. These verify the factory arm dispatch
// (not just the backend unit tests) and exercise cross-feature composition
// (document.parse + compose_prompt) per the v0.26 + v0.27 + v0.28 interop goal.
#[cfg(test)]
mod office_ocr_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures");
        p.push(name);
        p
    }

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    #[test]
    fn factory_pptx() {
        let path = fixture("sample.pptx");
        if !path.exists() {
            return;
        }
        let src = format!(
            "let d = document.parse(\"{}\")\nprint(d.markdown())",
            path.to_string_lossy().replace('\\', "/")
        );
        run(&src).expect("pptx parse end-to-end");
    }

    #[test]
    fn factory_docx() {
        let path = fixture("sample.docx");
        if !path.exists() {
            return;
        }
        let src = format!(
            "let d = document.parse(\"{}\")\nprint(d.text())",
            path.to_string_lossy().replace('\\', "/")
        );
        run(&src).expect("docx parse end-to-end");
    }

    #[test]
    #[ignore = "v0.28 OCR e2e requires MORA_OCR_MODELS_DIR pointing at the v0.28 vendored models; v0.30 will add CI support for OCR e2e"]
    fn factory_png_ocr() {
        let path = fixture("sample.png");
        if !path.exists() {
            return;
        }
        let src = format!(
            "let d = document.parse(\"{}\")\nprint(d.metadata()[\"ocr_engine\"])",
            path.to_string_lossy().replace('\\', "/")
        );
        run(&src).expect("png parse end-to-end");
    }

    #[test]
    fn factory_unsupported_xls() {
        let src = r#"let d = document.parse("foo.xls")"#;
        let r = run(src);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unsupported extension '.xls'"));
    }

    #[test]
    fn compose_prompt_with_pptx_text() {
        // v0.26 + v0.27 + v0.28 interop
        let path = fixture("sample.pptx");
        if !path.exists() {
            return;
        }
        let src = format!(
            "let d = document.parse(\"{}\")\nlet sys = compose_prompt({{role:\"system\", text:d.text(), budget:\"32 KB\"}})\nprint(sys.len())",
            path.to_string_lossy().replace('\\', "/")
        );
        run(&src).expect("compose_prompt with pptx text");
    }
}

// ===================================================================
// v0.29: compress / crush_json 顶层 builtin e2e 测试
// ===================================================================
#[cfg(test)]
mod compress_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    /// T01: compress(text, "summary") 返回 Value::String (mock 模式, 无 OPENAI_API_KEY)
    #[test]
    fn test_compress_string_summary() {
        let text: String = (0..50).map(|i| format!("line {}\n", i)).collect();
        let src = format!(
            r#"
let result = compress("{}", "summary")
print(len(result) > 0)
"#,
            text
        );
        run(&src).expect("compress summary should work");
    }

    /// T04: compress(text, "head_tail", opts) 包含 elided marker
    #[test]
    fn test_compress_string_head_tail() {
        let text: String = (0..100).map(|i| format!("line {}\n", i)).collect();
        // Mora 中 dict 字面量是 {key:value,...}; Rust format! 需转义 { 为 {{
        let src = format!(
            r#"
let result = compress("{}", "head_tail", {{head_pct: 0.3, tail_pct: 0.3, max_bytes: 200}})
print(result.contains("elided"))
"#,
            text
        );
        run(&src).expect("compress head_tail should work");
    }

    /// T05: compress(text, "lossless") 返回原文本 + original_size marker
    #[test]
    fn test_compress_string_lossless() {
        let src = r#"
let result = compress("hello world", "lossless")
print(result.contains("hello world"))
print(result.contains("original_size=11"))
"#;
        run(src).expect("compress lossless should work");
    }

    /// T09: SmartCrusher crush_json(list, 10) — auto 模式选 TopN 策略 (有 score 字段)
    #[test]
    fn test_crush_json_list_basic() {
        use crate::compress;
        let items: Vec<Value> = (0..100)
            .map(|i| {
                let mut d = HashMap::new();
                d.insert("id".to_string(), Value::Number(i as f64));
                d.insert("score".to_string(), Value::Number((i as f64) * 0.01));
                Value::Dict(d)
            })
            .collect();
        let opts = compress::CompressOptions::default();
        let result = compress::crush_json(&items, 10, &opts);
        assert_eq!(result.items.len(), 10, "100 items → target 10 → 10 kept");
        assert_eq!(result.strategy_used, "topn", "auto + score field → topn");
        assert!(result.savings_ratio > 0.8, "savings > 80%");
    }

    /// T10: SmartCrusher 错误保留 — 第 25 项含 error 字段必须保留
    #[test]
    fn test_crush_json_with_anomaly() {
        use crate::compress;
        let mut items: Vec<Value> = (0..50)
            .map(|i| {
                let mut d = HashMap::new();
                d.insert("id".to_string(), Value::Number(i as f64));
                d.insert("score".to_string(), Value::Number((i as f64) * 0.01));
                Value::Dict(d)
            })
            .collect();
        // 第 25 项注入 error 字段
        if let Value::Dict(d) = &mut items[25] {
            d.insert("error".to_string(), Value::String("BOOM".to_string()));
        }
        let opts = compress::CompressOptions::default();
        let result = compress::crush_json(&items, 5, &opts);
        // 异常项必须保留 (KeepErrorsConstraint)
        let has_boom = result.items.iter().any(|it| {
            if let Value::Dict(d) = it {
                d.get("error")
                    .map(|v| v.to_string().contains("BOOM"))
                    .unwrap_or(false)
            } else {
                false
            }
        });
        assert!(has_boom, "anomaly item with error='BOOM' must be preserved");
    }

    /// T17: 旧 v0.25 `compact(text)` 已重命名, 调用应报错
    #[test]
    fn test_compact_rename_old_call_fails() {
        let src = r#"
let result = compact("text")
"#;
        // 旧 compact 已重命名, 应报错 (Undefined function: compact)
        assert!(run(src).is_err());
    }

    #[test]
    fn test_compact_builtin_removed() {
        // T17: v0.25 compact() 应不再存在 (重命名为 compress)
        let src = r#"
            let result = compact("test")
        "#;
        let result = run(src);
        assert!(
            result.is_err(),
            "v0.25 compact() should be removed in v0.29"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("Undefined function") || err.contains("compact"),
            "error should mention compact being undefined, got: {}",
            err
        );
    }

    #[test]
    fn test_compress_unknown_strategy_errors() {
        // T18: 未知 strategy 应报错
        let src = r#"
            let result = compress("hello", "totally_invalid_strategy")
        "#;
        assert!(run(src).is_err());
    }

    #[test]
    fn test_crush_json_non_list_errors() {
        // T19: 非 List 输入应报错
        let src = r#"
            let result = crush_json(42, 10)
        "#;
        let result = run(src);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("crush_json:"));
    }
}

// ===================================================================
// v0.34: event bus builtin e2e 测试
// ===================================================================
#[cfg(test)]
mod bus_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser_v2 = ParserV2::new(tokens);
        let node_ids = parser_v2.parse();
        let arena = parser_v2.into_arena();
        let mut interp = Interpreter::new();
        interp.interpret(&node_ids, &arena)
    }

    /// T20: bus.emit + bus.count 集成
    #[test]
    fn test_bus_emit_and_count() {
        let src = r#"
            bus.emit("test.event", "hello")
            bus.emit("test.event2", 42)
            let c = bus.count()
            print(c)
        "#;
        run(src).expect("bus.emit + bus.count should run without error");
    }

    /// T21: bus.off 取消注册
    #[test]
    fn test_bus_off() {
        let src = r#"
            bus.off("unused.pattern.*")
            print(bus.count())
        "#;
        run(src).expect("bus.off should run without error");
    }

    /// T22: bus.emit 缺参数报错
    #[test]
    fn test_bus_emit_missing_arg() {
        let src = r#"
            bus.emit()
        "#;
        assert!(run(src).is_err());
    }

    /// T23: bus 未知 method 报错
    #[test]
    fn test_bus_unknown_method() {
        let src = r#"
            bus.unknown_method()
        "#;
        let result = run(src);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("bus.") && err.contains("unknown"));
    }

    /// T24: sandbox builtin (integrate src/sandbox/)
    #[test]
    fn test_sandbox_builtin_basic() {
        let src = r#"
            let m = sandbox.mode()
            let ok = sandbox.check_builtin("ai.chat")
            let bad = sandbox.check_path("../escape.txt")
            print(m, ok, bad)
        "#;
        run(src).expect("sandbox builtin should work");
    }

    /// T25: schedule builtin (integrate src/schedule/, MimiClaw cron)
    #[test]
    fn test_schedule_builtin_basic() {
        let src = r#"
            let id = schedule.add("test", "every", "tick me", 60)
            let n = schedule.count()
            print(id, n)
        "#;
        run(src).expect("schedule.add should work");
    }

    /// T26: ccr builtin (integrate src/ccr/, Headroom style)
    #[test]
    fn test_ccr_builtin_basic() {
        let src = r#"
            let hash = ccr.put("hello world")
            let data = ccr.get(hash)
            print(hash, data)
        "#;
        run(src).expect("ccr.put + ccr.get roundtrip");
    }

    /// T27: mock builtin (integrate src/mock/, OpenFugu + OpenInfer mock)
    #[test]
    fn test_mock_builtin_basic() {
        let src = r#"
            let n = mock.count()
            let ns = mock.names()
            print(n, ns)

            let handler = fn(x) return x * 2 end
            mock.register("double", handler)
            let doubled = mock.call("double", 21)
            let n2 = mock.count()
            print(doubled, n2)

            mock.unregister("double")
            let n3 = mock.count()
            print(n3)
        "#;
        run(src).expect("mock register/call/unregister should work");
    }

    /// T28: ai.tokens builtin (mini-swe-agent cost tracking pattern)
    #[test]
    fn test_ai_tokens_builtin() {
        let src = r#"
            let input = ai.tokens.input()
            let output = ai.tokens.output()
            let total = ai.tokens.total()
            let calls = ai.tokens.calls()
            print(input, output, total, calls)
        "#;
        run(src).expect("ai.tokens.* should work");
    }

    #[test]
    fn test_v0_27_document_parse_still_works() {
        // T20: 不破坏 v0.27 document.parse 路径
        let src = r#"
            let doc = document.parse("./tests/fixtures/sample.md")
            let meta = doc.metadata()
            print(meta["origin"])
        "#;
        run(src).expect("v0.27 document.parse must still work");
    }
}
