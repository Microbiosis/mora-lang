//! v0.20: 运行时值/环境/控制流核心类型（自 interpreter.rs 抽出）。
//!
//! **Move-only refactor** — 代码自 src/interpreter.rs **迁移**（非复制）：
//! interpreter.rs 不再持有这些定义的副本，而是通过 `pub use crate::value::*`
//! 重新导出。此处是唯一定义点。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::BufReader;
use std::sync::Arc;

// v0.94: HAMT persistent map —— Environment 的绑定存储（始终启用，非 opt-in）。
pub mod persistent;

// v0.83: Clojure-style transducers — 流式管道的底层原语
pub mod transducer;

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
    // v0.83: tea.* — TEA runtime (init/update/view/run/replay/send)
    Tea,
    // v0.83: xform.* — Clojure-style transducer (map/filter/take/comp)
    Xform,
    // v0.86: eval(code) — runtime eval, Mora 源码动态执行（Lisp homoiconicity + eval-apply）
    Eval,
    // v0.91: math.* — 标量数学（sin/cos/tan/exp/log/pow/sqrt/abs/floor/ceil/round + PI/E/TAU）
    Math,
    // v0.91: stats.* — 统计（sum/mean/median/stddev/var/min/max/histogram/corr/cov）
    Stats,
    // v0.91: linalg.* — 线性代数（dot/cross/norm/matmul/transpose）
    Linalg,
    // v0.91: random.* — PRNG（random/rand_int/rand_float/rand_choice/seed/shuffle）
    Random,
}

/// 全局**模块对象**（`name.method(...)` 形式）的单一事实源。
///
/// 三处消费方共享本表，杜绝各自维护名单造成的漂移：
/// 1. [`Interpreter::new`](crate::interpreter::Interpreter) 把它注册进 globals；
/// 2. [`crate::flow::is_builtin_object`] 判定「非变量绑定的内建对象名」；
/// 3. [`crate::typeck::hm`] 的 `infer_var` 把未绑定模块名解析为对象类型
///    （而非报 `Unbound variable`）。
///
/// **缺陷背景（v0.103）**：此前注册与判定各写一份名单 —— globals 注册了
/// 22 个模块（含 tea/xform），`is_builtin_object` 只列 6 个，`infer_var` 又
/// 手工列 6 个。结果 `bus`/`sandbox`/`schedule`/`ccr`/`mock`/`exec`/`tool`/
/// `skill`/`plan`/`mora`/`document`/`tea`/`xform` 共 13 个模块**运行时可用但
/// 类型检查阶段被拒**（Unbound variable），用户完全无法调用。
///
/// 注意：本表只含模块对象。裸函数（print/range/len/compose_prompt/tail/
/// compress/crush_json）与 `tool`（Toolplane 实例）不在此列 —— 它们不是
/// `name.method` 形式，注册见 `Interpreter::new`。
pub const MODULE_OBJECTS: &[(&str, BuiltinKind)] = &[
    ("ai", BuiltinKind::AiChat),
    ("web", BuiltinKind::Web),
    ("json", BuiltinKind::Json),
    ("file", BuiltinKind::File),
    ("memory", BuiltinKind::Memory),
    ("agent", BuiltinKind::Agent),
    ("document", BuiltinKind::Document),
    ("bus", BuiltinKind::Bus),
    ("sandbox", BuiltinKind::Sandbox),
    ("schedule", BuiltinKind::Schedule),
    ("ccr", BuiltinKind::Ccr),
    ("mock", BuiltinKind::Mock),
    ("exec", BuiltinKind::Exec),
    ("tool", BuiltinKind::Toolplane),
    ("skill", BuiltinKind::Skill),
    ("plan", BuiltinKind::Plan),
    ("mora", BuiltinKind::Mora),
    ("math", BuiltinKind::Math),
    ("stats", BuiltinKind::Stats),
    ("linalg", BuiltinKind::Linalg),
    ("random", BuiltinKind::Random),
    ("tea", BuiltinKind::Tea),
    ("xform", BuiltinKind::Xform),
];

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
            BuiltinKind::Tea => "tea",
            BuiltinKind::Xform => "xform",
            BuiltinKind::Eval => "eval",
            BuiltinKind::Math => "math",
            BuiltinKind::Stats => "stats",
            BuiltinKind::Linalg => "linalg",
            BuiltinKind::Random => "random",
        };
        f.write_str(s)
    }
}

