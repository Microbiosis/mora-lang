//! v0.20: 从 interpreter.rs 抽离的运行时值/环境/控制流核心类型。
//!
//! **Move-only refactor** — code copied verbatim from src/interpreter.rs
//! No signature changes, no field changes, no visibility changes.
//! Re-exported in interpreter.rs via `pub use crate::value::*;`

use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::BufReader;
use std::sync::Arc;

// v1 Stmt 已移除 — Value::Task/Closure 不再持有 body

// ─── StreamReader ─────────────────────────────────────────
/// 包装 BufReader<Box<dyn Read + Send + Sync>>，实现 Debug/Clone
#[derive(Clone)]
pub struct StreamReader(Arc<Mutex<BufReader<Box<dyn std::io::Read + Send + Sync>>>>);

impl StreamReader {
    pub fn new(reader: BufReader<Box<dyn std::io::Read + Send + Sync>>) -> Self {
        StreamReader(Arc::new(Mutex::new(reader)))
    }
    pub fn lock(
        &self,
    ) -> parking_lot::MutexGuard<'_, BufReader<Box<dyn std::io::Read + Send + Sync>>> {
        self.0.lock()
    }
}

impl std::fmt::Debug for StreamReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamReader")
    }
}

// ─── Value ───────────────────────────────────────────────

/// v0.37 (P1-3.6): Typed enum replacing stringly-typed builtin dispatch.
/// The original audit flagged 30+ string comparisons across dispatch,
/// Display, JSON encoding, and registration sites as weak typing.
/// Variants are derived directly from v0.36 mod.rs:346-416 plus the
/// additional builtin kinds the dispatch table knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinKind {
    Print,
    Range,
    Len,
    Web,
    Json,
    File,
    Memory,
    Bus,
    Sandbox,
    Schedule,
    Ccr,
    Mock,
    AiTokens,
    AiChat,
    Agent,
    Document,
    Compress,
    CrushJson,
    Tail,
    ComposePrompt,
    Router,
    McpServer,
    // v0.43.0: exec.* — parallel subprocess execution (pi-mono v1 inspired)
    Exec,
    // v0.45.0: tool.plane.* — ToolPlane Core/Extension adapter (loongclaw)
    Toolplane,
    // v0.45.0: ai.* — AI utilities (retry / role / reflection)
    Ai,
    // v0.46.0: skill.* — MoraSkillSpec + dual registry (CLI-Anything)
    Skill,
    // v0.48.0: plan.* — real-time checklist (pi-agent update_plan)
    Plan,
    // v0.48.0: mora.* — meta (refine / list-plans) (CLI-Anything /refine)
    Mora,
}

impl std::fmt::Display for BuiltinKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BuiltinKind::Print => "print",
            BuiltinKind::Range => "range",
            BuiltinKind::Len => "len",
            BuiltinKind::Web => "web",
            BuiltinKind::Json => "json",
            BuiltinKind::File => "file",
            BuiltinKind::Memory => "memory",
            BuiltinKind::Bus => "bus",
            BuiltinKind::Sandbox => "sandbox",
            BuiltinKind::Schedule => "schedule",
            BuiltinKind::Ccr => "ccr",
            BuiltinKind::Mock => "mock",
            BuiltinKind::AiTokens => "ai.tokens",
            BuiltinKind::AiChat => "ai.chat",
            BuiltinKind::Agent => "agent",
            BuiltinKind::Document => "document",
            BuiltinKind::Compress => "compress",
            BuiltinKind::CrushJson => "crush_json",
            BuiltinKind::Tail => "tail",
            BuiltinKind::ComposePrompt => "compose_prompt",
            BuiltinKind::Router => "Router::new",
            BuiltinKind::McpServer => "McpServer::new",
            BuiltinKind::Exec => "Exec::new",
            BuiltinKind::Toolplane => "Toolplane::new",
            BuiltinKind::Skill => "Skill::new",
            BuiltinKind::Plan => "Plan::new",
            BuiltinKind::Mora => "Mora::new",
            BuiltinKind::Ai => "Ai::new",
        };
        f.write_str(s)
    }
}

/// v0.40: Immutable Environment snapshot for closure captures.
///
/// Wraps a Box<Environment>. Unlike the legacy Arc<Mutex<Environment>>,
/// an EnvRef is owned — the captured env is frozen at capture time
/// and cannot be mutated by any other thread or closure. This also
/// makes EnvRef Send (Box<Environment> is Send because Environment
/// contains only Send-safe fields).
#[derive(Debug, Clone)]
pub struct EnvRef(pub Box<Environment>);

impl EnvRef {
    /// Returns an immutable reference to the inner Environment.
    pub fn env(&self) -> &Environment {
        &self.0
    }

    /// v0.40: convert an Arc<Mutex<Environment>> (legacy) into an
    /// EnvRef snapshot. The snapshot clones the Environment contents
    /// at capture time and is immutable thereafter.
    pub fn from_arc_mutex(parent: Arc<Mutex<Environment>>) -> Self {
        let env_clone = parent.lock().clone();
        EnvRef(Box::new(env_clone))
    }
}

