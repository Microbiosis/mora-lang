//! v11 静态类型检查
//!
//! 设计原则：
//! - **多错误收集**：一次跑完所有检查，统一报告（不首个错误终止）
//! - **位置精确**：每条 TypeError 带行号（line, col），IDE 友好
//! - **可选类型**：Mora 是动态语言，无 hint 时走推断；推断不出来视为 Any
//! - **不破坏现有行为**：未标注类型的代码继续动态执行（仅在 main.rs 入口可选启用 typeck）
//!
//! 检查范围：
//! - let 初始化值 vs 类型 hint
//! - task / closure 参数 vs 实参类型
//! - task / closure 返回类型 vs return 表达式
//! - binary 操作数类型（+ - * / % + 比较）
//! - 索引操作类型（list→number, dict→string）
//! - if 条件类型（任何值视为 truthy，不报）
//! - method call 接收者类型 + 方法存在性
//! - 变量引用 vs 作用域
//!
//! 不做：
//! - 跨模块 import 的 symbol table（import 解析时类型仍为 Any）
//! - 列表/字典元素类型推断（Mora 列表是异构容器）
//! - generic / union 类型
//! - 控制流敏感的类型缩窄

// v0.55: typeck V2 模块 (mod check, mod pregel_check) 已删除。
// 类型检查的唯一入口: `check_program_mir` (mod check_mir)。

use std::collections::{HashMap, HashSet};

// v1 AST types no longer imported — all v2 paths use ast_v2 / common
use crate::common::{BinaryOp, Span};

// ===================================================================
// 公共类型
// ===================================================================

/// Mora 类型系统：基础类型 + Any（推断不出时退路）
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    String,
    /// v0.x: 单字符类型（`string[number]` 索引结果）
    Char,
    // v0.38: numeric tower — Int(i64) and Float(f64) as distinct types.
    Int,
    Float,
    Bool,
    Nil,
    /// 任意类型（推断不出时的退路，或显式 `any` 标注）
    Any,
    /// v0.x: 列表类型携带元素类型（`list<T>`）
    List(Box<Type>),
    /// v0.x: 字典类型携带键值类型（`dict<K, V>`）
    Dict(Box<Type>, Box<Type>),
    Task,
    Closure,
    Conversation,
    Stream,
    Builtin,
    /// v0.06: AI 配置类型（ai.chat 的接收者 / AiConfig::new() 构造）
    AiConfig,
    /// v0.06: AI 调用结果类型（ai.chat 的成功返回）
    AiResult,
    /// v0.06: AI 调用错误类型（ai.chat 的失败返回，v0.06.2 起被 Result<T,E> 包裹）
    AiError,
    /// v0.06: AI 模块类型（`ai` 内建变量的接收者类型）
    AiModule,
    /// v0.06.2: 类型化错误处理 Result<T, E>
    Result_(Box<Type>, Box<Type>),
    /// v0.06.3: HTTP 路由构建器
    Router,
    /// v0.06.3: HTTP 请求对象
    HttpRequest,
    /// v0.06.3: HTTP 响应对象（handler 返回值）
    HttpResponse,
    /// v0.06.6: MCP 服务器构建器
    McpServer,
    /// v0.08: dyn trait 类型（名称）
    /// v0.09: 携带泛型参数列表（如 `dyn Container<number>`）
    Trait {
        name: String,
        generics: Vec<Type>,
    },
    /// v0.09: 具体类型（替代 v0.08.5 删的 Type::Struct）
    ///   携带泛型参数 + 实现的 trait 列表
    Concrete {
        name: String,
        generics: Vec<Type>,
        traits: Vec<Type>,
    },
    /// v0.13: Union 类型（多种类型的合集，e.g. `string | number | bool`）
    ///   用于 builtin 多类型签名（print 等）
    ///   兼容规则: A 兼容 B 当 A 是 B 的成员, 或 B 是 A 的成员, 或递归嵌套
    Union(Vec<Type>),
    // v0.36 (Permanent #3): 8 new Type variants for v0.17–v0.27 Value kinds.
    // The v0.34 audit's claim that "16 Value variants lack Type variants" was
    // solvable in one commit; the previous deferral to v1.0 was a cop-out.
    /// v0.03: Agent (name + tool_names + model_route + max_steps + system)
    Agent,
    /// v0.08.5: Trait object carrier (for_type + trait_name + generics + data)
    TraitObject,
    /// v0.17: Compose pipeline (arity = number of functions)
    Compose,
    /// v0.18: Partial application (boxed origin + how many args applied)
    Partial,
    /// v0.19: Atom (mutable reference cell)
    Atom,
    /// v0.20: Macro definition (name + params shape)
    Macro,
    /// v0.26: Prompt section (named system-prompt segment)
    PromptSection,
    /// v0.27: Document unified IR (Arc<dyn DocumentBackend>)
    Document,
} // ← close pub enum Type

