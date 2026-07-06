//! v0.20: 从 interpreter.rs 抽离的运行时值/环境/控制流核心类型。
//!
//! **Move-only refactor** — code copied verbatim from src/interpreter.rs
//! No signature changes, no field changes, no visibility changes.
//! Re-exported in interpreter.rs via `pub use crate::value::*;`

use std::collections::HashMap;
use std::io::BufReader;
use std::sync::{Arc, Mutex};

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
    ) -> std::sync::MutexGuard<'_, BufReader<Box<dyn std::io::Read + Send + Sync>>> {
        self.0
            .lock()
            .expect("StreamReader mutex poisoned: cannot acquire read lock")
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
    pub fn from_arc_mutex(parent: std::sync::Arc<std::sync::Mutex<Environment>>) -> Self {
        let env_clone = parent.lock().expect("env mutex poisoned").clone();
        EnvRef(Box::new(env_clone))
    }
}

#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    /// v0.x: 单字符（`string[number]` 索引结果）
    Char(char),
    // v0.38: Numeric tower — distinct Int/Float variants for type safety.
    // `Number(f64)` is kept as a legacy alias (default for unsuffixed literals).
    Int(i64),
    Float(f64),
    Number(f64),
    Bool(bool),
    Nil,
    List(Vec<Value>),
    Dict(HashMap<String, Value>),
    Task {
        name: String,
        params: Vec<String>,
        /// v2 body: 存储 arena 中的 NodeId 索引
        v2_body_ids: Vec<usize>,
    },
    Closure {
        params: Vec<String>,
        /// v0.40: env is now EnvRef (Local Rc<RefCell> or Owned Box<Environment>)
        /// instead of Arc<Mutex<Environment>>. Callers convert via
        /// EnvRef::from_arc_mutex(arc) for legacy Arc<Mutex<>> sources.
        env: EnvRef,
        /// v2 模式: 闭包表达式在 arena 中的 NodeId
        v2_node_id: Option<usize>,
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
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
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
            Value::Float(x) => write!(f, "{}", x),
            Value::Number(n) => {
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
                // v0.35: Display must be infallible; poisoned mutex used to panic.
                let route_count = routes.lock().map(|m| m.len()).unwrap_or(0);
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
                // v0.35: Display must be infallible; poisoned mutex used to panic.
                match arc.lock() {
                    Ok(v) => write!(f, "<atom {:?}>", v),
                    Err(_) => write!(f, "<atom (lock failed)>"),
                }
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
        Value::Atom(arc) => match arc.lock() {
            Ok(inner) => write!(f, "<atom {:?}>", inner),
            Err(_) => f.write_str("<atom (lock failed)>"),
        },
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

// ─── Environment ─────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct Environment {
    pub values: HashMap<String, Value>,
    pub exports: HashMap<String, Value>,
    pub parent: Option<Arc<Mutex<Environment>>>,
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
        }
    }

    pub fn with_parent_of(parent: Arc<Mutex<Environment>>) -> Self {
        Self {
            values: HashMap::new(),
            exports: HashMap::new(),
            parent: Some(parent),
        }
    }

    /// v0.40: accept Rc<RefCell<>> for the new env model. Converts
    /// to Arc<Mutex<>> internally for now (C1 shim, removed in C4).
    pub fn with_parent_of_rc(parent: std::rc::Rc<std::cell::RefCell<Environment>>) -> Self {
        Self::with_parent_of(std::sync::Arc::new(std::sync::Mutex::new(
            parent.borrow().clone(),
        )))
    }

    pub fn define(&mut self, name: String, value: Value, exported: bool) {
        self.values.insert(name.clone(), value.clone());
        if exported {
            self.exports.insert(name, value);
        }
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(value) = self.values.get(name) {
            Some(value.clone())
        } else if let Some(parent) = &self.parent {
            parent
                .lock()
                .expect("parent environment mutex poisoned")
                .get(name)
        } else {
            None
        }
    }

    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.values.contains_key(name) {
            self.values.insert(name.to_string(), value);
            true
        } else if let Some(parent) = &self.parent {
            parent
                .lock()
                .expect("parent environment mutex poisoned")
                .assign(name, value)
        } else {
            false
        }
    }

    // v0.21: 所有权语义支持

    /// 获取绑定状态
    pub fn get_binding(&self, name: &str) -> Option<Binding> {
        if let Some(value) = self.values.get(name) {
            Some(Binding::Value(value.clone()))
        } else if let Some(parent) = &self.parent {
            parent
                .lock()
                .expect("parent environment mutex poisoned")
                .get_binding(name)
        } else {
            None
        }
    }

    /// 移动变量（所有权转移）
    pub fn move_variable(&mut self, name: &str) -> Result<Value, String> {
        if let Some(value) = self.values.remove(name) {
            Ok(value)
        } else if let Some(parent) = &self.parent {
            parent
                .lock()
                .expect("parent environment mutex poisoned")
                .move_variable(name)
        } else {
            Err(format!("undefined variable: {}", name))
        }
    }

    /// 借用变量（不可变）
    pub fn borrow_variable(&self, name: &str) -> Result<Arc<Mutex<Value>>, String> {
        if let Some(value) = self.values.get(name) {
            Ok(Arc::new(Mutex::new(value.clone())))
        } else if let Some(parent) = &self.parent {
            parent
                .lock()
                .expect("parent environment mutex poisoned")
                .borrow_variable(name)
        } else {
            Err(format!("undefined variable: {}", name))
        }
    }

    /// 可变借用变量
    pub fn borrow_variable_mut(&mut self, name: &str) -> Result<Arc<Mutex<Value>>, String> {
        if let Some(value) = self.values.get(name) {
            Ok(Arc::new(Mutex::new(value.clone())))
        } else if let Some(parent) = &self.parent {
            parent
                .lock()
                .expect("parent environment mutex poisoned")
                .borrow_variable_mut(name)
        } else {
            Err(format!("undefined variable: {}", name))
        }
    }
}