#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    /// v0.x: 单字符（`string[number]` 索引结果）
    Char(char),
    // v0.38: Numeric tower — distinct Int and Float variants.
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    List(Vec<Value>),
    Dict(HashMap<String, Value>),
    Task {
        name: String,
        params: Vec<String>,
        /// α.10: MIR-built task 体（α.7/α.8 trait/impl/skill 由 MIR lowering 填）。
        /// 调用方一律走 run_mir；不再保留 v2 arena fallback。
        mir_body: std::sync::Arc<crate::mir::MirFunction>,
    },
    /// v0.54: 工具声明 — 可被 AI 调用的命名工具
    Tool {
        name: String,
        description: String,
        params: Vec<String>,
        return_type: Option<String>,
        /// α.10: MIR-built tool body。
        mir_body: std::sync::Arc<crate::mir::MirFunction>,
    },
    Closure {
        params: Vec<String>,
        /// v0.40: env is now EnvRef (Local Rc<RefCell> or Owned Box<Environment>)
        /// instead of Arc<Mutex<Environment>>. Callers convert via
        /// EnvRef::from_arc_mutex(arc) for legacy Arc<Mutex<>> sources.
        env: EnvRef,
        /// α.10/α.11: MIR-built 闭包体。所有 closure 必须有 body；
        /// dispatch 走 run_mir 不再有 arena fallback（AGENTS_CODE_MODIFICATION §28）。
        /// Arc 而非 Rc 以保留 Value: Send + Sync（http_server 跨 task 共享 Value）。
        mir_body: std::sync::Arc<crate::mir::MirFunction>,
    },
    Builtin(BuiltinKind),
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
    // v0.06: AiConfig 值类型
    AiConfig {
        model: Option<String>,
        temperature: Option<f64>,
        max_tokens: Option<usize>,
        system: Option<String>,
        budget: Option<usize>,
    },
    // v0.06.3: Router 值类型 — 路由用 Arc 包避免递归类型
    Router {
        routes: Arc<Mutex<Vec<(String, String, Value)>>>, // (method, path, handler)
    },
    // v0.06.3: HttpRequest 值类型
    HttpRequest {
        method: String,
        path: String,
        query: String,
        body: Box<Value>,
        params: HashMap<String, String>,
    },
    // v0.06.6: McpServer 值类型
    McpServer {
        tools: Vec<(String, Value)>, // (tool_name, handler)
    },
    // v0.08.5: trait 对象 — 携带 data + for_type + trait_name（一等值类型）
    // v0.09: 加 for_generics + trait_generics 两个字段
    //   for_generics: for_type 的泛型参数（如 `Boxed<T>` 的 `T`）
    //   trait_generics: trait 的泛型参数（如 `Container<number>` 的 `number`）
    // 不同实例化产生不同 dispatch key，避免冲突
    TraitObject {
        for_generics: Vec<String>,
        trait_generics: Vec<String>,
        for_type: String,
        trait_name: String,
        data: Box<Value>,
    },
    // v0.17: Compose 组合函数
    Compose(Vec<Value>),
    // v0.18: Partial 部分应用
    Partial(Box<Value>, Vec<Value>),
    // v0.19: Atom 可变引用 (Clojure 启发)
    Atom(Arc<Mutex<Value>>),
    // v0.20: 宏定义 (Common Lisp 启发)
    Macro {
        name: String,
        params: Vec<String>,
    },
    // v0.26: Prompt 分段 — 一段有 role / text / byte 预算的 system prompt 片段
    // (灵感: mimiclaw 的 5 段固定缓冲 + headroom 的内容感知压缩器)
    PromptSection {
        name: String,
        role: Option<String>,
        text: Box<Value>,
        budget_bytes: Option<usize>,
    },
    // v0.27: Document 统一 IR — 封装一个 Arc<dyn DocumentBackend>，
    // 二进制原始字节永不出现在 Value 树中
    Document {
        backend: std::sync::Arc<dyn crate::document::DocumentBackend>,
        metadata: std::collections::HashMap<String, Value>,
    },
}