impl BuiltinKind {
    /// v0.75.52: 静态查找表（P6）— 调用名 → BuiltinKind 的单一来源。
    /// 覆盖裸函数（print/len/range）与 domain 前缀（file.*/ai.chat 等）
    /// 的 kind 登记；未登记返回 None（dispatch 走原生 match fallback）。
    ///
    /// v0.103: **模块对象前缀从 [`MODULE_OBJECTS`] 派生**（单一事实源）——
    /// 此前本表另抄一份名单，与 globals 注册漂移：注册名是 `tool` 而本表
    /// 只认 `toolplane`，导致 `from_name("tool")` 返回 None。同时
    /// `BuiltinKind::Toolplane` 的 Display 又写作 `"Toolplane::new"` ——
    /// 同一内建三种拼写。现统一：模块前缀 == MODULE_OBJECTS 的键。
    pub fn from_name(name: &str) -> Option<BuiltinKind> {
        // 裸函数
        match name {
            "print" => return Some(BuiltinKind::Print),
            "range" => return Some(BuiltinKind::Range),
            "len" => return Some(BuiltinKind::Len),
            "eval" => return Some(BuiltinKind::Eval),
            _ => {}
        }
        // domain 前缀
        let prefix = name.split('.').next().unwrap_or(name);
        // ai 有子域细分（chat → AiChat / tokens → AiTokens / 其余 → Ai），
        // 先于 MODULE_OBJECTS 统一映射处理。
        if prefix == "ai" {
            return Some(if name.starts_with("ai.chat") {
                BuiltinKind::AiChat
            } else if name.starts_with("ai.tokens") {
                BuiltinKind::AiTokens
            } else {
                BuiltinKind::Ai
            });
        }
        // 模块对象：唯一名单（与 globals 注册同源）
        if let Some((_, kind)) = MODULE_OBJECTS.iter().find(|(n, _)| *n == prefix) {
            return Some(*kind);
        }
        // 非模块 builtin（裸函数形态的域前缀）
        Some(match prefix {
            "compress" => BuiltinKind::Compress,
            "crush_json" => BuiltinKind::CrushJson,
            "tail" => BuiltinKind::Tail,
            "compose_prompt" => BuiltinKind::ComposePrompt,
            _ => return None,
        })
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
}

#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    /// v0.x: 单字符（`string[number]` 索引结果）
    Char(char),
    // v0.38: Numeric tower — distinct Int and Float variants.
    Int(i64),
    Float(f64),
    /// v0.91: arbitrary-precision 整数（num-bigint 后端）。
    /// 字面量语法：`<digits>n`（如 `123n`、`99999999999999999999999n`）。
    /// 算术 promotion：任一含 BigInt 时结果为 BigInt（最小惊讶）。
    BigInt(num_bigint::BigInt),
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
        /// EnvRef::new(env) for closure captures.
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
        /// v0.83: transducer pipeline applied to each token.
        /// None = identity (current behavior, fully backward-compatible).
        /// Clojure-style push-based transducer：每个 SSE token 推入 `step`，
        /// 输出 None 表示终止流。
        /// 包装在 Arc 中以支持 Value::Clone（多消费者共享同一 transducer）。
        xform: Option<Arc<dyn transducer::Transducer<String, String>>>,
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
    // v0.20 / v0.83: 宏定义 (Common Lisp 启发) — 宏体作为 MIR 函数存储。
    // body 是宏的 MIR 编译体（由 parser_v3 emit_macro_def_w 经子 EmitContext
    // 编译而来）。宏调用时以 call_args 绑定 params，在子 env 中 run_mir 执行 body。
    // Arc 而非 Rc 以保留 Value: Send + Sync（与 Value::Closure 同模式）。
    Macro {
        name: String,
        params: Vec<String>,
        body: std::sync::Arc<crate::mir::MirFunction>,
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
    // v0.86: Curry — 函数柯里化（Lisp/Rust closure 启发）。
    // curry(fn, arity) 产生 Curry；调用时若已绑定参数不足 arity，返回新的 Curry
    // （累积参数）；足够则调用内部 fn。这是 compose/partial 的补完：
    // compose 是串行 f∘g∘h，partial 是前向绑定 f(x, ?)，curry 是
    // 按序分批接收参数（Clojure partial vs. Haskell curry 的区别）。
    Curry {
        func: Box<Value>,
        arity: usize,
        bound_args: Vec<Value>,
    },
    // v0.86: Cons — Lisp 链式列表原语。Value::Cons(car, cdr) 是
    // Lisp 'cons cell' 的直译；car 是头元素，cdr 是尾部（可嵌套
    // Cons 形成链表，或 nil 终止）。配合 car()/cdr()/cons() 内置函数
    // 提供对链表的不可变操作。cdr 为 Nil 时表示单元素列表。
    Cons {
        car: Box<Value>,
        cdr: Box<Value>,
    },
    /// v0.86: Lisp homoiconicity — `quote(expr)` captures `expr` as
    /// a source-text string wrapped in `Code`. Completes the
    /// eval-apply-quote triad: `eval(s)` compiles+runs; `quote(s)`
    /// freezes it. The stored string is the raw substring inside
    /// `quote(...)`, preserving exact user typing.
    ///
    /// Round-trip invariant: `eval(quote(expr)) == expr`.
    Code(String),
    // v0.83: TEA (The Elm Architecture) — Model/Msg/Update/Cmd 完整架构。
    //
    // TeaApp = 不可变 TEA runtime 值：Model + init/update/view 闭包 + msg_queue + cmd_queue。
    // v0.94: TeaApp 本身是纯数据（无内部 Mutex）；Arc 仅用于廉价共享同一快照。
    TeaApp(std::sync::Arc<crate::tea::TeaApp>),
    // v0.83: TEA Cmd — 描述 update 函数想触发的副作用。
    // 值化后可在 builtin / record / replay 间无缝传递。
    TeaCmd(crate::tea::Cmd),
    // v0.83: TEA Msg — 触发 update 的输入事件（tagged union：tag + payload）。
    TeaMsg(crate::tea::Msg),
    // v0.102: 声明式范式（逻辑式/关系式）— 关系值（一等值）。
    // clauses 为该关系的全部子句（事实+规则）；同名 rel 定义经 h_rel_def 累积。
    Relation {
        name: String,
        clauses: std::sync::Arc<Vec<crate::rel::Clause>>,
    },
    // v0.102: 目标值 — 一等值，可组合（both/either）、可高阶传递。
    // 调用关系值（`edge(?x, ?y)`）即构造目标；solve 消费目标执行搜索。
    // Box 打断 Value ↔ Goal/Term 的递归环。
    Goal(Box<crate::rel::Goal>),
    // v0.102: 逻辑变量 — 搜索期叶子。solve 查询变量由 h_solve 分配 id；
    // 解在 solve 边界 reify 为 `_.N` 符号，逻辑变量从不逃逸到普通值空间。
    LogicVar(u64),
}

// 手动实现 PartialEq（EnvRef 不支持自动派生）
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            // v0.91: BigInt 相等
            (Value::BigInt(a), Value::BigInt(b)) => a == b,
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
            // v0.94: TEA 变体是纯数据 —— 结构相等（此前因内部 Mutex 只能按指针比较）。
            (Value::TeaApp(a), Value::TeaApp(b)) => a == b,
            (Value::TeaCmd(a), Value::TeaCmd(b)) => a == b,
            (Value::TeaMsg(a), Value::TeaMsg(b)) => a == b,
            // v0.86: Curry — 按函数/arity/已绑定参数逐一比较。
            (Value::Curry { func: a, arity: aa, bound_args: ba },
             Value::Curry { func: b, arity: ab, bound_args: bb }) => {
                a == b && aa == ab && ba == bb
            }
            // v0.86: Cons — 按 car/cdr 结构比较。
            (Value::Cons { car: a1, cdr: a2 }, Value::Cons { car: b1, cdr: b2 }) => a1 == b1 && a2 == b2,
            // v0.86: Code — 按 source text 字符串比较。
            (Value::Code(a), Value::Code(b)) => a == b,
            // v0.102: 逻辑变量 — 按 id 相等（subst 测试/引擎内比较用）。
            // Relation/Goal 无结构相等语义（落入 `_ => false`）。
            (Value::LogicVar(a), Value::LogicVar(b)) => a == b,
            _ => false,
        }
    }
}