impl Type {
    /// 返回类型的字符串表示。v0.x 起支持泛型：`list<number>` / `dict<string, any>` / `result<T, E>`
    pub fn name(&self) -> String {
        match self {
            Type::String => "string".to_string(),
            Type::Char => "char".to_string(),
            // v0.38: Int and Float distinct.
            Type::Int => "int".to_string(),
            Type::Float => "float".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Nil => "nil".to_string(),
            Type::Any => "any".to_string(),
            Type::List(elem) => format!("list<{}>", elem.name()),
            Type::Dict(k, v) => format!("dict<{}, {}>", k.name(), v.name()),
            Type::Task => "task".to_string(),
            Type::Closure => "closure".to_string(),
            Type::Conversation => "conversation".to_string(),
            Type::Stream => "stream".to_string(),
            Type::Builtin => "builtin".to_string(),
            Type::AiConfig => "ai_config".to_string(),
            Type::AiResult => "ai_result".to_string(),
            Type::AiError => "ai_error".to_string(),
            Type::AiModule => "ai".to_string(),
            Type::Result_(ok, err) => format!("result<{}, {}>", ok.name(), err.name()),
            Type::Router => "router".to_string(),
            Type::HttpRequest => "http_request".to_string(),
            Type::HttpResponse => "http_response".to_string(),
            Type::McpServer => "mcp_server".to_string(),
            Type::Trait { .. } => "trait".to_string(),
            Type::Concrete { .. } => "concrete".to_string(),
            // v0.36: 8 new variants
            Type::Agent => "agent".to_string(),
            Type::TraitObject => "trait_object".to_string(),
            Type::Compose => "compose".to_string(),
            Type::Partial => "partial".to_string(),
            Type::Atom => "atom".to_string(),
            Type::Macro => "macro".to_string(),
            Type::PromptSection => "prompt_section".to_string(),
            Type::Document => "document".to_string(),
            // v0.13: Union 类型显示为 "T1 | T2 | T3"
            Type::Union(members) => {
                if members.is_empty() {
                    return "any".to_string();
                }
                let parts: Vec<String> = members.iter().map(|m| m.name()).collect();
                parts.join(" | ")
            }
        }
    }