// 手动实现 PartialEq（EnvRef 不支持自动派生）
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (
                Value::PromptSection {
                    name: a,
                    role: ra,
                    text: ta,
                    budget_bytes: ba,
                },
                Value::PromptSection {
                    name: b,
                    role: rb,
                    text: tb,
                    budget_bytes: bb,
                },
            ) => a == b && ra == rb && ta == tb && ba == bb,
            (Value::Document { metadata: a, .. }, Value::Document { metadata: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "{}", s),
            Value::Char(c) => write!(f, "{}", c),
            Value::Int(i) => write!(f, "{}", i),
            Value::Float(n) => {
                // v0.36 (P1-3.13): never panic on NaN/Inf — Display must be infallible.
                if n.is_nan() {
                    f.write_str("nan")
                } else if n.is_infinite() {
                    if *n > 0.0 {
                        f.write_str("inf")
                    } else {
                        f.write_str("-inf")
                    }
                } else {
                    write!(f, "{}", n)
                }
            }
            Value::Bool(b) => write!(f, "{}", b),
            Value::Nil => write!(f, "nil"),
            Value::List(items) => {
                // v0.36 (P1-2.7 + P2-3.14): streaming write, no Vec<String> build.
                // Depth-limited via fmt_inner helper below to guard against
                // recursive Value::List / Value::Atom cycles.
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    fmt_inner(f, v, 1)?;
                }
                write!(f, "]")
            }
            Value::Dict(map) => {
                write!(f, "{{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: ", k)?;
                    fmt_inner(f, v, 1)?;
                }
                write!(f, "}}")
            }
            Value::Task { name, .. } => write!(f, "<task {}>", name),
            Value::Tool { name, .. } => write!(f, "<tool {}>", name),
            Value::Closure { .. } => write!(f, "<closure>"),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Conversation {
                model, messages, ..
            } => {
                write!(f, "<conversation {} ({} messages)>", model, messages.len())
            }
            Value::Stream { .. } => write!(f, "<stream>"),
            Value::Agent { name, .. } => write!(f, "<agent {}>", name),
            Value::AiConfig {
                model,
                temperature,
                max_tokens,
                system,
                budget,
            } => {
                write!(
                    f,
                    "AiConfig(model={:?}, temp={:?}, max_tokens={:?}, system={:?}, budget={:?})",
                    model, temperature, max_tokens, system, budget
                )
            }
            Value::Router { routes } => {
                // v0.35: Display must be infallible; parking_lot::Mutex does not poison.
                let route_count = routes.lock().len();
                write!(f, "<router ({} routes)>", route_count)
            }
            Value::HttpRequest { method, path, .. } => {
                write!(f, "<http_request {} {}>", method, path)
            }
            Value::McpServer { tools } => write!(f, "<mcp_server ({} tools)>", tools.len()),
            Value::TraitObject {
                for_type,
                trait_name,
                for_generics: _,
                trait_generics: _,
                data,
            } => {
                write!(
                    f,
                    "<trait_object for={} as {} data={:?}>",
                    for_type, trait_name, data
                )
            }
            Value::Compose(funcs) => {
                write!(f, "<compose({} funcs)>", funcs.len())
            }
            Value::Partial(_, _) => {
                write!(f, "<partial>")
            }
            Value::Atom(arc) => {
                // v0.35: Display must be infallible; parking_lot::Mutex does not poison.
                let v = arc.lock();
                write!(f, "<atom {:?}>", v)
            }
            Value::Macro { name, .. } => {
                write!(f, "<macro {}>", name)
            }
            Value::PromptSection {
                name,
                role,
                budget_bytes,
                ..
            } => {
                write!(
                    f,
                    "<prompt_section name={} role={:?} budget={:?}>",
                    name, role, budget_bytes
                )
            }
            Value::Document { backend, .. } => {
                write!(f, "<document origin=\"{}\">", backend.origin())
            }
        }
    }
}

/// v0.36 (P2-3.14): depth-limited Display helper. Walks a Value recursively
/// but stops at MAX_DEPTH (default 16) to prevent stack overflow on
/// recursive/cyclic structures (e.g. Atom containing self).
const DISPLAY_MAX_DEPTH: usize = 16;

fn fmt_inner(f: &mut std::fmt::Formatter<'_>, v: &Value, depth: usize) -> std::fmt::Result {
    if depth > DISPLAY_MAX_DEPTH {
        return f.write_str("…");
    }
    match v {
        Value::Atom(arc) => {
            let inner = arc.lock();
            write!(f, "<atom {:?}>", inner)
        }
        Value::List(items) => {
            write!(f, "[")?;
            for (i, child) in items.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                fmt_inner(f, child, depth + 1)?;
            }
            write!(f, "]")
        }
        Value::Dict(map) => {
            write!(f, "{{")?;
            for (i, (k, child)) in map.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: ", k)?;
                fmt_inner(f, child, depth + 1)?;
            }
            write!(f, "}}")
        }
        _ => write!(f, "{}", v),
    }
}

// ─── Merge Strategy ─────────────────────────────────────────
/// v0.59: CRDT-inspired merge strategies for concurrent state.
///
/// When two environments (or state channels) write to the same key,
/// the merge strategy determines how the values are combined.
#[derive(Debug, Clone, PartialEq)]
pub enum MergeStrategy {
    /// Child overwrites parent (classic last-write-wins).
    LastWriteWins,
    /// List: concatenate. String: concatenate. Other: LWW.
    Append,
    /// Int/Float: numeric addition. Other: LWW.
    Add,
    /// Dict: key-level merge (child keys win on conflict).
    DictUnion,
    /// v0.75.5: G-Set（grow-only set）— List: 并集（只加新元素）；
    /// Dict: key 级并集（child 的 key 仅在 parent 缺失时插入）；其他 LWW。
    GrowOnlySet,
}

impl MergeStrategy {
    /// v0.75.24: 策略名 → 枚举的单一事实来源（替代运行时/typeck 各自的
    /// 硬编码字符串 match）。`merge_with(key, strategy)` 的运行时解析与
    /// typeck 字面量校验都走这里。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "lww" | "last_write_wins" => Some(Self::LastWriteWins),
            "append" => Some(Self::Append),
            "add" => Some(Self::Add),
            "dict_union" => Some(Self::DictUnion),
            "grow_only_set" => Some(Self::GrowOnlySet),
            _ => None,
        }
    }
}