mod display; // v0.75.62: Value Display + fmt_inner（纯格式化，自 value.rs 拆出）

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
    /// 返回值可用的方法名列表（`methods_of(value)` builtin 用）。
    /// v0.75.47: 从 flow.rs::get_methods_for_value 下沉 —— 内禀属性贴近
    /// 数据定义（Lua 5.4 元表同源思想），dispatch 不再跨模块查方法表。
    pub fn methods(&self) -> Vec<String> {
        let names: &[&str] = match self {
            Value::String(_) => &[
                "len",
                "upper",
                "lower",
                "trim",
                "starts_with",
                "ends_with",
                "contains",
                "split",
                "replace",
                "json",
            ],
            Value::List(_) => &[
                "push",
                "pop",
                "get",
                "len",
                "map",
                "filter",
                "reduce",
                "take",
                "drop",
                "window",
                "batch",
                "shape",
                "flatten",
                "transpose",
                "reshape",
                // v0.91: 列表统计方法（复用 stats.* 函数）
                "sum",
                "min",
                "max",
                "mean",
                "median",
                "stddev",
                "var",
                "sort",
            ],
            Value::Dict(_) => &["get", "set", "keys", "values", "len", "json"],
            Value::Int(_) => &[
                // v0.91: 整数数学方法
                "abs", "sign", "floor", "ceil", "round", "sqrt",
                "sin", "cos", "tan", "exp", "log", "log2", "log10",
                "to_float",
            ],
            Value::Float(_) => &[
                // v0.91: 浮点数学方法
                "abs", "sign", "floor", "ceil", "round", "trunc", "fract",
                "sqrt", "cbrt",
                "sin", "cos", "tan", "asin", "acos", "atan",
                "sinh", "cosh", "tanh",
                "exp", "log", "log2", "log10", "log1p",
                "is_nan", "is_inf", "is_finite",
                "to_int",
            ],
            Value::BigInt(_) => &[
                // v0.91: BigInt 数学方法
                "abs", "sign",
                "to_int", "to_float",
            ],
            Value::Conversation { .. } => &["chat", "history", "clear", "model", "len"],
            Value::Stream { .. } => &["collect", "is_done"],
            Value::Router { .. } => &["route", "listen"],
            Value::McpServer { .. } => &["tool", "serve"],
            Value::Agent { .. } => &["run", "name", "max_steps"],
            _ => &[],
        };
        names.iter().map(|s| s.to_string()).collect()
    }

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
                (Value::BigInt(a), Value::BigInt(b)) => Value::BigInt(a + b),
                (Value::Int(a), Value::BigInt(b)) => {
                    Value::BigInt(num_bigint::BigInt::from(a) + b)
                }
                (Value::BigInt(a), Value::Int(b)) => {
                    Value::BigInt(a + num_bigint::BigInt::from(b))
                }
                (Value::Float(a), Value::BigInt(b)) => {
                    // Float + BigInt → Float（如果 b 能转 f64，否则保留 BigInt）
                    b.to_string().parse::<f64>().map_or_else(
                        |_| Value::BigInt(num_bigint::BigInt::from(a as i64) + b),
                        |bf| Value::Float(a + bf),
                    )
                }
                (Value::BigInt(a), Value::Float(b)) => {
                    a.to_string().parse::<f64>().map_or_else(
                        |_| Value::BigInt(a + num_bigint::BigInt::from(b as i64)),
                        |af| Value::Float(af + b),
                    )
                }
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
/// 词法环境 —— **不可变绑定存储**（v0.94 数据流化）。
///
/// 与过去的 `HashMap<String, Arc<Mutex<Value>>>` + `Arc<Mutex<Environment>>` 父链
/// 不同，这里的绑定表是持久化 HAMT（[`persistent::PersistentMap`]）：
/// - 绑定值是纯 `Value`，无每绑定 `Mutex` —— 捕获的闭包无法再被外部改写。
/// - `parent` 是 `Arc<Environment>`（不可变），父链共享无需加锁。
/// - `assoc` 返回新版本（结构共享），因此 [`Environment::snapshot`] 是 O(1) 克隆。
///
/// `define`/`assign` 仍是 `&mut self` 以兼容解释器按 `&mut Environment` 穿线的
/// 调用面，但其内部以持久 map 产生新版本，不再有内部可变性。
#[derive(Debug, Clone)]
pub struct Environment {
    pub values: persistent::PersistentMap<Value>,
    pub parent: Option<Arc<Environment>>,
    /// v0.61: Per-binding version clocks (which agent modified each key).
    pub versions: HashMap<String, VectorClock>,
    /// v0.61: This environment's own vector clock.
    pub clock: VectorClock,
    /// v0.103: 本层**对外公开**的绑定名（模块导出面）。
    ///
    /// 语义（spec §10.2）：`export` 标记的符号对 import 者可见，未标记的
    /// 是模块私有。本集合是该模块接口的单一存储 —— 两条入口写同一存储：
    /// 1. [`Environment::define`] 的 `exported` 参数（此前是死参数，被忽略）；
    /// 2. `MirInst::ExportMark` 指令（`export <decl>` 的运行时落点）。
    ///
    /// `import` 只把**导出集内**的名字合并进导入方环境（typeck 侧由
    /// witness 静态收集同名集合，两侧同源）。
    pub exports: std::collections::HashSet<String>,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    pub fn new() -> Self {
        Self {
            values: persistent::PersistentMap::new(),
            parent: None,
            versions: HashMap::new(),
            clock: VectorClock::default(),
            exports: std::collections::HashSet::new(),
        }
    }

