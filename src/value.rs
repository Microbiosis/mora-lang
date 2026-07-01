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
#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    /// v0.x: 单字符（`string[number]` 索引结果）
    Char(char),
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
        env: Arc<Mutex<Environment>>,
        /// v2 模式: 闭包表达式在 arena 中的 NodeId
        v2_node_id: Option<usize>,
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

// 手动实现 PartialEq（Arc<Mutex<Environment>> 不支持自动派生）
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (
                Value::PromptSection { name: a, role: ra, text: ta, budget_bytes: ba },
                Value::PromptSection { name: b, role: rb, text: tb, budget_bytes: bb },
            ) => a == b && ra == rb && ta == tb && ba == bb,
            (
                Value::Document { metadata: a, .. },
                Value::Document { metadata: b, .. },
            ) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "{}", s),
            Value::Char(c) => write!(f, "{}", c),
            Value::Number(n) => write!(f, "{}", n),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Nil => write!(f, "nil"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Dict(map) => {
                let parts: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
                write!(f, "{{{}}}", parts.join(", "))
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
                write!(
                    f,
                    "<router ({} routes)>",
                    routes.lock().expect("Router routes mutex poisoned").len()
                )
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
                write!(f, "<atom {:?}>", arc.lock().expect("Atom mutex poisoned"))
            }
            Value::Macro { name, .. } => {
                write!(f, "<macro {}>", name)
            }
            Value::PromptSection { name, role, budget_bytes, .. } => {
                write!(
                    f,
                    "<prompt_section name={} role={:?} budget={:?}>",
                    name, role, budget_bytes
                )
            }
            Value::Document { backend, metadata } => write!(
                f,
                "<document origin={} meta_keys={}>",
                backend.origin(),
                metadata.len()
            ),
        }
    }
}

// ─── Environment ─────────────────────────────────────────
#[derive(Debug)]
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

    pub fn with_parent(parent: Arc<Mutex<Environment>>) -> Self {
        Self {
            values: HashMap::new(),
            exports: HashMap::new(),
            parent: Some(parent),
        }
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
