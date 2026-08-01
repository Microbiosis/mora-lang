mod ai_chat;
mod ai_helpers;
mod builtins;
mod dispatch;
mod trait_dispatch;
// v0.75.x: MirPregelEngine + WorkerPool 已迁至 src/pregel/（解耦 mir ↔ interpreter 循环）

use parking_lot::Mutex;
use std::collections::HashMap;
use std::env;

use crate::checkpoint::Checkpoint;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;

// v1 AST types no longer imported — all v2 paths use ast_v2 / common
// v0.52 ADR-001: ai_infra::* 不再需要（ContextWindow/SpeculativeVerifier/CacheWarmer 迁到 src/runtime/ai.rs）
use crate::flow::*;
use crate::lexer::Lexer;
use crate::trace_collector::TraceCollector;

/// AI 模型配置常量（避免硬编码）
/// 通过环境变量覆盖；未设定时使用默认值。
///
/// 环境变量:
///   MORA_AI_MODEL    — 默认模型名称
///   OPENAI_API_KEY   — API 密钥
///   MORA_AI_BASE_URL — 服务端点 URL
pub const AI_MODEL_ENV: &str = "MORA_AI_MODEL";
pub const AI_MODEL_DEFAULT: &str = "example-model";
pub const AI_API_KEY_ENV: &str = "OPENAI_API_KEY";
pub const AI_BASE_URL_ENV: &str = "MORA_AI_BASE_URL";
pub const AI_BASE_URL_DEFAULT: &str = "https://api.openai.com/v1";

/// v0.55: 使用 ParserV3 解析代码，返回 MirExpr 列表（纯 MIR，零 AST 依赖）
pub fn parse_code_v3(source: &str) -> Result<Vec<crate::mir::expr::MirExpr>, String> {
    let tokens = Lexer::new(source).scan_tokens();
    let parser = crate::parser_v3::ParserV3::new(tokens);
    parser.parse().map_err(|e| format!("{:?}", e))
}

/// 内部 v3 解析辅助（不暴露 Result — mir_import / REPL 失败 panic-out）
fn parse_v3_internal(source: &str) -> Vec<crate::mir::expr::MirExpr> {
    parse_code_v3(source).expect("ParserV3 failed")
}

pub use crate::value::{Environment, StreamReader, Value};

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

/// v0.08.5: BFS 收集 trait + 全部祖先的方法名（去重、防循环）
/// 用于：构造 trait instance 时的完整性检查（与 dispatch 保持一致）
///
/// 参数 trait_registry 是 trait 元数据表（self.registry.trait_registry 借用）
///
/// v0.75.x: LruCache 已下沉到 runtime/types.rs，经下方 re-export 引用。
pub struct Interpreter {
    /// v0.52 ADR-001: CoreRuntime — 核心执行字段（globals/environment/tool_registry/
    /// current_ai_config/config_stack/current_merge_strategies/dynamic_sends）。
    /// v0.70 移除了 worker_channels/worker_receivers 死代码分支。
    pub(crate) core: crate::runtime::core::CoreRuntime,
    /// v0.52 ADR-001: RegistryRuntime facade — BC8 (trait_registry + impl_table + mock_registry +
    /// ccr_store + memory_store)
    pub(crate) registry: crate::runtime::registry::RegistryRuntime,
    /// v0.52 ADR-001: InfraRuntime facade — BC9 (recorder + string_interner + ai_cache + bus + scheduler)
    pub(crate) infra: crate::runtime::infra::InfraRuntime,
    /// v0.52 ADR-001: AiRuntime facade — BC3 (model_routes + token_budget + token_usage + trace +
    /// draft_model_stats + context_window + speculative_verifier + cache_warmer)
    pub(crate) ai: crate::runtime::ai::AiRuntime,
    /// v0.52 ADR-001: SandboxRuntime facade — BC7 (sandbox + container + tool_planes)
    ///
    /// 注：capability 是 module-level state（`src/sandbox/capability.rs::CapabilityStore`），
    /// 不属于 Interpreter 字段 — 保留 module-level 访问。
    pub(crate) sandbox: crate::runtime::sandbox::SandboxRuntime,
    /// v0.52 ADR-001: PersistRuntime facade — BC5 (audit_sink + markdown_memory_dir + checkpoint_saver)
    pub(crate) persist: crate::runtime::persist::PersistRuntime,
    /// v0.52 ADR-001: OrchRuntime facade — BC4 (plans + refine_registry + skill_registry)
    pub(crate) orch: crate::runtime::orch::OrchRuntime,
}