// ─── FlowSignal ──────────────────────────────────────────
/// 控制流信号
#[derive(Debug)]
pub enum FlowSignal {
    None,
    Return(Value),
    Break,
    Continue,
}

impl FlowSignal {
    pub fn into_value(self) -> Value {
        match self {
            FlowSignal::None => Value::Nil,
            FlowSignal::Return(v) => v,
            FlowSignal::Break => Value::Nil,
            FlowSignal::Continue => Value::Nil,
        }
    }

    pub fn is_return(&self) -> bool {
        matches!(self, FlowSignal::Return(_))
    }
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
        let v = Value::Atom(Arc::new(Mutex::new(Value::Number(42.0))));
        let s = format!("{}", v);
        assert!(s.contains("atom"), "got: {}", s);
        assert!(s.contains("42"), "got: {}", s);
    }

    /// v0.36 (P1-3.13): Number Display should render NaN/Inf without panicking.
    #[test]
    fn number_display_handles_nan() {
        let v = Value::Number(f64::NAN);
        let s = format!("{}", v);
        assert_eq!(s, "nan");
    }

    #[test]
    fn number_display_handles_pos_inf() {
        let v = Value::Number(f64::INFINITY);
        let s = format!("{}", v);
        assert_eq!(s, "inf");
    }

    #[test]
    fn number_display_handles_neg_inf() {
        let v = Value::Number(f64::NEG_INFINITY);
        let s = format!("{}", v);
        assert_eq!(s, "-inf");
    }

    #[test]
    fn number_display_normal_value() {
        let v = Value::Number(42.5);
        let s = format!("{}", v);
        assert_eq!(s, "42.5");
    }

    /// v0.40: EnvRef smoke test.
    #[test]
    fn envref_from_arc_mutex_roundtrip() {
        let mut e = Environment::new();
        e.define("x".to_string(), Value::String("y".to_string()), false);
        let arc = std::sync::Arc::new(std::sync::Mutex::new(e));
        let r = EnvRef::from_arc_mutex(arc);
        assert_eq!(r.env().get("x"), Some(Value::String("y".to_string())));
    }
}