impl Value {
    /// Merge two values using the given strategy.
    /// Falls back to `LastWriteWins` if the strategy doesn't apply
    /// to the value types.
    pub fn merge(parent: Value, child: Value, strategy: &MergeStrategy) -> Value {
        match strategy {
            MergeStrategy::LastWriteWins => child,
            MergeStrategy::Append => match (parent, child) {
                (Value::List(mut a), Value::List(b)) => {
                    a.extend(b);
                    Value::List(a)
                }
                (Value::String(a), Value::String(b)) => Value::String(a + &b),
                (_, child) => child, // fallback: LWW
            },
            MergeStrategy::Add => match (parent, child) {
                (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
                (Value::Float(a), Value::Float(b)) => Value::Float(a + b),
                (_, child) => child,
            },
            MergeStrategy::DictUnion => match (parent, child) {
                (Value::Dict(mut a), Value::Dict(b)) => {
                    for (k, v) in b {
                        a.insert(k, v);
                    }
                    Value::Dict(a)
                }
                (_, child) => child,
            },
            MergeStrategy::GrowOnlySet => match (parent, child) {
                (Value::List(mut a), Value::List(b)) => {
                    for item in b {
                        if !a.contains(&item) {
                            a.push(item);
                        }
                    }
                    Value::List(a)
                }
                (Value::Dict(mut a), Value::Dict(b)) => {
                    for (k, v) in b {
                        a.entry(k).or_insert(v);
                    }
                    Value::Dict(a)
                }
                (_, child) => child, // fallback: LWW
            },
        }
    }
}

// ─── Vector Clock ─────────────────────────────────────────
/// v0.61: Vector clock for causal consistency in concurrent environments.
///
/// Maps agent/node name → logical counter. Used to detect concurrent
/// (non-causally-ordered) modifications during environment merges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VectorClock {
    entries: HashMap<String, u64>,
}

impl VectorClock {
    /// Increment this agent's counter by 1.
    pub fn tick(&mut self, agent: &str) {
        *self.entries.entry(agent.to_string()).or_insert(0) += 1;
    }

    /// Merge another clock: take the maximum counter for each agent.
    pub fn merge(&mut self, other: &VectorClock) {
        for (k, &v) in &other.entries {
            let e = self.entries.entry(k.clone()).or_insert(0);
            *e = (*e).max(v);
        }
    }

    /// True if `a` happened-before `b` (strict partial order).
    ///
    /// Condition: ∀k: a[k] ≤ b[k] AND ∃k: a[k] < b[k].
    pub fn happened_before(a: &VectorClock, b: &VectorClock) -> bool {
        let mut has_strict = false;
        for k in a.entries.keys().chain(b.entries.keys()) {
            let av = a.entries.get(k).copied().unwrap_or(0);
            let bv = b.entries.get(k).copied().unwrap_or(0);
            if av > bv {
                return false;
            }
            if av < bv {
                has_strict = true;
            }
        }
        has_strict
    }

    /// True if neither clock happened-before the other.
    ///
    /// Two clocks are concurrent when they have conflicting information —
    /// each has at least one counter greater than the other.
    /// Equal clocks (same causal history) are NOT concurrent.
    pub fn concurrent(a: &VectorClock, b: &VectorClock) -> bool {
        a != b && !Self::happened_before(a, b) && !Self::happened_before(b, a)
    }

    /// True if this clock has no entries (freshly created).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// v0.63: Serialize to a Dict for checkpoint storage.
    pub fn to_dict(&self) -> HashMap<String, Value> {
        self.entries
            .iter()
            .map(|(k, &v)| (k.clone(), Value::Int(v as i64)))
            .collect()
    }

    /// v0.63: Deserialize from a Dict (checkpoint restore).
    pub fn from_dict(d: &HashMap<String, Value>) -> Self {
        let entries: HashMap<String, u64> = d
            .iter()
            .filter_map(|(k, v)| match v {
                Value::Int(n) => Some((k.clone(), *n as u64)),
                Value::Float(n) => Some((k.clone(), *n as u64)),
                _ => None,
            })
            .collect();
        VectorClock { entries }
    }
}

// ─── Conflict ──────────────────────────────────────────────

/// v0.61: Detected write-write conflict during environment merge.
///
/// Captured when two environments modified the same key with
/// concurrent clocks (neither happened-before the other).
#[derive(Debug, Clone)]
pub struct Conflict {
    pub key: String,
    pub parent_value: Value,
    pub child_value: Value,
    pub parent_clock: VectorClock,
    pub child_clock: VectorClock,
}