// v0.75.x: 共享数据类型（AiConfigValue/LruCache/RouteConfig/TokenBudget/TokenUsage/
// ToolDef/TraitInfo/TraitMethodSig）与 trait key 辅助函数（impl_method_key /
// default_impl_method_key）已下沉到 runtime/types.rs。
// 此处 re-export 保持既有路径兼容（mir/、stress_tests.rs、interpreter 子模块
// 继续 use crate::interpreter::* 均不受影响）。
pub use crate::runtime::types::{
    AiConfigValue, LruCache, RouteConfig, TokenBudget, TokenUsage, ToolDef, TraitInfo,
    TraitMethodSig,
};
pub(crate) use crate::runtime::types::{default_impl_method_key, impl_method_key};

// v0.04: 显式实现 Clone 而非 derive
// v0.52 ADR-001: Interpreter 已薄化为 7 个 facade holder，Clone 简化
impl Clone for Interpreter {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            registry: self.registry.clone(),
            infra: self.infra.clone(),
            ai: self.ai.clone(),
            sandbox: self.sandbox.clone(),
            persist: self.persist.clone(),
            orch: self.orch.clone(),
        }
    }
}

// v0.75.x: MirHost — MIR 解释器宿主抽象（mir/host.rs）。
// 解耦 mir ↔ interpreter 双向依赖：mir 侧只依赖 trait，Interpreter 实现之。
// 各方法委托既有固有方法/字段；固有方法同名时 `self.method()` 优先固有，
// 无递归风险。
impl crate::mir::host::MirHost for Interpreter {
    fn mir_call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        Interpreter::mir_call_function(self, name, args)
    }

    fn mir_call_method(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        Interpreter::mir_call_method(self, object, method, args)
    }

    fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String> {
        self.call_value(value, args)
    }

    fn mir_import(&mut self, path: &str, env: &mut Environment) -> Result<(), String> {
        Interpreter::mir_import(self, path, env)
    }

    fn mir_with_config(&mut self, bindings: &[(String, Value)]) -> Result<(), String> {
        Interpreter::mir_with_config(self, bindings)
    }

    fn mir_restore_config(&mut self) {
        Interpreter::mir_restore_config(self)
    }

    fn current_merge_strategies(&self) -> Option<HashMap<String, crate::value::MergeStrategy>> {
        self.current_merge_strategies()
    }

    fn environment(&self) -> Arc<parking_lot::Mutex<Environment>> {
        self.core.environment.clone()
    }

    fn dynamic_sends(&mut self) -> &mut Vec<crate::checkpoint::SendTask> {
        &mut self.core.dynamic_sends
    }

    fn checkpoint_saver(&self) -> Option<Arc<dyn crate::checkpoint::CheckpointSaver>> {
        self.persist.checkpoint_saver()
    }

    fn load_checkpoint(&self, thread_id: &str) -> Result<Option<Checkpoint>, String> {
        self.load_checkpoint(thread_id)
    }

    fn trait_registry(&mut self) -> &mut Arc<HashMap<String, TraitInfo>> {
        &mut self.registry.trait_registry
    }

    fn impl_table(&mut self) -> &mut Arc<HashMap<String, Vec<String>>> {
        &mut self.registry.impl_table
    }

    fn clone_box(&self) -> Box<dyn crate::mir::host::MirHost + Send> {
        Box::new(self.clone())
    }
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
        use crate::value::BuiltinKind as Bk;
        {
            let mut g = globals.lock();
            // v0.37 (P1-3.6): use typed BuiltinKind instead of String.
            g.define("print".to_string(), Value::Builtin(Bk::Print), false);
            g.define("range".to_string(), Value::Builtin(Bk::Range), false);
            g.define("len".to_string(), Value::Builtin(Bk::Len), false);
            for (name, kind) in &[
                ("ai", Bk::AiChat),
                ("web", Bk::Web),
                ("json", Bk::Json),
                ("file", Bk::File),
                ("memory", Bk::Memory),
                ("agent", Bk::Agent),
                ("document", Bk::Document),
            ] {
                g.define(name.to_string(), Value::Builtin(*kind), false);
            }
            // v0.26: prompt-section builtins
            g.define(
                "compose_prompt".to_string(),
                Value::Builtin(Bk::ComposePrompt),
                false,
            );
            g.define("tail".to_string(), Value::Builtin(Bk::Tail), false);
            // v0.29: compress / crush_json
            g.define("compress".to_string(), Value::Builtin(Bk::Compress), false);
            g.define(
                "crush_json".to_string(),
                Value::Builtin(Bk::CrushJson),
                false,
            );
            // v0.34: bus / sandbox / schedule / ccr / mock
            g.define("bus".to_string(), Value::Builtin(Bk::Bus), false);
            g.define("sandbox".to_string(), Value::Builtin(Bk::Sandbox), false);
            g.define("schedule".to_string(), Value::Builtin(Bk::Schedule), false);
            g.define("ccr".to_string(), Value::Builtin(Bk::Ccr), false);
            g.define("mock".to_string(), Value::Builtin(Bk::Mock), false);
            // v0.43.0: exec.* — parallel subprocess execution (pi-mono v1 inspired)
            g.define("exec".to_string(), Value::Builtin(Bk::Exec), false);
            // v0.45.0: tool.plane.* — ToolPlane Core/Extension adapter
            g.define("tool".to_string(), Value::Builtin(Bk::Toolplane), false);
            // v0.46.0: skill.* — MoraSkillSpec + dual registry
            g.define("skill".to_string(), Value::Builtin(Bk::Skill), false);
            // v0.48.0: plan.* — real-time checklist (pi-agent)
            g.define("plan".to_string(), Value::Builtin(Bk::Plan), false);
            // v0.48.0: mora.* — meta (refine)
            g.define("mora".to_string(), Value::Builtin(Bk::Mora), false);
        }
        Self {
            core: crate::runtime::core::CoreRuntime {
                globals: globals.clone(),
                environment: globals,
                tool_registry: Arc::new(HashMap::new()),
                ..Default::default()
            },
            // v0.52 ADR-001: 其余 facade 内部 Default
            infra: crate::runtime::infra::InfraRuntime::default(),
            ai: crate::runtime::ai::AiRuntime::default(),
            orch: crate::runtime::orch::OrchRuntime::default(),
            persist: crate::runtime::persist::PersistRuntime::default(),
            sandbox: crate::runtime::sandbox::SandboxRuntime::default(),
            registry: crate::runtime::registry::RegistryRuntime::default(),
        }
    }

    /// v0.04: 构造一个空 Interpreter (用于 std::mem::replace 占位)
    /// 空 Interpreter 不能跑 execute, 仅作为占位符存在
    pub fn new_empty() -> Self {
        let env = Arc::new(Mutex::new(Environment::new()));
        Self {
            core: crate::runtime::core::CoreRuntime {
                globals: env.clone(),
                environment: env,
                ..Default::default()
            },
            infra: crate::runtime::infra::InfraRuntime::default(),
            ai: crate::runtime::ai::AiRuntime::default(),
            orch: crate::runtime::orch::OrchRuntime::default(),
            persist: crate::runtime::persist::PersistRuntime::default(),
            sandbox: crate::runtime::sandbox::SandboxRuntime::default(),
            registry: crate::runtime::registry::RegistryRuntime::default(),
        }
    }

    pub fn new_with_globals(globals: Arc<Mutex<Environment>>) -> Self {
        let env = Arc::new(Mutex::new(Environment::with_parent_of(globals.clone())));
        Self {
            core: crate::runtime::core::CoreRuntime {
                globals: globals.clone(),
                environment: env,
                ..Default::default()
            },
            infra: crate::runtime::infra::InfraRuntime::default(),
            ai: crate::runtime::ai::AiRuntime::default(),
            orch: crate::runtime::orch::OrchRuntime::default(),
            persist: crate::runtime::persist::PersistRuntime::default(),
            sandbox: crate::runtime::sandbox::SandboxRuntime::default(),
            registry: crate::runtime::registry::RegistryRuntime::default(),
        }
    }

    /// v0.51: 回溯到指定检查点之前的步骤（rewind）
    /// v0.74: 修复 — checkpoint id 是 UUID，不再按 `cp-{thread_id}-{step}` 解析。
    /// 改为读取每个 checkpoint 的 `step` 字段判断是否删除。
    pub fn rewind(&mut self, thread_id: &str, before_step: usize) -> Result<(), String> {
        if let Some(ref saver) = self.persist.checkpoint_saver {
            let checkpoints = saver.list(thread_id)?;
            let mut to_remove: Vec<String> = Vec::new();
            for id in checkpoints {
                if let Some(cp) = saver.load(thread_id, Some(&id))? {
                    if cp.step >= before_step {
                        to_remove.push(id);
                    }
                }
            }
            for id in to_remove {
                saver.delete(thread_id, &id)?;
            }
            Ok(())
        } else {
            Err("No checkpoint saver configured".to_string())
        }
    }

    /// v0.50: 从最新检查点恢复执行（resume）
    pub fn resume(&mut self, thread_id: &str) -> Result<HashMap<String, Value>, String> {
        if let Some(ref saver) = self.persist.checkpoint_saver {
            let cp = saver
                .load(thread_id, None)?
                .ok_or_else(|| format!("No checkpoint found for thread {}", thread_id))?;
            Ok(cp.channel_values)
        } else {
            Err("No checkpoint saver configured".to_string())
        }
    }

    /// v0.63: Load the full latest checkpoint for the given thread.
    pub fn load_checkpoint(&self, thread_id: &str) -> Result<Option<Checkpoint>, String> {
        if let Some(ref saver) = self.persist.checkpoint_saver {
            saver.load(thread_id, None)
        } else {
            Ok(None)
        }
    }

    /// v0.66: Persist a checkpoint to the configured saver.
    pub fn save_checkpoint(&self, thread_id: &str, cp: &Checkpoint) -> Result<(), String> {
        match self.persist.checkpoint_saver() {
            Some(saver) => saver.save(thread_id, cp),
            None => Err("No checkpoint saver configured".to_string()),
        }
    }

    /// v0.67: Set per-key CRDT merge strategies for the next transaction/worker.
    /// Subsequent `h_transaction` / `h_worker` invocations will use
    /// `merge_from_with_strategies` with this map. Pass `None` to revert to
    /// the hardcoded LWW fallback.
    pub fn set_merge_strategies(
        &mut self,
        strategies: Option<HashMap<String, crate::value::MergeStrategy>>,
    ) {
        self.core.current_merge_strategies = strategies;
    }

    /// v0.67: Get a clone of the current merge strategies (used by handlers).
    pub fn current_merge_strategies(&self) -> Option<HashMap<String, crate::value::MergeStrategy>> {
        self.core.current_merge_strategies.clone()
    }

    #[allow(dead_code)]
    pub fn get_globals(&self) -> Arc<Mutex<Environment>> {
        self.core.globals.clone()
    }

    pub fn get_tool_registry(&self) -> &HashMap<String, ToolDef> {
        &self.core.tool_registry
    }

    /// 访问 InfraRuntime（二进制 crate 需 accessor 而非直接字段访问）
    pub fn infra(&self) -> &crate::runtime::infra::InfraRuntime {
        &self.infra
    }

    /// 可变访问 InfraRuntime
    pub fn infra_mut(&mut self) -> &mut crate::runtime::infra::InfraRuntime {
        &mut self.infra
    }

    /// α.0: MIR 解释器的函数调用桥。复用 dispatch.rs 的 call_function。
    /// pub(crate) 让 mir::interp 能调用，不暴露给 crate 外。
    pub(crate) fn mir_call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        self.call_function(name, args, crate::common::Span::default())
    }

    /// α.1: MIR 解释器的方法调用桥。复用 dispatch.rs 的 call_method。
    pub(crate) fn mir_call_method(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        self.call_method(object, method, args, crate::common::Span::default())
    }

    /// α.3: MIR 解释器的 import 桥。
    /// 解析 → lowering → run_mir，不依赖 AST 解释器的 execute。
    pub(crate) fn mir_import(
        &mut self,
        path: &str,
        env: &mut crate::value::Environment,
    ) -> Result<(), String> {
        match std::fs::read_to_string(path) {
            Ok(source) => {
                let mut imported_exprs = parse_v3_internal(&source);
                let _type_errs = crate::mir::lower::typecheck_mir_exprs(&mut imported_exprs);
                let imported_func = match crate::mir::lower::lower_mir_exprs(&imported_exprs) {
                    Ok(f) => f,
                    Err(e) => return Err(format!("import lowering error: {}", e)),
                };
                // 子 import 的 env 是当前 env 的克隆（与 with 块语义一致）
                let mut child_env = env.clone();
                // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                let _ = crate::mir::interp::run_mir(
                    &std::sync::Arc::new(imported_func),
                    self,
                    &mut child_env,
                )?;
                // child_env 中的定义合并回父 env
                for (name, val) in child_env.iter() {
                    env.define(name, val, false);
                }
                Ok(())
            }
            Err(e) => Err(format!("import error: {}", e)),
        }
    }

    /// α.2: MIR 解释器的 with 块 config 设置桥。
    /// 保存当前 current_ai_config，应用新 bindings。
    pub(crate) fn mir_with_config(&mut self, bindings: &[(String, Value)]) -> Result<(), String> {
        // 保存到栈（mir_restore_config 弹出）
        self.core
            .config_stack
            .push(self.core.current_ai_config.clone());
        let mut cfg = self.core.current_ai_config.clone().unwrap_or_default();
        for (key, v) in bindings {
            match key.as_str() {
                "model" => cfg.model = Some(v.to_string()),
                "temperature" => {
                    if let Value::Float(n) = v {
                        cfg.temperature = Some(*n);
                    }
                }
                "max_tokens" => {
                    if let Value::Float(n) = v {
                        cfg.max_tokens = Some(*n as usize);
                    }
                }
                "system" => cfg.system = Some(v.to_string()),
                _ => {}
            }
        }
        self.core.current_ai_config = Some(cfg);
        Ok(())
    }

    /// α.2: 恢复 with 块之前的 AI config。
    pub(crate) fn mir_restore_config(&mut self) {
        self.core.current_ai_config = self.core.config_stack.pop().flatten();
    }

    /// 获取可变的当前执行环境（MIR 解释器入口用）
    pub fn take_env(&mut self) -> Environment {
        std::mem::take(&mut *self.core.environment.lock())
    }

    pub fn set_trace_enabled(&mut self, enabled: bool) {
        self.ai.trace = TraceCollector::new(enabled);
    }

    /// α.11: 通过 MIR 解释器求值单个 AST 表达式。
    /// 注：原 `mir_eval_expr` (走 lower_expr_only + ast_v2) 在 v0.55 删除 — 无外部调用者。
    /// 改用纯 MIR 路径请调用 `crate::mir::lower::lower_mir_exprs` + `run_mir`。

    /// v0.22: 字符串驻留 - 相同字符串只存储一次
    pub fn intern_string(&mut self, s: String) -> Value {
        // v0.49.0 (C2): LRU cache + lock (was unbounded HashMap direct access)
        // v0.52: lock 跨 self.infra.string_interner，poison 概率极低 expect
        let mut map = self
            .infra
            .string_interner
            .lock()
            .expect("Interpreter string_interner poisoned");
        if let Some(interned) = map.get(&s) {
            return interned;
        }
        let val = Value::String(s.clone());
        map.put(s, val.clone());
        val
    }

    /// 入口：直接执行 v2 AST
    /// v0.04补: REPL 入口（main.rs 和 serve as repl 共用）
    /// 与 main.rs::run_repl 行为一致：循环读 stdin, 逐行 tokenize+parse+lower+run_mir
    /// 接收外部 &mut Interpreter 保留 setup 代码的 state
    pub fn run_repl_with(interp: &mut Interpreter) {
        use crate::mir::interp::run_mir;
        use crate::mir::lower::{lower_mir_exprs, typecheck_mir_exprs};
        use crate::mir::{MirFunction, MirInst};
        use std::io::{self, BufRead, Write};

        println!("Mora v0.04 REPL — type 'exit' to quit");
        println!();

        let stdin = io::stdin();
        let mut handle = stdin.lock();
        let mut line = String::new();
        let mut env = interp.take_env();
        let mut repl_task_defs: Vec<MirInst> = Vec::new();

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

            let node_ids = parse_v3_internal(trimmed);
            if node_ids.is_empty() {
                continue;
            }

            // v0.35 (P0-C1): REPL also type-checks (other entry points do).
            let mut exprs = node_ids;
            let type_errs = typecheck_mir_exprs(&mut exprs);
            if !type_errs.is_empty() {
                for e in type_errs {
                    eprintln!("type error: {}", e.message);
                }
                continue;
            }

            let func = match lower_mir_exprs(&exprs) {
                Ok(func) => func,
                Err(e) => {
                    eprintln!("MIR lowering error: {}", e);
                    continue;
                }
            };

            let mut body = repl_task_defs.clone();
            body.extend(func.body.clone());
            let run_func = MirFunction {
                params: Vec::new(),
                body,
                n_regs: func.n_regs,
            };

            // v0.75.9: 包裹 Arc 走全局 DAG 缓存（REPL 每行新建 func，天然独立缓存项）
            match run_mir(&std::sync::Arc::new(run_func), interp, &mut env) {
                Ok(value) => {
                    if !matches!(value, Value::Nil) {
                        println!("= {}", value);
                    }
                    repl_task_defs.extend(
                        func.body
                            .into_iter()
                            .filter(|inst| matches!(inst, MirInst::TaskDef { .. })),
                    );
                }
                Err(e) => eprintln!("Error: {}", e),
            }
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
                    Some(Value::Float(n)) => *n as usize,
                    _ => 0,
                };
                let vec = match m.get("embedding") {
                    Some(Value::List(vs)) => vs
                        .iter()
                        .filter_map(|v| {
                            if let Value::Float(n) = v {
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
        Ok(Value::List(vec.into_iter().map(Value::Float).collect()))
    } else {
        // 批量：返回 List<List>
        let items: Vec<Value> = indexed
            .into_iter()
            .map(|(_, v)| Value::List(v.into_iter().map(Value::Float).collect()))
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