    pub fn with_parent_of(parent: Arc<Environment>) -> Self {
        Self {
            values: persistent::PersistentMap::new(),
            parent: Some(parent),
            versions: HashMap::new(),
            clock: VectorClock::default(),
            exports: std::collections::HashSet::new(),
        }
    }

    /// O(1) 快照：持久 map 结构共享，父链 Arc 递增。并发捕获的底层原语。
    pub fn snapshot(&self) -> Environment {
        self.clone()
    }

    /// 纯函数式定义：返回带新绑定的新环境，`self` 保持不变。
    /// 这是「用数据流代替状态机」的环境原语 —— 调用方持有新版本即可。
    pub fn assoc(&self, name: &str, value: Value) -> Environment {
        let mut next = self.clone();
        next.values = next.values.assoc(name, value);
        next.versions.insert(name.to_string(), self.clock.clone());
        next
    }

    /// 定义绑定。`exported=true` 同时把名字记入本层的**导出集**
    /// （模块对外接口，见 [`Environment::exports`]）。
    ///
    /// v0.103 修复：此前 `exported` 形参被忽略（写作 `_exported`）——
    /// 模块可见性因此无从表达（spec §10.2 的 `export` 承诺零实现）。
    pub fn define(&mut self, name: String, value: Value, exported: bool) {
        if exported {
            self.exports.insert(name.clone());
        }
        self.values = self.values.assoc(&name, value);
        // v0.61: record the current clock for this binding
        self.versions.insert(name, self.clock.clone());
    }