    /// 从用户写的类型 hint 字符串解析
    pub fn from_hint(hint: &str) -> Type {
        match hint {
            "string" => Type::String,
            "char" => Type::Char,
            "float" | "number" => Type::Float, // v0.x: "number" kept for backwards compatibility
            "bool" => Type::Bool,
            "nil" => Type::Nil,
            // v0.50: "any" 应解析为 Union(vec![])（兼容任何类型）
            "any" => Type::Union(vec![]),
            "list" => Type::List(Box::new(Type::Union(vec![]))),
            "dict" => Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
            "task" => Type::Task,
            "closure" => Type::Closure,
            "conversation" => Type::Conversation,
            "stream" => Type::Stream,
            "ai_config" => Type::AiConfig,
            "ai_result" => Type::AiResult,
            "ai_error" => Type::AiError,
            "router" => Type::Router,
            "http_request" => Type::HttpRequest,
            "http_response" => Type::HttpResponse,
            "mcp_server" => Type::McpServer,
            // v0.x: list<T> 泛型语法
            s if s.starts_with("list<") && s.ends_with('>') => {
                let inner = &s[5..s.len() - 1];
                Type::List(Box::new(Type::from_hint(inner.trim())))
            }
            // v0.x: dict<K, V> 泛型语法（顶层 split，保留嵌套）
            s if s.starts_with("dict<") && s.ends_with('>') => {
                let inner = &s[5..s.len() - 1];
                match split_top_level_comma(inner) {
                    Some((k_str, v_str)) => Type::Dict(
                        Box::new(Type::from_hint(k_str.trim())),
                        Box::new(Type::from_hint(v_str.trim())),
                    ),
                    None => {
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![])))
                    }
                }
            }
            // v0.x: string<char> 单字符
            "string<char>" => Type::Char,
            // v0.08: dyn: 前缀 → Trait 类型
            // v0.09: dyn:Foo<number> → Trait { name: "Foo", generics: [Number] }
            // v0.10: 泛型嵌套如 Boxed<number> → Type::Trait { name: "Boxed", generics: [Number] }
            s if s.starts_with("dyn:") => {
                let rest = &s[4..];
                if let Some(lt) = rest.find('<') {
                    let name = rest[..lt].to_string();
                    let generics_str = &rest[lt + 1..rest.len() - 1];
                    let generics: Vec<Type> = if generics_str.is_empty() {
                        vec![]
                    } else {
                        generics_str
                            .split(',')
                            .map(|s| Type::from_hint(s.trim()))
                            .collect()
                    };
                    Type::Trait { name, generics }
                } else {
                    Type::Trait {
                        name: rest.to_string(),
                        generics: vec![],
                    }
                }
            }
            // v0.10 修复: 嵌套泛型 `Foo<Bar<number>>` 解析为 Type::Trait
            s if s.contains('<') && s.ends_with('>') => {
                if let Some(lt) = s.find('<') {
                    let name = s[..lt].to_string();
                    let generics_str = &s[lt + 1..s.len() - 1];
                    let generics: Vec<Type> = if generics_str.is_empty() {
                        vec![]
                    } else {
                        generics_str
                            .split(',')
                            .map(|s| Type::from_hint(s.trim()))
                            .collect()
                    };
                    Type::Trait { name, generics }
                } else {
                    Type::Union(vec![])
                }
            }
            // v0.12: 未知类型名 fallback → 改用 Type::Trait 占位
            //   这样调用方可以查 trait_registry 判断是否合法
            //   （之前是 Any, 丢失了 hint 信息）
            _ => Type::Trait {
                name: hint.to_string(),
                generics: vec![],
            },
        }
    }

    /// v0.12: 判断类型名是否是合法 builtin / 已知类型
    pub fn is_builtin_type_name(name: &str) -> bool {
        matches!(
            name,
            "string"
                | "char"
                | "float"
                | "bool"
                | "nil"
                | "list"
                | "dict"
                | "task"
                | "closure"
                | "conversation"
                | "stream"
                | "ai_config"
                | "ai_result"
                | "ai_error"
                | "ai_module"
                | "router"
                | "http_request"
                | "http_response"
                | "mcp_server"
                | "any"
        )
    }

    /// 类型兼容：Any 总兼容；Result<T,E> 与 Ok/Err 兼容
    /// v0.13: Union 类型支持 —— A ∈ union(expected) 或 expected ∈ union(self)
    pub fn compatible_with(&self, expected: &Type) -> bool {
        // v0.13: Union 兼容 —— self 是 union, expected 是 union 任一成员
        if let Type::Union(members) = expected {
            // 空 Union = "any element type" (兼容任何)
            if members.is_empty() {
                return true;
            }
            return members.iter().any(|m| self.compatible_with(m));
        }
        if let Type::Union(members) = self {
            // 空 Union = "any element type" (兼容任何)
            if members.is_empty() {
                return true;
            }
            return members.iter().any(|m| m.compatible_with(expected));
        }
        // v0.13: Result<T1, E1> 兼容 Result<T2, E2> 当 T1==T2 且 E1==E2 (真正同构)
        if let (Type::Result_(t1, e1), Type::Result_(t2, e2)) = (self, expected) {
            return t1.compatible_with(t2) && e1.compatible_with(e2);
        }
        // v0.x: List<T1> 兼容 List<T2> 当 T1 兼容 T2
        if let (Type::List(a), Type::List(b)) = (self, expected) {
            return a.compatible_with(b);
        }
        // v0.x: Dict<K1, V1> 兼容 Dict<K2, V2> 当 K 兼容且 V 兼容
        if let (Type::Dict(k1, v1), Type::Dict(k2, v2)) = (self, expected) {
            return k1.compatible_with(k2) && v1.compatible_with(v2);
        }
        // v0.08.1: Nil 兼容所有 trait（用于 dyn Trait = nil 占位）
        // v0.12: 后门 2 关闭 —— Nil 仅兼容 Nil, 不再豁免 trait 赋值
        //   若需要 dyn Trait = nil, 显式使用 Option<T> 或 T? 语法
        if matches!(self, Type::Nil) && matches!(expected, Type::Nil) {
            return true;
        }
        if matches!(self, Type::Nil) || matches!(expected, Type::Nil) {
            return false;
        }
        // v0.08.5: Trait 兼容
        // v0.09: 含泛型比较（name 一致 + generics 个数一致 + 元素兼容）
        if let (
            Type::Trait {
                name: a,
                generics: ga,
            },
            Type::Trait {
                name: b,
                generics: gb,
            },
        ) = (self, expected)
        {
            if a != b || ga.len() != gb.len() {
                return false;
            }
            for (x, y) in ga.iter().zip(gb.iter()) {
                if !x.compatible_with(y) {
                    return false;
                }
            }
            return true;
        }
        // v0.08.5: Type::Struct 已删除，统一为 Type::Trait 注册
        self == expected
    }
}