// ─── Environment ─────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct Environment {
    pub values: HashMap<String, Arc<Mutex<Value>>>,
    pub exports: HashMap<String, Arc<Mutex<Value>>>,
    pub parent: Option<Arc<Mutex<Environment>>>,
    /// v0.61: Per-binding version clocks (which agent modified each key).
    pub versions: HashMap<String, VectorClock>,
    /// v0.61: This environment's own vector clock.
    pub clock: VectorClock,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
            exports: HashMap::new(),
            parent: None,
            versions: HashMap::new(),
            clock: VectorClock::default(),
        }
    }

    pub fn with_parent_of(parent: Arc<Mutex<Environment>>) -> Self {
        Self {
            values: HashMap::new(),
            exports: HashMap::new(),
            parent: Some(parent),
            versions: HashMap::new(),
            clock: VectorClock::default(),
        }
    }

    /// v0.40: accept Rc<RefCell<>> for the new env model. Converts
    /// to Arc<Mutex<>> internally for now (C1 shim, removed in C4).
    pub fn with_parent_of_rc(parent: std::rc::Rc<std::cell::RefCell<Environment>>) -> Self {
        Self::with_parent_of(Arc::new(Mutex::new(parent.borrow().clone())))
    }

    pub fn define(&mut self, name: String, value: Value, exported: bool) {
        let arc = Arc::new(Mutex::new(value.clone()));
        self.values.insert(name.clone(), arc.clone());
        // v0.61: record the current clock for this binding
        self.versions.insert(name.clone(), self.clock.clone());
        if exported {
            self.exports.insert(name, arc);
        }
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(arc) = self.values.get(name) {
            Some(arc.lock().clone())
        } else if let Some(parent) = &self.parent {
            parent.lock().get(name)
        } else {
            None
        }
    }

    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        if let Some(arc) = self.values.get(name) {
            *arc.lock() = value;
            // v0.61: update the version clock for this binding
            self.versions.insert(name.to_string(), self.clock.clone());
            true
        } else if let Some(parent) = &self.parent {
            let result = parent.lock().assign(name, value);
            // v0.64: Bug fix — also update local clock for parent-scope writes.
            // Without this, concurrent modifications through the parent chain
            // would carry stale clocks and fail conflict detection.
            if result {
                self.versions.insert(name.to_string(), self.clock.clone());
            }
            result
        } else {
            false
        }
    }

    // v0.21: 所有权语义支持

    /// 获取绑定状态
    pub fn get_binding(&self, name: &str) -> Option<Binding> {
        if let Some(arc) = self.values.get(name) {
            Some(Binding::Value(arc.lock().clone()))
        } else if let Some(parent) = &self.parent {
            parent.lock().get_binding(name)
        } else {
            None
        }
    }

    /// 移动变量（所有权转移）
    pub fn move_variable(&mut self, name: &str) -> Result<Value, String> {
        if let Some(arc) = self.values.remove(name) {
            Ok(arc.lock().clone())
        } else if let Some(parent) = &self.parent {
            parent.lock().move_variable(name)
        } else {
            Err(format!("undefined variable: {}", name))
        }
    }

    /// 借用变量（不可变）— 返回共享 Arc，修改会反映到原变量
    pub fn borrow_variable(&self, name: &str) -> Result<Arc<Mutex<Value>>, String> {
        if let Some(arc) = self.values.get(name) {
            Ok(Arc::clone(arc))
        } else if let Some(parent) = &self.parent {
            parent.lock().borrow_variable(name)
        } else {
            Err(format!("undefined variable: {}", name))
        }
    }

    /// 可变借用变量 — 返回共享 Arc，修改会反映到原变量
    pub fn borrow_variable_mut(&mut self, name: &str) -> Result<Arc<Mutex<Value>>, String> {
        if let Some(arc) = self.values.get(name) {
            Ok(Arc::clone(arc))
        } else if let Some(parent) = &self.parent {
            parent.lock().borrow_variable_mut(name)
        } else {
            Err(format!("undefined variable: {}", name))
        }
    }

    /// 迭代环境中的所有绑定（仅当前层，不含 parent），供 import/子 env 合并用。
    /// 返回 (name, Value) 的克隆，避免借用临时 MutexGuard。
    pub fn iter(&self) -> Vec<(String, Value)> {
        self.values
            .iter()
            .map(|(k, v)| (k.clone(), v.lock().clone()))
            .collect()
    }

    /// v0.59: Merge bindings from a child environment into this one.
    ///
    /// For each binding in `child`, if the key already exists in `self`,
    /// the values are merged using the given strategy. Otherwise the
    /// child binding is defined as new.
    ///
    /// v0.61: Also merges per-binding version clocks and the environment-level clock.
    pub fn merge_from(&mut self, child: &Environment, strategy: &MergeStrategy) {
        for (name, child_val) in values_iter(child) {
            match self.values.get(&name) {
                Some(parent_arc) => {
                    let parent_val = parent_arc.lock().clone();
                    let merged = Value::merge(parent_val, child_val, strategy);
                    *parent_arc.lock() = merged;
                    // Merge version clock for this binding
                    if let Some(child_v) = child.versions.get(&name) {
                        self.versions
                            .entry(name.clone())
                            .or_default()
                            .merge(child_v);
                    }
                }
                None => {
                    // Carry over child's version clock before moving `name`
                    let child_ver = child.versions.get(&name).cloned();
                    self.define(name.clone(), child_val, false);
                    if let Some(child_v) = child_ver {
                        self.versions.insert(name, child_v);
                    }
                }
            }
        }
        self.clock.merge(&child.clock);
    }

    /// v0.60: Merge bindings with per-key strategies.
    ///
    /// Keys listed in `strategies` use their specific strategy; all
    /// other keys fall back to `default`.
    ///
    /// v0.61: Returns detected write-write conflicts (concurrent clocks).
    /// Also merges per-binding version clocks.
    pub fn merge_from_with_strategies(
        &mut self,
        child: &Environment,
        strategies: &HashMap<String, MergeStrategy>,
        default: &MergeStrategy,
    ) -> Vec<Conflict> {
        let mut conflicts = Vec::new();
        for (name, child_val) in values_iter(child) {
            let strategy = strategies.get(&name).unwrap_or(default);
            match self.values.get(&name) {
                Some(parent_arc) => {
                    let parent_clock = self.versions.get(&name).cloned().unwrap_or_default();
                    let child_clock = child.versions.get(&name).cloned().unwrap_or_default();

                    // Detect concurrent modifications
                    if !parent_clock.is_empty()
                        && !child_clock.is_empty()
                        && VectorClock::concurrent(&parent_clock, &child_clock)
                    {
                        conflicts.push(Conflict {
                            key: name.clone(),
                            parent_value: parent_arc.lock().clone(),
                            child_value: child_val.clone(),
                            parent_clock: parent_clock.clone(),
                            child_clock: child_clock.clone(),
                        });
                    }

                    let parent_val = parent_arc.lock().clone();
                    let merged = Value::merge(parent_val, child_val, strategy);
                    *parent_arc.lock() = merged;
                    // Merge clocks: take max per agent
                    let mut merged_clock = parent_clock;
                    merged_clock.merge(&child_clock);
                    self.versions.insert(name.clone(), merged_clock);
                }
                None => {
                    // v0.61: new binding — carry over child's clock
                    if let Some(child_v) = child.versions.get(&name) {
                        self.versions.insert(name.clone(), child_v.clone());
                    }
                    self.define(name, child_val, false);
                }
            }
        }
        self.clock.merge(&child.clock);
        conflicts
    }
}