    /// v0.103: 把名字标记为对外公开（`MirInst::ExportMark` 的落点）。
    /// 与 `define(name, value, true)` 写同一存储，只是允许在绑定之后标记
    /// （parser 需先解析完整声明才知道名字，故用独立的标记指令）。
    pub fn mark_exported(&mut self, name: &str) {
        self.exports.insert(name.to_string());
    }

    /// 名字是否在本层导出（不看父链 —— 导出面属模块自身）。
    pub fn is_exported(&self, name: &str) -> bool {
        self.exports.contains(name)
    }

    /// 本层导出名（排序后稳定返回，供 import 合并与测试断言）。
    pub fn exported_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.exports.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.values.get(name) {
            Some(v.clone())
        } else if let Some(parent) = &self.parent {
            parent.get(name)
        } else {
            None
        }
    }

    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.values.contains_key(name) {
            self.values = self.values.assoc(name, value);
            // v0.61: update the version clock for this binding
            self.versions.insert(name.to_string(), self.clock.clone());
            true
        } else if let Some(parent) = &mut self.parent {
            // 父链是 Arc<Environment>：写入触发写时复制，共享的父版本不被就地改写。
            let result = Arc::make_mut(parent).assign(name, value);
            // v0.64: also update local clock for parent-scope writes.
            if result {
                self.versions.insert(name.to_string(), self.clock.clone());
            }
            result
        } else {
            false
        }
    }

    /// 迭代环境中的所有绑定（仅当前层，不含 parent），供 import/子 env 合并用。
    pub fn iter(&self) -> Vec<(String, Value)> {
        self.values
            .iter()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
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
            match self.values.get(&name).cloned() {
                Some(parent_val) => {
                    let merged = Value::merge(parent_val, child_val, strategy);
                    self.values = self.values.assoc(&name, merged);
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
            match self.values.get(&name).cloned() {
                Some(parent_val) => {
                    let parent_clock = self.versions.get(&name).cloned().unwrap_or_default();
                    let child_clock = child.versions.get(&name).cloned().unwrap_or_default();

                    // Detect concurrent modifications
                    if !parent_clock.is_empty()
                        && !child_clock.is_empty()
                        && VectorClock::concurrent(&parent_clock, &child_clock)
                    {
                        conflicts.push(Conflict {
                            key: name.clone(),
                            parent_value: parent_val.clone(),
                            child_value: child_val.clone(),
                            parent_clock: parent_clock.clone(),
                            child_clock: child_clock.clone(),
                        });
                    }

                    let merged = Value::merge(parent_val, child_val, strategy);
                    self.values = self.values.assoc(&name, merged);
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
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
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
    fn env_clone_is_isolated_not_aliased() {
        // 旧实现用 HashMap<String, Arc<Mutex<Value>>>：clone 共享绑定 cell，
        // 一处写会穿透另一处。持久化绑定后 clone 是独立版本。
        let mut base = Environment::new();
        base.define("x".into(), Value::Int(1), false);
        let mut copy = base.clone();
        copy.define("x".into(), Value::Int(2), false);
        assert_eq!(copy.get("x"), Some(Value::Int(2)));
        assert_eq!(base.get("x"), Some(Value::Int(1)), "clone 不得被别名写穿");
    }

    #[test]
    fn env_snapshot_isolated_from_further_writes() {
        let mut base = Environment::new();
        base.define("x".into(), Value::Int(1), false);
        let snap = base.snapshot();
        base.define("x".into(), Value::Int(9), false);
        base.define("y".into(), Value::Int(7), false);
        assert_eq!(snap.get("x"), Some(Value::Int(1)), "快照冻结在捕获时刻");
        assert_eq!(snap.get("y"), None, "后续定义不进入旧快照");
        assert_eq!(base.get("x"), Some(Value::Int(9)));
    }

    #[test]
    fn env_assoc_is_pure() {
        let base = Environment::new();
        let with_a = base.assoc("a", Value::Int(1));
        let with_b = with_a.assoc("b", Value::Int(2));
        assert_eq!(base.get("a"), None, "assoc 不改变原环境");
        assert_eq!(with_a.get("a"), Some(Value::Int(1)));
        assert_eq!(with_a.get("b"), None);
        assert_eq!(with_b.get("a"), Some(Value::Int(1)), "结构共享保留旧键");
        assert_eq!(with_b.get("b"), Some(Value::Int(2)));
    }

    #[test]
    fn env_parent_scope_assign_copies_on_write() {
        let mut p = Environment::new();
        p.define("a".into(), Value::Int(1), false);
        let parent = Arc::new(p);
        let mut child = Environment::with_parent_of(Arc::clone(&parent));
        assert!(child.assign("a", Value::Int(99)), "写入父作用域已有绑定");
        assert_eq!(child.get("a"), Some(Value::Int(99)));
        assert_eq!(
            parent.get("a"),
            Some(Value::Int(1)),
            "父链是不可变的：写时复制，共享的父版本不被就地改写"
        );
    }

    #[test]
    fn environment_is_send_sync() {
        // 纯函数式数据（无内部可变性）天然满足 Send + Sync —— 并发不再依赖锁。
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Environment>();
    }

    #[test]
    fn env_shared_across_threads_without_locks() {
        let mut base = Environment::new();
        base.define("answer".into(), Value::Int(42), false);
        let shared = Arc::new(base);
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let env = Arc::clone(&shared);
                std::thread::spawn(move || {
                    // 纯读：无锁、无 &mut，仅结构共享。
                    (i, env.get("answer").expect("binding present"))
                })
            })
            .collect();
        for h in handles {
            let (_, v) = h.join().expect("thread join");
            assert_eq!(v, Value::Int(42));
        }
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