/// v0.13: 判断类型是否是空 Union (即原 Any 占位)
pub fn is_empty_union(ty: &Type) -> bool {
    matches!(ty, Type::Union(m) if m.is_empty())
}

/// 检查是否是已知的内置类型名（大小写不敏感）
pub fn is_known_type(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "string"
            | "char"
            | "float"
            | "bool"
            | "nil"
            | "list"
            | "dict"
            | "task"
            | "closure"
            | "conversation"
            | "stream"
            | "ai_config"
            | "ai_result"
            | "ai_error"
            | "ai_module"
            | "router"
            | "http_request"
            | "http_response"
            | "mcp_server"
            | "result"
            | "atom"
            | "compose"
            | "partial"
            | "macro"
            | "any"
    ) || lower.starts_with("list<")
        || lower.starts_with("dict<")
        || lower.starts_with("result<")
        || lower.starts_with("dyn ")
}

/// v0.x: 在顶层（不进入嵌套 `<...>`）按 ',' 分割字符串。
/// 返回 `Some((head, tail))`；若找不到顶层 ',' 则 `None`。
/// 例：`"string, list<int>"` → `Some(("string", " list<int>"))`
fn split_top_level_comma(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
    }
    None
}

/// 类型错误 + 位置 + 修复建议（v0.05）
#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub line: usize,
    pub column: usize,
    pub message: String,
    /// 期望的类型（可选）
    pub expected: Option<String>,
    /// 实际的类型（可选）
    pub actual: Option<String>,
    /// 修复建议（可选）
    pub hint: Option<String>,
}

impl TypeError {
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            column: 0,
            message: message.into(),
            expected: None,
            actual: None,
            hint: None,
        }
    }

    /// v0.05: 从 Span 构造 (line + column)
    pub fn from_span(span: &Span, message: impl Into<String>) -> Self {
        Self {
            line: span.line,
            column: span.column,
            message: message.into(),
            expected: None,
            actual: None,
            hint: None,
        }
    }

    /// v0.05: 从 Span + 详情构造
    pub fn from_span_with_detail(
        span: &Span,
        message: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            line: span.line,
            column: span.column,
            message: message.into(),
            expected: Some(expected.into()),
            actual: Some(actual.into()),
            hint: Some(hint.into()),
        }
    }

    /// 完整构造：定位 + 期望 + 实际 + 修复建议
    pub fn with_detail(
        line: usize,
        message: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            line,
            column: 0,
            message: message.into(),
            expected: Some(expected.into()),
            actual: Some(actual.into()),
            hint: Some(hint.into()),
        }
    }

    /// 加修复建议
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// 格式化错误信息（含修复建议）
pub fn format_error(err: &TypeError) -> String {
    let mut s = if err.column > 0 {
        format!("Type error at line {}:{}", err.line, err.column)
    } else {
        format!("Type error at line {}", err.line)
    };
    s.push_str(&format!(": {}", err.message));
    if let (Some(exp), Some(act)) = (&err.expected, &err.actual) {
        s.push_str(&format!("\n  expected: {}", exp));
        s.push_str(&format!("\n  actual:   {}", act));
    }
    if let Some(hint) = &err.hint {
        s.push_str(&format!("\n  hint:     {}", hint));
    }
    s
}