/// Iterate bindings from an Environment without consuming it.
fn values_iter(env: &Environment) -> Vec<(String, Value)> {
    env.values
        .iter()
        .map(|(k, v)| (k.clone(), v.lock().clone()))
        .collect()
}

// ─── Binding (v0.21: 所有权语义) ───────────────────────
/// 变量绑定状态，支持移动语义
#[derive(Debug, Clone)]
pub enum Binding {
    /// 正常值
    Value(Value),
    /// 已移动（所有权转移）
    Moved,
    /// 不可变借用
    Borrowed(Arc<Mutex<Value>>),
    /// 可变借用
    BorrowedMut(Arc<Mutex<Value>>),
}

impl Binding {
    pub fn is_moved(&self) -> bool {
        matches!(self, Binding::Moved)
    }

    pub fn is_borrowed(&self) -> bool {
        matches!(self, Binding::Borrowed(_) | Binding::BorrowedMut(_))
    }

    pub fn is_borrowed_mut(&self) -> bool {
        matches!(self, Binding::BorrowedMut(_))
    }

    pub fn get_value(&self) -> Option<&Value> {
        match self {
            Binding::Value(v) => Some(v),
            _ => None,
        }
    }

    pub fn into_value(self) -> Result<Value, String> {
        match self {
            Binding::Value(v) => Ok(v),
            Binding::Moved => Err("use of moved value".to_string()),
            Binding::Borrowed(_) | Binding::BorrowedMut(_) => {
                Err("cannot move out of borrowed value".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.35 (P0-B2): Display must be infallible even if the inner mutex is poisoned.
    /// Smoke test: a Router with an empty routes Vec should render without panic.
    #[test]
    fn router_display_does_not_panic_on_empty_routes() {
        let v = Value::Router {
            routes: Arc::new(Mutex::new(Vec::new())),
        };
        let s = format!("{}", v);
        assert!(s.contains("router"), "got: {}", s);
        assert!(s.contains("0 routes"), "got: {}", s);
    }

    /// v0.35 (P0-B2): Atom Display must not panic (smoke test).
    #[test]
    fn atom_display_does_not_panic_on_valid_value() {
        let v = Value::Atom(Arc::new(Mutex::new(Value::Float(42.0))));
        let s = format!("{}", v);
        assert!(s.contains("atom"), "got: {}", s);
        assert!(s.contains("42"), "got: {}", s);
    }

    /// v0.36 (P1-3.13): Number Display should render NaN/Inf without panicking.
    #[test]
    fn number_display_handles_nan() {
        let v = Value::Float(f64::NAN);
        let s = format!("{}", v);
        assert_eq!(s, "nan");
    }

    #[test]
    fn number_display_handles_pos_inf() {
        let v = Value::Float(f64::INFINITY);
        let s = format!("{}", v);
        assert_eq!(s, "inf");
    }

    #[test]
    fn number_display_handles_neg_inf() {
        let v = Value::Float(f64::NEG_INFINITY);
        let s = format!("{}", v);
        assert_eq!(s, "-inf");
    }

    #[test]
    fn number_display_normal_value() {
        let v = Value::Float(42.5);
        let s = format!("{}", v);
        assert_eq!(s, "42.5");
    }

    /// v0.40: EnvRef smoke test.
    #[test]
    fn envref_from_arc_mutex_roundtrip() {
        let mut e = Environment::new();
        e.define("x".to_string(), Value::String("y".to_string()), false);
        let arc = Arc::new(Mutex::new(e));
        let r = EnvRef::from_arc_mutex(arc);
        assert_eq!(r.env().get("x"), Some(Value::String("y".to_string())));
    }

    // ─── Merge tests ──────────────────────────────────────────

    #[test]
    fn merge_add_ints() {
        assert_eq!(
            Value::merge(Value::Int(5), Value::Int(3), &MergeStrategy::Add),
            Value::Int(8)
        );
    }

    #[test]
    fn merge_add_floats() {
        assert_eq!(
            Value::merge(Value::Float(1.5), Value::Float(2.5), &MergeStrategy::Add),
            Value::Float(4.0)
        );
    }

    #[test]
    fn merge_append_lists() {
        assert_eq!(
            Value::merge(
                Value::List(vec![Value::Int(1)]),
                Value::List(vec![Value::Int(2)]),
                &MergeStrategy::Append
            ),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn merge_append_strings() {
        assert_eq!(
            Value::merge(
                Value::String("a".into()),
                Value::String("b".into()),
                &MergeStrategy::Append
            ),
            Value::String("ab".into())
        );
    }

    #[test]
    fn merge_dict_union() {
        let mut a = HashMap::new();
        a.insert("x".into(), Value::Int(1));
        let mut b = HashMap::new();
        b.insert("y".into(), Value::Int(2));
        let mut expected = HashMap::new();
        expected.insert("x".into(), Value::Int(1));
        expected.insert("y".into(), Value::Int(2));
        assert_eq!(
            Value::merge(Value::Dict(a), Value::Dict(b), &MergeStrategy::DictUnion),
            Value::Dict(expected)
        );
    }

    #[test]
    fn merge_lww_is_child_wins() {
        assert_eq!(
            Value::merge(Value::Int(1), Value::Int(99), &MergeStrategy::LastWriteWins),
            Value::Int(99)
        );
    }

    #[test]
    fn merge_fallback_to_lww() {
        // String + Int with Add strategy — can't add, falls back to child
        assert_eq!(
            Value::merge(
                Value::String("x".into()),
                Value::Int(42),
                &MergeStrategy::Add
            ),
            Value::Int(42)
        );
    }

    // ─── v0.75.5: G-Set（grow-only set）───

    #[test]
    fn merge_grow_only_set_lists() {
        // 并集：parent ∪ child，只加新元素
        assert_eq!(
            Value::merge(
                Value::List(vec![Value::Int(1), Value::Int(2)]),
                Value::List(vec![Value::Int(2), Value::Int(3)]),
                &MergeStrategy::GrowOnlySet
            ),
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn merge_grow_only_set_dicts() {
        // key 级并集：child 的 key 仅在 parent 缺失时插入，不覆盖已存在 key
        let parent = Value::Dict([("a".to_string(), Value::Int(1))].into_iter().collect());
        let child = Value::Dict(
            [
                ("a".to_string(), Value::Int(99)),
                ("b".to_string(), Value::Int(2)),
            ]
            .into_iter()
            .collect(),
        );
        let merged = Value::merge(parent, child, &MergeStrategy::GrowOnlySet);
        let map = match merged {
            Value::Dict(m) => m,
            _ => panic!("expected dict"),
        };
        assert_eq!(map.get("a"), Some(&Value::Int(1)), "已存在 key 不被覆盖");
        assert_eq!(map.get("b"), Some(&Value::Int(2)));
    }

    #[test]
    fn merge_grow_only_set_fallback_lww() {
        // 非 List/Dict 走 child（与 Append/Add 的 fallback 模式一致）
        assert_eq!(
            Value::merge(Value::Int(1), Value::Int(42), &MergeStrategy::GrowOnlySet),
            Value::Int(42)
        );
    }

    #[test]
    fn env_merge_with_grow_only_set_strategy() {
        let mut parent = Environment::new();
        parent.define(
            "tags".into(),
            Value::List(vec![Value::String("a".into())]),
            false,
        );
        let mut child = Environment::new();
        child.define(
            "tags".into(),
            Value::List(vec![Value::String("a".into()), Value::String("b".into())]),
            false,
        );
        let mut strategies = HashMap::new();
        strategies.insert("tags".to_string(), MergeStrategy::GrowOnlySet);
        parent.merge_from_with_strategies(&child, &strategies, &MergeStrategy::LastWriteWins);
        assert_eq!(
            parent.get("tags"),
            Some(Value::List(vec![
                Value::String("a".into()),
                Value::String("b".into())
            ]))
        );
    }

    #[test]
    fn env_merge_from_new_bindings() {
        let mut parent = Environment::new();
        parent.define("a".into(), Value::Int(1), false);
        let mut child = Environment::new();
        child.define("b".into(), Value::Int(2), false);
        parent.merge_from(&child, &MergeStrategy::LastWriteWins);
        assert_eq!(parent.get("a"), Some(Value::Int(1)));
        assert_eq!(parent.get("b"), Some(Value::Int(2)));
    }

    #[test]
    fn env_merge_from_conflict_uses_strategy() {
        let mut parent = Environment::new();
        parent.define("x".into(), Value::Int(5), false);
        let mut child = Environment::new();
        child.define("x".into(), Value::Int(3), false);
        parent.merge_from(&child, &MergeStrategy::Add);
        assert_eq!(parent.get("x"), Some(Value::Int(8)));
    }

    #[test]
    fn env_merge_with_per_key_strategies() {
        let mut parent = Environment::new();
        parent.define("counter".into(), Value::Int(100), false);
        parent.define("log".into(), Value::List(vec![]), false);
        parent.define("name".into(), Value::String("alice".into()), false);

        let mut child = Environment::new();
        child.define("counter".into(), Value::Int(5), false);
        child.define(
            "log".into(),
            Value::List(vec![Value::String("msg1".into())]),
            false,
        );
        child.define("name".into(), Value::String("bob".into()), false);
        child.define("new_key".into(), Value::Int(42), false);

        let mut strategies: HashMap<String, MergeStrategy> = HashMap::new();
        strategies.insert("counter".into(), MergeStrategy::Add);
        strategies.insert("log".into(), MergeStrategy::Append);
        // "name" not in strategies → uses default (LWW)

        parent.merge_from_with_strategies(&child, &strategies, &MergeStrategy::LastWriteWins);

        // counter: 100 + 5 = 105 (Add strategy)
        assert_eq!(parent.get("counter"), Some(Value::Int(105)));
        // log: [] ++ ["msg1"] = ["msg1"] (Append strategy)
        assert_eq!(
            parent.get("log"),
            Some(Value::List(vec![Value::String("msg1".into())]))
        );
        // name: LWW → child wins (not in strategies map)
        assert_eq!(parent.get("name"), Some(Value::String("bob".into())));
        // new_key: new binding, defined directly
        assert_eq!(parent.get("new_key"), Some(Value::Int(42)));
    }

    // ─── VectorClock tests ───────────────────────────────────────

    #[test]
    fn vector_clock_tick_increments() {
        let mut c = VectorClock::default();
        c.tick("agent-a");
        c.tick("agent-a");
        c.tick("agent-b");
        assert_eq!(c.entries.get("agent-a"), Some(&2));
        assert_eq!(c.entries.get("agent-b"), Some(&1));
    }

    #[test]
    fn vector_clock_merge_takes_max() {
        let mut a = VectorClock::default();
        a.tick("x"); // x=1
        let mut b = VectorClock::default();
        b.tick("y"); // y=1
        b.tick("x");
        b.tick("x"); // x=2
        a.merge(&b);
        assert_eq!(a.entries.get("x"), Some(&2)); // max(1,2)=2
        assert_eq!(a.entries.get("y"), Some(&1)); // max(0,1)=1
    }

    #[test]
    fn vector_clock_happened_before() {
        let mut a = VectorClock::default();
        a.tick("x"); // {x:1}
        let mut b = a.clone();
        b.tick("x"); // {x:2}
        // a happened-before b: a[x]=1 ≤ b[x]=2, and strict
        assert!(VectorClock::happened_before(&a, &b));
        assert!(!VectorClock::happened_before(&b, &a));
    }

    #[test]
    fn vector_clock_concurrent_detection() {
        let mut a = VectorClock::default();
        a.tick("x"); // {x:1}
        let mut b = VectorClock::default();
        b.tick("y"); // {y:1}
        // Neither happened-before the other
        assert!(VectorClock::concurrent(&a, &b));
        assert!(!VectorClock::happened_before(&a, &b));
        assert!(!VectorClock::happened_before(&b, &a));
    }

    #[test]
    fn vector_clock_equal_is_not_concurrent() {
        let mut a = VectorClock::default();
        a.tick("x");
        let b = a.clone();
        // Equal clocks: not concurrent (happened-before requires strict <)
        assert!(!VectorClock::concurrent(&a, &b));
    }

    #[test]
    fn vector_clock_empty_is_not_concurrent() {
        let a = VectorClock::default();
        let mut b = VectorClock::default();
        b.tick("x");
        // Empty clock is trivially ≤ any other clock
        assert!(!VectorClock::concurrent(&a, &b));
    }

    #[test]
    fn vector_clock_to_from_dict_roundtrip() {
        let mut c = VectorClock::default();
        c.tick("agent-a");
        c.tick("agent-a");
        c.tick("agent-b");
        let dict = c.to_dict();
        let restored = VectorClock::from_dict(&dict);
        assert_eq!(c, restored);
        assert_eq!(restored.entries.get("agent-a"), Some(&2));
        assert_eq!(restored.entries.get("agent-b"), Some(&1));
    }

    #[test]
    fn vector_clock_from_dict_handles_empty() {
        let dict: HashMap<String, Value> = HashMap::new();
        let c = VectorClock::from_dict(&dict);
        assert!(c.is_empty());
    }
}