// ===================================================================
// 符号表
// ===================================================================

/// 多 scope 嵌套的变量类型表
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    scopes: Vec<HashMap<String, Type>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// 当前 scope 定义变量
    pub fn define(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    /// 沿作用域链查找；找不到返回 Any
    pub fn lookup(&self, name: &str) -> Type {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return t.clone();
            }
        }
        Type::Union(vec![])
    }
}

// ===================================================================
// TypeChecker
// ===================================================================

#[allow(dead_code)]
pub struct TypeChecker {
    /// 全局已知 task / closure 签名（供 call 时检查）
    signatures: HashMap<String, Signature>,
    errors: Vec<TypeError>,
    /// 当前所在 task/closure 的返回类型 hint（None 表示未标注）
    current_return_hint: Option<Type>,
    // v0.05: 已注册的 route 名称（供 `let x = fast(p"...")` 推断为 String）
    routes: std::collections::HashSet<String>,
    // v0.08:  trait/impl 注册表
    trait_registry: HashMap<String, TraitTypeDef>,
    impl_registry: HashMap<(String, String), Vec<String>>,
    // v0.21: 生命周期跟踪
    lifetime_env: LifetimeEnv,
    // v0.21: 借用检查器
    borrow_checker: BorrowChecker,
    // v0.22: 类型缓存 (表达式位置 -> 类型)
    type_cache: HashMap<(usize, usize), Type>,
}

/// v0.21: 生命周期环境
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
struct LifetimeEnv {
    /// 当前作用域中声明的生命周期参数
    declared: Vec<String>,
    /// 变量到生命周期的映射
    bindings: HashMap<String, String>,
}

/// v0.21: 借用状态
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum BorrowKind {
    /// 不可变借用
    Shared,
    /// 可变借用
    Mutable,
}

/// v0.21: 借用跟踪器
#[derive(Debug, Clone, Default)]
struct BorrowChecker {
    /// 变量的借用状态：变量名 -> (借用类型, 借用位置)
    borrows: HashMap<String, Vec<(BorrowKind, Span)>>,
    /// 已移动的变量
    moved: HashSet<String>,
}

/// v0.08: typeck 内使用的 trait 定义
/// v0.08.4: 加 parents 字段实现 trait 继承
#[derive(Debug, Clone)]
// TraitTypeDef 已有 Debug + Clone derive（impl 完整性检查用 tdef.cloned()）
#[allow(dead_code)]
struct TraitTypeDef {
    /// trait 名（Debug 输出 + 未来 trait 名查找用）
    #[allow(dead_code)] // derive(Debug) 已使用，dead_code 检查不计入 derive
    name: String,
    parents: Vec<String>,
    /// v0.09: trait 自身的泛型参数列表（如 `trait Container<T>` 的 `["T"]`）
    generics: Vec<String>,
    /// (method_name, signature, has_default_impl, has_self)
    /// v0.08.5 任务 1: has_self 表示 trait method 第一个参数是否为 `self`
    methods: Vec<(String, Signature, bool, bool)>,
}

/// 任务/闭包签名
#[derive(Debug, Clone)]
pub struct Signature {
    pub params: Vec<(String, Type)>, // (name, type)，未标注为 Any
    /// v0.10 修复: 原始参数 hint 字符串,用于泛型实例化时替换 (e.g. "T" → number)
    pub raw_params: Vec<Option<String>>,
    pub return_type: Type,
    /// v0.10 修复: 原始返回类型 hint 字符串
    pub raw_return_type: Option<String>,
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeChecker {
    /// v0.08.4: 递归收集 trait + 所有父 trait 的方法（去重，防止循环继承）
    /// v0.08.5 任务 1: 返回 (name, sig, has_default, has_self) 4 元组
    #[allow(dead_code)]
    fn collect_trait_methods_recursive(
        &self,
        trait_name: &str,
        visited: &mut std::collections::HashSet<String>,
        out: &mut Vec<(String, Signature, bool, bool)>,
    ) {
        if visited.contains(trait_name) {
            return;
        } // 防循环
        visited.insert(trait_name.to_string());
        let td = match self.trait_registry.get(trait_name) {
            Some(td) => td,
            None => return, // 未知父 trait —— typeck 已报错过
        };
        // 先收集父 trait 的方法（深度优先，子覆盖父）
        for parent in &td.parents {
            self.collect_trait_methods_recursive(parent, visited, out);
        }
        // 再收集本 trait 的方法（同名覆盖父 trait 的）
        for m in &td.methods {
            // 去重：先移除同名旧项（来自父 trait）
            out.retain(|(n, _, _, _)| n != &m.0);
            out.push(m.clone());
        }
    }

    pub fn new() -> Self {
        let mut sigs = HashMap::new();
        // 内置函数签名 (v0.13: Any 全部改 Union)
        //   print 接受 string/number/bool/char/nil
        sigs.insert(
            "print".to_string(),
            Signature {
                params: vec![(
                    "x".to_string(),
                    Type::Union(vec![
                        Type::String,
                        Type::Float,
                        Type::Bool,
                        Type::Char,
                        Type::Nil,
                        Type::List(Box::new(Type::Union(vec![]))), // List<Any>
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
                    ]),
                )],
                raw_params: vec![None],
                return_type: Type::Nil,
                raw_return_type: None,
            },
        );
        // range 全部 number (激进档)
        sigs.insert(
            "range".to_string(),
            Signature {
                params: vec![
                    ("start".to_string(), Type::Float),
                    ("end".to_string(), Type::Float),
                    ("step".to_string(), Type::Float),
                ],
                raw_params: vec![None, None, None],
                return_type: Type::List(Box::new(Type::Float)),
                raw_return_type: None,
            },
        );
        // len 接受 string/list/dict (激进档)
        sigs.insert(
            "len".to_string(),
            Signature {
                params: vec![(
                    "x".to_string(),
                    Type::Union(vec![
                        Type::String,
                        Type::List(Box::new(Type::Union(vec![]))), // List<Any> 占位
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
                    ]),
                )],
                raw_params: vec![None],
                return_type: Type::Float,
                raw_return_type: None,
            },
        );
        // v0.06: ai.chat(cfg: AiConfig, prompt: String) -> AiResult
        sigs.insert(
            "ai.chat".to_string(),
            Signature {
                params: vec![
                    ("cfg".to_string(), Type::AiConfig),
                    ("prompt".to_string(), Type::String),
                ],
                raw_params: vec![None, None],
                return_type: Type::AiResult,
                raw_return_type: None,
            },
        );
        // v0.06.3: Router::new() -> Router
        sigs.insert(
            "Router::new".to_string(),
            Signature {
                params: vec![],
                raw_params: vec![],
                return_type: Type::Router,
                raw_return_type: None,
            },
        );
        // v0.06.6: McpServer::new() -> McpServer
        sigs.insert(
            "McpServer::new".to_string(),
            Signature {
                params: vec![],
                raw_params: vec![],
                return_type: Type::McpServer,
                raw_return_type: None,
            },
        );
        Self {
            signatures: sigs,
            errors: Vec::new(),
            current_return_hint: None,
            routes: std::collections::HashSet::new(),
            // v0.08
            trait_registry: std::collections::HashMap::new(),
            impl_registry: std::collections::HashMap::new(),
            // v0.21
            lifetime_env: LifetimeEnv::default(),
            borrow_checker: BorrowChecker::default(),
            // v0.22
            type_cache: HashMap::new(),
        }
    }

    pub fn errors(&self) -> &[TypeError] {
        &self.errors
    }

    /// 第一趟：收集所有 task 定义（签名）
    /// v0.21: 检查借用冲突
    /// v0.21: 标记变量已移动
    #[allow(dead_code)] // 未来扩展用
    fn mark_moved(&mut self, name: &str, span: Span) {
        // 如果有借用，报错
        if let Some(borrows) = self.borrow_checker.borrows.get(name)
            && !borrows.is_empty()
        {
            self.errors.push(TypeError::from_span(
                &span,
                format!("cannot move `{}` because it is borrowed", name),
            ));
        }
        self.borrow_checker.moved.insert(name.to_string());
        self.borrow_checker.borrows.remove(name);
    }

    #[allow(dead_code)]
    fn check_binary_op(
        &mut self,
        op: BinaryOp,
        lt: &Type,
        rt: &Type,
        line: usize,
        column: usize,
    ) -> Type {
        let span = Span::new(line, column);
        use BinaryOp::*;
        match op {
            Add => {
                // v0.12: 严格化 —— string + string 才返回 string (移除"string + 任意" 兜底)
                //   旧逻辑: "a" + 1 自动转 "a1" (太宽容, 隐藏类型错误)
                //   新逻辑: "a" + 1 必须报错
                if matches!(lt, Type::String) && matches!(rt, Type::String) {
                    return Type::String;
                }
                // v0.24: string + any → string (运行时做字符串拼接)
                if matches!(lt, Type::String) || matches!(rt, Type::String) {
                    return Type::String;
                }
                if let (Type::List(le), Type::List(re)) = (lt, rt) {
                    // list+list 元素类型相同才精确；否则 Any
                    return if le.compatible_with(re) && re.compatible_with(le) {
                        Type::List(le.clone())
                    } else {
                        Type::List(Box::new(Type::Union(vec![])))
                    };
                }
                // number + number (允许 Any 兼容 number 作为 boundary)
                if matches!(lt, Type::Float) && matches!(rt, Type::Float) {
                    return Type::Float;
                }
                // Any 在二元运算中视为"未知" - 上游负责保证不推 Any
                //   (string + Any 也应报错, 因为 Any 已不是 boundary)
                self.errors.push(TypeError::from_span_with_detail(
                    &span,
                    format!(
                        "operator '+' not defined for '{}' and '{}'",
                        lt.name(),
                        rt.name()
                    ),
                    "number + number / string + any / list + list",
                    format!("'{}' + '{}'", lt.name(), rt.name()),
                    "convert both to same type: `let s = str(x); let z = s + ...`",
                ));
                Type::Union(vec![])
            }
            Sub | Mul | Div | Mod => {
                if matches!(lt, Type::Float) && matches!(rt, Type::Float) {
                    Type::Float
                } else {
                    self.errors.push(TypeError::from_span_with_detail(
                        &span,
                        format!(
                            "operator '{}' requires number operands, got '{}' and '{}'",
                            match op {
                                Sub => "-",
                                Mul => "*",
                                Div => "/",
                                Mod => "%",
                                _ => "?",
                            },
                            lt.name(),
                            rt.name()
                        ),
                        "number / number",
                        format!("'{}' / '{}'", lt.name(), rt.name()),
                        "arithmetic operators work on numbers: `let z = 42 + 1`",
                    ));
                    Type::Union(vec![])
                }
            }
            Equal | NotEqual => Type::Bool,
            Greater | Less | GreaterEqual | LessEqual => {
                if matches!(lt, Type::Float | Type::String)
                    && matches!(rt, Type::Float | Type::String)
                {
                    Type::Bool
                } else {
                    self.errors.push(TypeError::from_span_with_detail(
                        &span,
                        format!(
                            "comparison requires number or string, got '{}' and '{}'",
                            lt.name(),
                            rt.name()
                        ),
                        "number or string",
                        format!("'{}' / '{}'", lt.name(), rt.name()),
                        "compare with compatible types: `let eq = (str(x) == str(y))`",
                    ));
                    Type::Union(vec![])
                }
            }
        }
    }
}

// ===================================================================
// 辅助函数
// ===================================================================

/// 给定方法名和接收者类型，返回方法的返回类型
fn method_return_type(receiver: &Type, method: &str) -> Type {
    // v0.x: list<T> 的方法返回类型，元素类型从 receiver 提取
    if let Type::List(elem) = receiver {
        match method {
            "map" | "filter" => return Type::List(elem.clone()),
            "push" => return Type::List(elem.clone()),
            // reduce/pop/get 的返回类型不依赖元素类型，仍为 Any
            "reduce" | "pop" | "get" => return Type::Union(vec![]),
            "len" => return Type::Float,
            _ => {} // fall through to fallback
        }
    }
    // v0.x: dict<K, V> 的方法返回类型
    if let Type::Dict(k, v) = receiver {
        match method {
            // v0.35 (P0-C3): runtime may return Nil on missing key, so the
            // static return type must reflect that: V | Nil.
            "get" => return Type::Union(vec![v.as_ref().clone(), Type::Nil]),
            "set" => return Type::Dict(k.clone(), v.clone()),
            "keys" => return Type::List(k.clone()),
            "values" => return Type::List(v.clone()),
            "len" => return Type::Float,
            _ => {} // fall through to fallback
        }
    }
    method_return_type_fallback(receiver, method)
}

/// 通用方法返回类型（不依赖 list/dict 元素类型）
fn method_return_type_fallback(receiver: &Type, method: &str) -> Type {
    match (receiver, method) {
        (Type::String, "len") => Type::Float,
        (Type::String, "upper" | "lower" | "trim" | "replace") => Type::String,
        (Type::String, "starts_with" | "ends_with" | "contains") => Type::Bool,
        (Type::String, "split") => Type::List(Box::new(Type::String)),
        (Type::Conversation, "chat") => Type::Union(vec![]),
        (Type::Conversation, "history" | "len") => Type::List(Box::new(Type::Union(vec![]))),
        (Type::Conversation, "model") => Type::String,
        // v0.06: ai.chat(prompt, cfg) — 虚线调用, 接收者 ai (AiModule) 的方法
        (Type::AiModule, "chat") => Type::AiResult,
        // v0.06: AiConfig 链式方法 (builder pattern)
        (Type::AiConfig, "model") => Type::AiConfig,
        (Type::AiConfig, "temperature") => Type::AiConfig,
        (Type::AiConfig, "max_tokens") => Type::AiConfig,
        (Type::AiConfig, "system") => Type::AiConfig,
        (Type::AiConfig, "budget") => Type::AiConfig,
        // v0.06.3: Router 链式方法
        (Type::Router, "route") => Type::Router,
        (Type::Router, "listen") => Type::Nil,
        // v0.06.6: McpServer 链式方法
        (Type::McpServer, "tool") => Type::McpServer,
        (Type::McpServer, "serve") => Type::Nil,
        // v0.06.3: HttpRequest 方法
        (Type::HttpRequest, "json") => Type::Union(vec![]), // ~Result<T, ParseError>
        (Type::Union(_), _) => Type::Union(vec![]),
        (_, "len") => Type::Float, // 通用 len
        _ => Type::Union(vec![]),
    }
}

/// v0.10 修复: 在 type hint 字符串中替换 trait 泛型参数名
///   `substitute_type_hint("T", {T: number})` → `Some("float")`
///   `substitute_type_hint("Boxed<T>", {T: number})` → `Some("Boxed<number>")`
///   `substitute_type_hint("T<U>", {T: number, U: string})` → `Some("number<string>")`
///   不在替换表中的标识符保留原样
#[allow(dead_code)]
fn substitute_type_hint(
    hint: &str,
    subst: &std::collections::HashMap<String, Type>,
) -> Option<String> {
    // 简单实现: 按字符扫描,遇到 IDENT 字符就累积,然后查表替换
    let mut result = String::new();
    let chars: Vec<char> = hint.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() || c == '_' {
            // 累积整个标识符
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            if let Some(replacement) = subst.get(&ident) {
                // v0.10 修复: 用原始 hint 字符串重建(支持嵌套 Boxed<number>)
                result.push_str(&type_to_hint_string(replacement));
            } else {
                result.push_str(&ident);
            }
        } else {
            result.push(c);
            i += 1;
        }
    }
    Some(result)
}

/// v0.10 修复: 把 Type 转回 hint 字符串（用于泛型替换）
///   支持 Number/String/Trait{...}/Any 等
#[allow(dead_code)]
fn type_to_hint_string(ty: &Type) -> String {
    match ty {
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::String => "string".to_string(),
        Type::Char => "char".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Nil => "nil".to_string(),
        Type::List(elem) => format!("list<{}>", type_to_hint_string(elem)),
        Type::Dict(k, v) => format!(
            "dict<{},{}>",
            type_to_hint_string(k),
            type_to_hint_string(v)
        ),
        Type::Trait { name, generics } => {
            if generics.is_empty() {
                name.clone()
            } else {
                let inner: Vec<String> = generics.iter().map(type_to_hint_string).collect();
                format!("{}<{}>", name, inner.join(","))
            }
        }
        Type::Union(members) if members.is_empty() => "any".to_string(),
        _ => ty.name().to_string(),
    }
}
// ===================================================================
// 顶层入口
// ===================================================================

// ===================================================================
// v0.55: MirExpr-based type checking (V3 pipeline)
// ===================================================================

/// 对 MirExpr 列表执行类型检查（占位：返回空错误列表）
pub fn check_program_mir(_exprs: &[crate::mir::expr::MirExpr]) -> Vec<TypeError> {
    // TODO: implement HM type inference on MirExpr tree
    vec![]
}
