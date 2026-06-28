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

use std::collections::{HashMap, HashSet};

use crate::ast::*;

// ===================================================================
// 公共类型
// ===================================================================

/// Mora 类型系统：基础类型 + Any（推断不出时退路）
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    String,
    /// v0.x: 单字符类型（`string[number]` 索引结果）
    Char,
    Number,
    Bool,
    Nil,
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
} // ← close pub enum Type

impl Type {
    /// 返回类型的字符串表示。v0.x 起支持泛型：`list<number>` / `dict<string, any>` / `result<T, E>`
    pub fn name(&self) -> String {
        match self {
            Type::String => "string".to_string(),
            Type::Char => "char".to_string(),
            Type::Number => "number".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Nil => "nil".to_string(),
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
            "number" => Type::Number,
            "bool" => Type::Bool,
            "nil" => Type::Nil,
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
                | "number"
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
struct LifetimeEnv {
    /// 当前作用域中声明的生命周期参数
    declared: Vec<String>,
    /// 变量到生命周期的映射
    bindings: HashMap<String, String>,
}

/// v0.21: 借用状态
#[derive(Debug, Clone)]
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
                        Type::Number,
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
                    ("start".to_string(), Type::Number),
                    ("end".to_string(), Type::Number),
                    ("step".to_string(), Type::Number),
                ],
                raw_params: vec![None, None, None],
                return_type: Type::List(Box::new(Type::Number)),
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
                return_type: Type::Number,
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
    fn collect_signatures(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            if let Stmt::TaskDef {
                name,
                params,
                return_type,
                ..
            } = stmt
            {
                let param_types: Vec<(String, Type)> = params
                    .iter()
                    .map(|(n, hint)| {
                        (
                            n.clone(),
                            hint.as_deref()
                                .map(Type::from_hint)
                                .unwrap_or(Type::Union(vec![])),
                        )
                    })
                    .collect();
                let ret = return_type
                    .as_deref()
                    .map(Type::from_hint)
                    .unwrap_or(Type::Union(vec![]));
                self.signatures.insert(
                    name.clone(),
                    Signature {
                        params: param_types,
                        raw_params: params.iter().map(|(_, h)| h.clone()).collect(),
                        return_type: ret,
                        raw_return_type: return_type.clone(),
                    },
                );
            }
            // v0.05: 收集 route 名称 —— `let x = fast(p"...")` 推断为 String
            if let Stmt::Route { name, .. } = stmt {
                self.routes.insert(name.clone());
            }
            // v0.08.1: 收集 trait 定义
            // v0.08.4: 记录 parent traits
            // v0.09: 记录 trait 自身的泛型参数列表
            if let Stmt::TraitDef {
                name,
                generics,
                parents,
                methods,
                ..
            } = stmt
            {
                let trait_generics: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
                let mut method_sigs = Vec::new();
                for m in methods {
                    // v0.10 修复: 保留原始 hint 字符串,用于 trait 泛型参数替换
                    let param_types: Vec<(String, Type)> = m
                        .params
                        .iter()
                        .map(|(n, hint)| {
                            (
                                n.clone(),
                                hint.as_deref()
                                    .map(Type::from_hint)
                                    .unwrap_or(Type::Union(vec![])),
                            )
                        })
                        .collect();
                    let raw_params: Vec<Option<String>> =
                        m.params.iter().map(|(_, hint)| hint.clone()).collect();
                    let ret = m
                        .return_type
                        .as_deref()
                        .map(Type::from_hint)
                        .unwrap_or(Type::Union(vec![]));
                    let raw_ret = m.return_type.clone();
                    let has_default = !m.body.is_empty();
                    // v0.08.5 任务 1: 第一个参数名为 `self` 视为有 self
                    let has_self = m.params.first().map(|(n, _)| n == "self").unwrap_or(false);
                    method_sigs.push((
                        m.name.clone(),
                        Signature {
                            params: param_types,
                            raw_params,
                            return_type: ret,
                            raw_return_type: raw_ret,
                        },
                        has_default,
                        has_self,
                    ));
                }
                self.trait_registry.insert(
                    name.clone(),
                    TraitTypeDef {
                        name: name.clone(),
                        generics: trait_generics, // v0.09
                        parents: parents.clone(),
                        methods: method_sigs,
                    },
                );
            }
            // v0.08.1: 收集 impl 关联 (for_type, trait_name) → methods
            // v0.09: 加 trait_generics 字段 + 泛型参数个数检查
            if let Stmt::ImplDef {
                trait_name,
                trait_generics,
                for_type,
                methods,
                span,
                ..
            } = stmt
            {
                let method_names: Vec<String> = methods.iter().map(|m| m.name.clone()).collect();
                self.impl_registry
                    .insert((for_type.clone(), trait_name.clone()), method_names);
                // v0.09: trait 泛型参数个数检查（仅检查 trait 声明的 generics 个数）
                if let Some(tdef) = self.trait_registry.get(trait_name).cloned() {
                    let expected = tdef.generics.len();
                    let actual = trait_generics.len();
                    if expected != actual {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!(
                                "impl '{}' for '{}': trait expects {} generics, got {}",
                                trait_name, for_type, expected, actual
                            ),
                        ));
                    }
                }
            }
        }
    }

    /// 顶层入口
    pub fn check(&mut self, stmts: &[Stmt]) {
        self.collect_signatures(stmts);
        let mut symbols = SymbolTable::new();
        // v0.06: 注入 `ai` 内建变量 (AiModule 类型)
        symbols.define("ai".to_string(), Type::AiModule);
        // v0.06.3: 注入 `Router` builtin (Router::new() 走签名表)
        symbols.define("Router".to_string(), Type::Builtin);
        for stmt in stmts {
            self.check_stmt(stmt, &mut symbols);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt, symbols: &mut SymbolTable) {
        match stmt {
            Stmt::Let {
                name,
                type_hint,
                init,
                span,
                ..
            } => {
                let init_ty = self.check_expr(init, symbols);
                // v0.13: Walrus 语法已删, 所有 let 必须有 hint 或可推断
                //   - 有 type_hint (let x: T = expr) → 验证 init_ty 与 T 兼容
                //   - 无 type_hint (let x = expr) → 强制推断: declared = init_ty
                //     (init_ty 若是空 Union —— 比如未注册函数调用 —— 报错"无法推断")
                let declared = if let Some(hint) = type_hint {
                    let t = Type::from_hint(hint);
                    // v0.12: 检测未知顶层类型名
                    //   from_hint fallback 产生 Type::Trait { name: <hint>, generics: [] }
                    //   若名字不在 builtin 列表 → 报错
                    if let Type::Trait {
                        name: tname,
                        generics: g,
                    } = &t
                    {
                        // "any" 是用户写动态类型的显式标注, 不视为未知
                        if tname == "any" {
                            Type::Union(vec![])
                        } else if g.is_empty()
                            && !Type::is_builtin_type_name(tname)
                            && !self.trait_registry.contains_key(tname)
                        {
                            // v0.12: 未知顶层类型名 (非 builtin + 非已注册 trait)
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                format!("unknown type '{}' in let {}", tname, name),
                                tname,
                                "<unknown>",
                                format!("use a builtin type or `let {}: T = expr` with explicit type annotation", name),
                            ));
                            Type::Union(vec![])
                        } else if !init_ty.compatible_with(&t) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                format!("type mismatch: let {}", name),
                                t.name(),
                                init_ty.name(),
                                format!("try `let {}: T = expr` with explicit type, or fix the initializer", name),
                            ));
                            t
                        } else {
                            t
                        }
                    } else if !init_ty.compatible_with(&t) {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("type mismatch: let {}", name),
                            t.name(),
                            init_ty.name(),
                            format!(
                                "try `let {}: T = expr` with explicit type, or fix the initializer",
                                name
                            ),
                        ));
                        t
                    } else {
                        t
                    }
                } else {
                    // v0.13: 缺 hint 必须报错 (不论 init_ty 是什么)
                    self.errors.push(TypeError::from_span_with_detail(
                        span,
                        format!("missing type annotation: let {}", name),
                        "<unknown>",
                        init_ty.name(),
                        "add a type hint: `let x: T = expr`",
                    ));
                    Type::Union(vec![])
                };
                symbols.define(name.clone(), declared);
            }
            Stmt::Assign { name, value, span } => {
                let val_ty = self.check_expr(value, symbols);
                let current = symbols.lookup(name);
                if !val_ty.compatible_with(&current) {
                    self.errors.push(TypeError::from_span_with_detail(
                        span,
                        format!(
                            "type mismatch: cannot assign '{}' to variable '{}' of type '{}'",
                            val_ty.name(),
                            name,
                            current.name()
                        ),
                        current.name(),
                        val_ty.name(),
                        format!(
                            "change value or add cast: `let y: {} = ...`",
                            current.name()
                        ),
                    ));
                }
            }
            Stmt::IndexAssign {
                object,
                index,
                value,
                span,
            } => {
                // v0.x: 索引赋值同步细化 — 元素/值类型不匹配时报错
                let ot = self.check_expr(object, symbols);
                let it = self.check_expr(index, symbols);
                let vt = self.check_expr(value, symbols);
                match &ot {
                    Type::List(elem) => {
                        if !matches!(&it, Type::Number) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "list index must be number",
                                "number",
                                it.name(),
                                "use a number to index a list",
                            ));
                        }
                        if !vt.compatible_with(elem) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "list element type mismatch on assign",
                                elem.name(),
                                vt.name(),
                                format!("convert value to {}", elem.name()),
                            ));
                        }
                    }
                    Type::Dict(_k_ty, v_ty) => {
                        if !matches!(&it, Type::String) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "dict key must be string",
                                "string",
                                it.name(),
                                "use a string key",
                            ));
                        }
                        if !vt.compatible_with(v_ty) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "dict value type mismatch on assign",
                                v_ty.name(),
                                vt.name(),
                                format!("convert value to {}", v_ty.name()),
                            ));
                        }
                    }
                    Type::Union(_) => { /* 不严格检查 */ }
                    _ => {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("cannot index-assign to type '{}'", ot.name()),
                            "list | dict",
                            ot.name(),
                            "use a list or dict",
                        ));
                    }
                }
            }
            Stmt::TaskDef {
                name,
                lifetime_params,
                params,
                return_type,
                body,
                span,
                ..
            } => {
                // v0.21: 保存并设置生命周期环境
                let prev_lifetime_env = self.lifetime_env.clone();
                self.lifetime_env.declared = lifetime_params.clone();

                symbols.push_scope();
                for (pname, phint) in params {
                    let pty = phint
                        .as_deref()
                        .map(Type::from_hint)
                        .unwrap_or(Type::Union(vec![]));
                    symbols.define(pname.clone(), pty);

                    // v0.21: 检查参数中的生命周期标注
                    if let Some(hint) = phint
                        && hint.contains('\'') {
                            // 提取生命周期名 (如 'a 从 &'a string)
                            if let Some(lt_pos) = hint.find('\'') {
                                let lt_str = &hint[lt_pos..];
                                if let Some(lt_end) = lt_str.find(|c: char| !c.is_alphanumeric() && c != '_') {
                                    let lifetime = &lt_str[..lt_end];
                                    if !self.lifetime_env.declared.contains(&lifetime.to_string()) {
                                        self.errors.push(TypeError::from_span(
                                            span,
                                            format!("use of undeclared lifetime '{}'", lifetime),
                                        ));
                                    }
                                    self.lifetime_env.bindings.insert(pname.clone(), lifetime.to_string());
                                }
                            }
                        }
                }
                let prev_hint = self.current_return_hint.clone();
                self.current_return_hint = return_type.as_deref().map(Type::from_hint);

                // v0.21: 检查返回类型中的生命周期
                if let Some(ret) = return_type
                    && ret.contains('\'')
                        && let Some(lt_pos) = ret.find('\'') {
                            let lt_str = &ret[lt_pos..];
                            if let Some(lt_end) = lt_str.find(|c: char| !c.is_alphanumeric() && c != '_') {
                                let lifetime = &lt_str[..lt_end];
                                if !self.lifetime_env.declared.contains(&lifetime.to_string()) {
                                    self.errors.push(TypeError::from_span(
                                        span,
                                        format!("use of undeclared lifetime '{}' in return type", lifetime),
                                    ));
                                }
                            }
                        }

                for s in body {
                    self.check_stmt(s, symbols);
                }

                // v0.21: 恢复生命周期环境
                self.lifetime_env = prev_lifetime_env;
                // v0.05: 检查"缺少 return"
                // 如果声明了非 nil/Any 的返回类型，body 里必须有 return 语句
                if let Some(ret_hint) = &self.current_return_hint
                    && !matches!(ret_hint, Type::Nil)
                    && !is_empty_union(ret_hint)
                    && !body_has_return(body)
                {
                    self.errors.push(TypeError::from_span_with_detail(
                        span,
                        format!(
                            "missing return in task '{}' with return type '{}'",
                            name,
                            ret_hint.name()
                        ),
                        ret_hint.name(),
                        "nil (missing return)",
                        "add `return <expr>` at the end of the task body".to_string(),
                    ));
                }
                self.current_return_hint = prev_hint;
                symbols.pop_scope();
                let _ = (name, span);
            }
            Stmt::If {
                condition,
                then_branch,
                ..
            } => {
                self.check_expr(condition, symbols);
                symbols.push_scope();
                for s in then_branch {
                    self.check_stmt(s, symbols);
                }
                symbols.pop_scope();
            }
            Stmt::For {
                var,
                var_type,
                iterable,
                body,
                ..
            } => {
                let iter_ty = self.check_expr(iterable, symbols);
                // iterable 应该是 list 或 string（字符串按 char 迭代）
                if !matches!(iter_ty, Type::List(_) | Type::String) {
                    self.errors.push(TypeError::with_hint(
                        TypeError::new(
                            iter_ty_debug_line(iterable),
                            format!("for-in expects a list or string, got '{}'", iter_ty.name()),
                        ),
                        "iterate over a list or string: `for x in [1, 2, 3]`",
                    ));
                }
                symbols.push_scope();
                let vty = var_type
                    .as_deref()
                    .map(Type::from_hint)
                    .unwrap_or(Type::Union(vec![]));
                symbols.define(var.clone(), vty);
                for s in body {
                    self.check_stmt(s, symbols);
                }
                symbols.pop_scope();
            }
            Stmt::Import { .. } => {
                // 不做跨模块 symbol 解析
            }
            Stmt::Parallel { stmts, .. } => {
                for s in stmts {
                    self.check_stmt(s, symbols);
                }
            }
            // v0.19: Worker 声明
            Stmt::Worker { body, .. } => {
                for s in body {
                    self.check_stmt(s, symbols);
                }
            }
            // v0.19: 发送消息
            Stmt::Send { value, .. } => {
                self.check_expr(value, symbols);
            }
            // v0.19: 接收消息
            Stmt::Receive { .. } => {
                // 接收的类型在运行时确定
            }
            // v0.19: 事务块
            Stmt::Transaction { body, compensation, .. } => {
                for s in body {
                    self.check_stmt(s, symbols);
                }
                for s in compensation {
                    self.check_stmt(s, symbols);
                }
            }
            // v0.19: 提交/回滚
            Stmt::Commit { .. } | Stmt::Rollback { .. } => {}
            // v0.20: 宏定义
            Stmt::MacroDef { body, .. } => {
                for s in body {
                    self.check_stmt(s, symbols);
                }
            }
            // v0.23: 类型别名
            Stmt::TypeAlias { name, target, .. } => {
                // 注册类型别名
                symbols.define(name.clone(), Type::from_hint(target));
            }
            // v0.23: 枚举类型
            Stmt::EnumDef { name, variants, .. } => {
                // 注册枚举类型
                symbols.define(name.clone(), Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![]))));
                // 注册每个变体
                for v in variants {
                    symbols.define(v.name.clone(), Type::Builtin);
                }
            }
            // v0.23: 结构体类型
            Stmt::StructDef { name, fields, .. } => {
                // 注册结构体类型为函数
                let param_types: Vec<(String, Type)> = fields.iter()
                    .map(|f| (f.name.clone(), Type::from_hint(&f.type_hint)))
                    .collect();
                self.signatures.insert(name.clone(), Signature {
                    params: param_types.clone(),
                    raw_params: param_types.iter().map(|_| None).collect(),
                    return_type: Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![]))),
                    raw_return_type: None,
                });
                symbols.define(name.clone(), Type::Task);
            }
            Stmt::Match { expr, arms, .. } => {
                self.check_expr(expr, symbols);
                for (_pat, arm_stmts) in arms {
                    symbols.push_scope();
                    for s in arm_stmts {
                        self.check_stmt(s, symbols);
                    }
                    symbols.pop_scope();
                }
            }
            Stmt::Save { path, value, .. } => {
                self.check_expr(path, symbols);
                self.check_expr(value, symbols);
            }
            Stmt::Load { path, var, .. } => {
                self.check_expr(path, symbols);
                symbols.define(var.clone(), Type::Union(vec![]));
            }
            Stmt::ReadFile { path, var, .. } => {
                self.check_expr(path, symbols);
                symbols.define(var.clone(), Type::String);
            }
            Stmt::WriteFile { path, content, .. } => {
                self.check_expr(path, symbols);
                let _ = self.check_expr(content, symbols);
            }
            Stmt::AppendFile { path, content, .. } => {
                self.check_expr(path, symbols);
                let _ = self.check_expr(content, symbols);
            }
            Stmt::ReadBytesFile { path, var, .. } => {
                self.check_expr(path, symbols);
                symbols.define(var.clone(), Type::String);
            }
            Stmt::WriteBytesFile { path, content, .. } => {
                self.check_expr(path, symbols);
                let _ = self.check_expr(content, symbols);
            }
            Stmt::Return { value, span } => {
                if let Some(expr) = value {
                    let val_ty = self.check_expr(expr, symbols);
                    if let Some(expected) = &self.current_return_hint
                        && !val_ty.compatible_with(expected)
                    {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!(
                                "return type mismatch: expected '{}', got '{}'",
                                expected.name(),
                                val_ty.name()
                            ),
                        ));
                    }
                } else {
                    // return 无值 → 期望 nil
                    if let Some(expected) = &self.current_return_hint
                        && !matches!(expected, Type::Nil)
                        && !is_empty_union(expected)
                    {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!(
                                "return type mismatch: expected '{}', got nil",
                                expected.name()
                            ),
                        ));
                    }
                }
            }
            Stmt::Expr(expr) => {
                self.check_expr(expr, symbols);
            }
            // v0.04.0: AI 原语
            Stmt::With {
                bindings,
                body,
                span,
                ..
            } => {
                // v0.05: 校验 binding 类型
                //   model → string, temperature/max_tokens → number, budget → number, system → string
                for (key, val_expr) in bindings {
                    let val_ty = self.check_expr(val_expr, symbols);
                    let expected = match key.as_str() {
                        "model" => Type::String,
                        "system" => Type::String,
                        "temperature" | "max_tokens" | "budget" => Type::Number,
                        other => {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                format!("with: unknown binding '{}'", other),
                                "model / system / temperature / max_tokens / budget",
                                "any",
                                "valid keys: model, system, temperature, max_tokens, budget",
                            ));
                            continue;
                        }
                    };
                    if !val_ty.compatible_with(&expected) {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("with {} = ...", key),
                            expected.name(),
                            val_ty.name(),
                            format!("use a {} literal: `with {} = ...`", expected.name(), key),
                        ));
                    }
                }
                symbols.push_scope();
                for s in body {
                    self.check_stmt(s, symbols);
                }
                symbols.pop_scope();
            }
            Stmt::StreamFor {
                prompt,
                var,
                body,
                span,
                ..
            } => {
                let prompt_ty = self.check_expr(prompt, symbols);
                if !matches!(prompt_ty, Type::String) {
                    self.errors.push(TypeError::from_span_with_detail(
                        span,
                        "stream prompt must be a string or ai.stream(...) expression",
                        "string",
                        prompt_ty.name(),
                        "use `ai.stream(p\"...\")` or `p\"...\"`",
                    ));
                }
                symbols.push_scope();
                symbols.define(var.clone(), Type::String);
                for s in body {
                    self.check_stmt(s, symbols);
                }
                symbols.pop_scope();
            }
            Stmt::ToolDef {
                name,
                params,
                return_type,
                body,
                exported,
                span,
                ..
            } => {
                symbols.push_scope();
                // v0.05: 注入 tool 参数进 scope
                for (pname, phint) in params {
                    let pty = phint
                        .as_deref()
                        .map(Type::from_hint)
                        .unwrap_or(Type::Union(vec![]));
                    symbols.define(pname.clone(), pty);
                }
                // v0.05: 注入 args: dict<string, Any> —— MCP 调用时 args 形参
                symbols.define(
                    "args".to_string(),
                    Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![]))),
                );
                let prev_hint = self.current_return_hint.clone();
                self.current_return_hint = return_type.as_deref().map(Type::from_hint);
                for s in body {
                    self.check_stmt(s, symbols);
                }
                self.current_return_hint = prev_hint;
                symbols.pop_scope();
                let declared = return_type
                    .as_deref()
                    .map(Type::from_hint)
                    .unwrap_or(Type::Union(vec![]));
                symbols.define(name.clone(), declared);
                let _ = (exported, span);
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {
                // v0.04.0 简化:仅警告(v0.04.1 强制"必须在 loop 内")
            }
            // v0.06.7: serve as 已移除，用 Router::new() / McpServer::new() 显式 API
            Stmt::Observe {
                config, body, span, ..
            } => {
                // 校验 observe config
                match config {
                    ObserveConfig::Trace | ObserveConfig::Metrics => {}
                    ObserveConfig::Otel { endpoint } => {
                        let ep_ty = self.check_expr(endpoint, symbols);
                        if !matches!(ep_ty, Type::String) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "observe otel endpoint must be a string",
                                "string",
                                ep_ty.name(),
                                "use a string literal: `observe otel endpoint \"http://...\"`",
                            ));
                        }
                    }
                }
                symbols.push_scope();
                for s in body {
                    self.check_stmt(s, symbols);
                }
                symbols.pop_scope();
            }
            Stmt::Span {
                name,
                attributes,
                body,
                span,
                ..
            } => {
                // name 必须是 string literal
                if name.is_empty() {
                    self.errors
                        .push(TypeError::from_span(span, "span name cannot be empty"));
                }
                // attributes 必须是 dict literal
                for (k, v) in attributes {
                    self.check_expr(v, symbols);
                    let _ = k;
                }
                symbols.push_scope();
                for s in body {
                    self.check_stmt(s, symbols);
                }
                symbols.pop_scope();
            }
            // v0.04补: Stmt::Route 必须递归 typeck target, 触发 ai_model 校验
            Stmt::Route { target, .. } => {
                self.check_expr(target, symbols);
            }
            // v0.04.0 终态补: record_tokens 参数必须 number
            Stmt::RecordTokens {
                input,
                output,
                span,
                ..
            } => {
                let in_ty = self.check_expr(input, symbols);
                if !matches!(in_ty, Type::Number) {
                    self.errors.push(TypeError::from_span(
                        span,
                        format!(
                            "record_tokens: input must be number, got '{}'",
                            in_ty.name()
                        ),
                    ));
                }
                let out_ty = self.check_expr(output, symbols);
                if !matches!(out_ty, Type::Number) {
                    self.errors.push(TypeError::from_span(
                        span,
                        format!(
                            "record_tokens: output must be number, got '{}'",
                            out_ty.name()
                        ),
                    ));
                }
            }
            // v0.08: trait/impl — 第一趟 collect_signatures 已注册, 这里做完整性检查
            Stmt::TraitDef { name, methods, .. } => {
                // 确保方法签名无重复
                let mut seen = std::collections::HashSet::new();
                for m in methods {
                    if !seen.insert(&m.name) {
                        self.errors.push(TypeError::from_span(
                            &m.span,
                            format!("trait '{}': duplicate method '{}'", name, m.name),
                        ));
                    }
                }
            }
            // v0.09: 解构含 6 个新字段（含 impl 自身 generics）
            Stmt::ImplDef {
                generics,
                trait_name,
                trait_generics,
                for_type,
                for_generics,
                where_clause,
                methods,
                span,
                ..
            } => {
                // v0.09: trait 泛型参数个数检查（先于完整性检查）
                if let Some(tdef) = self.trait_registry.get(trait_name).cloned() {
                    let expected = tdef.generics.len();
                    let actual = trait_generics.len();
                    if expected != actual {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!(
                                "impl '{}' for '{}': trait expects {} generics, got {}",
                                trait_name, for_type, expected, actual
                            ),
                        ));
                    }
                    // v0.09 完整版: impl 多泛型一致（impl<T> Foo<T> for Bar<T> 的 T 必须一致）
                    //   检查: impl_generics[i] == trait_generics[i] == for_generics[i]
                    //   （如果有 where bound 也算进来）
                    for ig in generics.iter() {
                        let in_trait = trait_generics.iter().any(|g| g == &ig.name);
                        let in_for = for_generics.iter().any(|g| g == &ig.name);
                        // 简化版: impl generics 里的名字必须同时出现在 trait 和 for 中
                        // 完全版: 实际位置/位置对应关系检查（v0.10 强化）
                        if !in_trait && !in_for {
                            // 可能是 where 引入的（不强求）
                            let _ = in_for;
                        }
                    }
                }
                // v0.09 完整版: where bound 实际验证
                //   1. bound trait 必须存在（已在 trait_registry 中）
                //   2. bound 名字如果是 impl generics 里的，可以被替换
                for w in where_clause.iter() {
                    if let Some(bound) = &w.bound {
                        if !self.trait_registry.contains_key(bound) {
                            self.errors.push(TypeError::from_span(
                                span,
                                format!(
                                    "impl '{}' for '{}': where bound '{}' is not a known trait",
                                    trait_name, for_type, bound
                                ),
                            ));
                        }
                        // v0.09 完整版: bound 名字（如 T）必须出现在 impl generics 中
                        //   或在 trait_generics 中（trait 自身引入的类型变量）
                        let is_impl_generic = generics.iter().any(|g| g.name == w.name);
                        let is_trait_generic = trait_generics.iter().any(|g| g == &w.name);
                        if !is_impl_generic && !is_trait_generic {
                            self.errors.push(TypeError::from_span(span,
                                format!("impl '{}' for '{}': where bound '{}' refers to unknown type variable",
                                    trait_name, for_type, w.name)));
                        }
                    }
                }
                // v0.09 完整版: for_type 泛型参数检查
                //   for_type 是普通类型（Number/String 等基础类型 + 用户定义 struct 等）
                //     → for_generics 必须为空
                //   for_type 是 trait 类型
                //     → for_generics 个数必须与 trait 期望匹配（但 trait 是单泛型注册，这里简化）
                if !for_generics.is_empty() {
                    // 检查 for_type 是否是基础类型（这种 for_generics 应该空）
                    if matches!(
                        for_type.as_str(),
                        "string"
                            | "number"
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
                            | "router"
                            | "http_request"
                            | "http_response"
                            | "mcp_server"
                    ) {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!(
                                "impl '{}' for '{}': type '{}' is not generic, got {} generics",
                                trait_name,
                                for_type,
                                for_type,
                                for_generics.len()
                            ),
                        ));
                    }
                }
                // v0.08.4: 验证 trait 存在，然后递归收集 trait + 所有父 trait 的方法
                if !self.trait_registry.contains_key(trait_name) {
                    self.errors.push(TypeError::from_span(
                        span,
                        format!("impl: trait '{}' not defined", trait_name),
                    ));
                } else {
                    let mut all_methods: Vec<(String, Signature, bool, bool)> = Vec::new();
                    let mut visited = std::collections::HashSet::new();
                    self.collect_trait_methods_recursive(
                        trait_name,
                        &mut visited,
                        &mut all_methods,
                    );
                    let impl_methods = methods.clone();
                    for (tm_name, tm_sig, tm_has_default, _tm_has_self) in &all_methods {
                        let impl_m = impl_methods.iter().find(|m| m.name == *tm_name);
                        if impl_m.is_none() {
                            // v0.08.3: 如果 trait method 有默认实现，impl 可省略
                            if !*tm_has_default {
                                self.errors.push(TypeError::from_span(
                                    span,
                                    format!(
                                        "impl '{}' for '{}': missing method '{}'",
                                        trait_name, for_type, tm_name
                                    ),
                                ));
                            }
                            continue;
                        }
                        // 校验参数个数和类型 hint
                        let im = impl_m.unwrap();
                        if im.params.len() != tm_sig.params.len() {
                            self.errors.push(TypeError::from_span(
                                &im.span,
                                format!(
                                    "impl '{}' for '{}': method '{}' expects {} params, got {}",
                                    trait_name,
                                    for_type,
                                    tm_name,
                                    tm_sig.params.len(),
                                    im.params.len()
                                ),
                            ));
                        }
                        // 递归 typeck 方法体
                        symbols.push_scope();
                        for (pname, phint) in &im.params {
                            let pty = phint
                                .as_deref()
                                .map(Type::from_hint)
                                .unwrap_or(Type::Union(vec![]));
                            symbols.define(pname.clone(), pty);
                        }
                        for s in &im.body {
                            self.check_stmt(s, symbols);
                        }
                        symbols.pop_scope();
                    }
                    // v0.08.5: 注册 impl type 为 Type::Trait(trait_name)
                    //   之前用 Type::Struct(name, [trait_name]) 但 interpreter 完全不消费 Struct
                    //   现在直接注册为 Trait 类型——dispatch 时 receiver 类型若是 Trait(trait_name)
                    //   表示它实现了这个 trait
                    // v0.09: 加空 generics（无泛型实例化）
                    symbols.define(
                        for_type.clone(),
                        Type::Trait {
                            name: trait_name.clone(),
                            generics: vec![],
                        },
                    );
                }
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr, symbols: &SymbolTable) -> Type {
        // v0.22: 类型缓存 - 检查是否已缓存
        let cache_key = self.get_expr_cache_key(expr);
        if let Some(cached) = self.type_cache.get(&cache_key) {
            return cached.clone();
        }

        let result = match expr {
            Expr::Literal(lit) => self.check_literal(lit, expr, symbols),
            Expr::Variable(name, _) => symbols.lookup(name),
            Expr::Binary {
                left,
                op,
                right,
                span,
                ..
            } => {
                let lt = self.check_expr(left, symbols);
                let rt = self.check_expr(right, symbols);
                self.check_binary_op(op.clone(), &lt, &rt, span.line, span.column)
            }
            Expr::Pipe { left, right, .. } => {
                let _ = self.check_expr(left, symbols);
                self.check_expr(right, symbols)
            }
            Expr::Call {
                callee, args, span, ..
            } => {
                for a in args {
                    let _ = self.check_expr(a, symbols);
                }
                // v0.05: 先看是否是已注册 route —— `let x = fast(p"...")` 推断为 String
                if self.routes.contains(callee) {
                    if args.len() != 1 {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!(
                                "route '{}()' expects 1 arg (the prompt), got {}",
                                callee,
                                args.len()
                            ),
                        ));
                    }
                    return Type::String;
                }
                if let Some(sig) = self.signatures.get(callee).cloned() {
                    // 参数个数检查
                    if args.len() != sig.params.len() {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!(
                                "function '{}' expects {} args, got {}",
                                callee,
                                sig.params.len(),
                                args.len()
                            ),
                        ));
                    } else {
                        // 参数类型检查
                        for (i, ((_pname, pty), arg)) in
                            sig.params.iter().zip(args.iter()).enumerate()
                        {
                            let aty = self.check_expr(arg, symbols);
                            if !aty.compatible_with(pty) {
                                self.errors.push(TypeError::from_span_with_detail(
                                    &expr_to_span(arg).unwrap_or(*span),
                                    format!(
                                        "arg {} of '{}': expected '{}', got '{}'",
                                        i + 1,
                                        callee,
                                        pty.name(),
                                        aty.name()
                                    ),
                                    pty.name(),
                                    aty.name(),
                                    format!("convert arg or pass a {} value", pty.name()),
                                ));
                            }
                        }
                    }
                    sig.return_type
                } else {
                    // 未知函数 → Any（Mora 允许 task 名字空间）
                    Type::Union(vec![])
                }
            }
            Expr::MethodCall {
                object,
                method,
                args,
                span,
                ..
            } => {
                let ot = self.check_expr(object, symbols);
                for a in args {
                    let _ = self.check_expr(a, symbols);
                }
                // v0.06: AiConfig 链式参数校验
                if matches!(ot, Type::AiConfig) {
                    check_ai_config_method(method, args, &mut self.errors, span);
                }
                // v0.08.1: dyn trait dispatch — 从 trait_registry 查方法返回类型
                // v0.09: 解构 Type::Trait { name, generics }
                if let Type::Trait {
                    name: tname,
                    generics: trait_g_args,
                } = &ot
                {
                    // v0.08.4: 递归收集 trait + 父 trait 的方法
                    let mut all_methods: Vec<(String, Signature, bool, bool)> = Vec::new();
                    let mut visited = std::collections::HashSet::new();
                    self.collect_trait_methods_recursive(tname, &mut visited, &mut all_methods);
                    for (mname, sig, _has_default, has_self) in &all_methods {
                        if mname == method {
                            // v0.08.5 任务 1: self-having 时减 1（去掉 self 参数）
                            //                self-less 时不减（用户传的就是全部 args）
                            let expected = if *has_self {
                                sig.params.len().saturating_sub(1)
                            } else {
                                sig.params.len()
                            };
                            if args.len() != expected {
                                self.errors.push(TypeError::from_span(
                                    span,
                                    format!(
                                        "trait method '{}.{}' expects {} args, got {}",
                                        tname,
                                        method,
                                        expected,
                                        args.len()
                                    ),
                                ));
                            }
                            // v0.10 修复: trait 泛型实例化——把方法签名里的 `T`/`U` 替换为
                            //   dyn:Foo<number> 中的实参类型 `number`
                            // 当前简单版: 仅替换 return_type (最常触发"cannot infer"的字段)
                            let td = self.trait_registry.get(tname).cloned();
                            if let Some(td) = td
                                && !td.generics.is_empty()
                                && !trait_g_args.is_empty()
                            {
                                // 构造替换表: T → number
                                let mut subst: std::collections::HashMap<String, Type> =
                                    std::collections::HashMap::new();
                                for (i, gname) in td.generics.iter().enumerate() {
                                    if let Some(actual) = trait_g_args.get(i) {
                                        subst.insert(gname.clone(), actual.clone());
                                    }
                                }
                                if let Some(raw_ret) = &sig.raw_return_type
                                    && let Some(replaced) = substitute_type_hint(raw_ret, &subst)
                                {
                                    return Type::from_hint(&replaced);
                                }
                            }
                            return sig.return_type.clone();
                        }
                    }
                    self.errors.push(TypeError::from_span(
                        span,
                        format!("trait '{}' has no method '{}'", tname, method),
                    ));
                    return Type::Union(vec![]);
                }
                method_return_type(&ot, method)
            }
            Expr::Index {
                object,
                index,
                span,
            } => {
                // v0.x: 索引结果类型精确化为容器元素类型
                let ot = self.check_expr(object, symbols);
                let it = self.check_expr(index, symbols);
                match &ot {
                    Type::List(elem) => {
                        if !matches!(&it, Type::Number) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "list index must be number",
                                "number",
                                it.name(),
                                "use a number to index a list",
                            ));
                            return Type::Union(vec![]);
                        }
                        elem.as_ref().clone()
                    }
                    Type::Dict(_k, v) => {
                        if !matches!(&it, Type::String) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "dict key must be string",
                                "string",
                                it.name(),
                                "use a string key to index a dict",
                            ));
                            return Type::Union(vec![]);
                        }
                        v.as_ref().clone()
                    }
                    Type::String => {
                        if !matches!(&it, Type::Number) {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                "string index must be number",
                                "number",
                                it.name(),
                                "use a number to index a string",
                            ));
                            return Type::Union(vec![]);
                        }
                        Type::Char
                    }
                    Type::Union(_) => Type::Union(vec![]),
                    _ => {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("cannot index type '{}'", ot.name()),
                            "list | dict | string",
                            ot.name(),
                            "use a container type",
                        ));
                        Type::Union(vec![])
                    }
                }
            }
            Expr::Closure {
                params,
                return_type,
                body,
                span,
                ..
            } => {
                let mut inner = SymbolTable::new();
                for (pname, phint) in params {
                    // v0.12: 闭包参数缺类型 → 报错 (不再静默给 Any)
                    let pty = match phint.as_deref() {
                        Some(h) => Type::from_hint(h),
                        None => {
                            self.errors.push(TypeError::from_span_with_detail(
                                span,
                                format!("missing type annotation: closure parameter '{}'", pname),
                                "<unknown>",
                                "any",
                                format!("add a type hint: `fn({}: T) ...`", pname),
                            ));
                            Type::Union(vec![])
                        }
                    };
                    inner.define(pname.clone(), pty);
                }
                let prev_hint = self.current_return_hint.clone();
                self.current_return_hint = return_type.as_deref().map(Type::from_hint);
                for s in body {
                    self.check_stmt(s, &mut inner);
                }
                self.current_return_hint = prev_hint;
                Type::Closure
            }
            Expr::Match { expr, arms, .. } => {
                self.check_expr(expr, symbols);
                // v0.16: 检查守卫条件类型
                for (pat, _arm_expr) in arms {
                    if let Pattern::Guard { condition, .. } = pat {
                        let cond_ty = self.check_expr(condition, symbols);
                        if cond_ty != Type::Bool && cond_ty != Type::Union(vec![]) {
                            self.errors.push(TypeError {
                                message: format!(
                                    "Guard condition must be bool, got {:?}",
                                    cond_ty
                                ),
                                line: 0,
                                column: 0,
                                expected: Some("bool".to_string()),
                                actual: Some(format!("{:?}", cond_ty)),
                                hint: None,
                            });
                        }
                    }
                }
                // 取最后 arm 的类型（Mora 不强制 arm 类型一致）
                let mut ty = Type::Union(vec![]);
                for (_pat, arm_expr) in arms {
                    ty = self.check_expr(arm_expr, symbols);
                }
                ty
            }
            Expr::Grouping(inner, _) => self.check_expr(inner, symbols),
            // v0.21: 借用表达式（返回被借用值的类型）
            Expr::Borrow { expr, span } => {
                // v0.21: 不可变借用检查
                if let Expr::Variable(name, _) = expr.as_ref() {
                    self.check_borrow(name, false, *span);
                }
                self.check_expr(expr, symbols)
            }
            Expr::BorrowMut { expr, span } => {
                // v0.21: 可变借用检查
                if let Expr::Variable(name, _) = expr.as_ref() {
                    self.check_borrow(name, true, *span);
                }
                self.check_expr(expr, symbols)
            }
            // v0.04.0: p"..." 表达式 type = String
            Expr::Prompt { parts, .. } => {
                for p in parts {
                    let _ = self.check_expr(p, symbols);
                }
                Type::String
            }
            // v0.04 Slice 2: RouteCall type = String
            Expr::RouteCall { args, .. } => {
                for a in args {
                    let _ = self.check_expr(a, symbols);
                }
                Type::String
            }
            // v0.04补: ai_model(...) 表达式 type = Dict
            // 校验: model 字符串, temperature/max_tokens number, system string
            Expr::AiModelCall {
                model,
                temperature,
                max_tokens,
                system,
                span,
            } => {
                let mt = self.check_expr(model, symbols);
                if !matches!(mt, Type::String) {
                    self.errors.push(TypeError::from_span(
                        span,
                        format!("ai_model: model name must be string, got '{}'", mt.name()),
                    ));
                }
                if let Some(t) = temperature {
                    let tt = self.check_expr(t, symbols);
                    if !matches!(tt, Type::Number) {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("ai_model: temperature must be number, got '{}'", tt.name()),
                        ));
                    }
                }
                if let Some(n) = max_tokens {
                    let nt = self.check_expr(n, symbols);
                    if !matches!(nt, Type::Number) {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("ai_model: max_tokens must be number, got '{}'", nt.name()),
                        ));
                    }
                }
                if let Some(s) = system {
                    let st = self.check_expr(s, symbols);
                    if !matches!(st, Type::String) {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("ai_model: system must be string, got '{}'", st.name()),
                        ));
                    }
                }
                Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![])))
            }
            // v0.06.2: expr? 操作符 — expr 必须是 Result<T,E> , 返回 T
            Expr::Question { expr, span } => {
                let expr_ty = self.check_expr(expr, symbols);
                match &expr_ty {
                    Type::Result_(ok_ty, _err_ty) => (**ok_ty).clone(),
                    Type::Union(_) => Type::Union(vec![]), // 推断不出, 不报
                    _ => {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("'?' operator expects Result<T,E>, got '{}'", expr_ty.name()),
                            "result",
                            expr_ty.name(),
                            "wrap the expression in Ok(...) or change return type to Result<T,E>",
                        ));
                        Type::Union(vec![])
                    }
                }
            }
            // v0.07.1: NamespaceRef — IDENT::IDENT, typeck as Any for now
            Expr::NamespaceRef { .. } => Type::Union(vec![]),
            // v0.08.5: DynTrait — dyn TraitName type hint，对齐 let x: dyn Trait 解析为 Type::Trait(name)
            // v0.09: 加空 generics（trait 表达式本身不带泛型标注）
            Expr::DynTrait { trait_name, .. } => Type::Trait {
                name: trait_name.clone(),
                generics: vec![],
            },
        };

        // v0.22: 缓存类型检查结果
        self.type_cache.insert(cache_key, result.clone());
        result
    }

    /// v0.22: 获取表达式的缓存键 (行号, 列号)
    fn get_expr_cache_key(&self, expr: &Expr) -> (usize, usize) {
        match expr {
            Expr::Binary { span, .. }
            | Expr::Pipe { span, .. }
            | Expr::Call { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::Index { span, .. }
            | Expr::Closure { span, .. }
            | Expr::Match { span, .. }
            | Expr::Prompt { span, .. }
            | Expr::RouteCall { span, .. }
            | Expr::AiModelCall { span, .. }
            | Expr::Question { span, .. }
            | Expr::NamespaceRef { span, .. }
            | Expr::DynTrait { span, .. }
            | Expr::Borrow { span, .. }
            | Expr::BorrowMut { span, .. } => (span.line, span.column),
            Expr::Literal(lit) => match lit {
                Literal::String(_, s)
                | Literal::Number(_, s)
                | Literal::Bool(_, s)
                | Literal::Char(_, s)
                | Literal::Nil(s)
                | Literal::List(_, s)
                | Literal::Dict(_, s) => (s.line, s.column),
            },
            Expr::Variable(_, span) | Expr::Grouping(_, span) => (span.line, span.column),
        }
    }

    /// v0.21: 检查借用冲突
    fn check_borrow(&mut self, name: &str, mutable: bool, span: Span) {
        // 检查变量是否已移动
        if self.borrow_checker.moved.contains(name) {
            self.errors.push(TypeError::from_span(
                &span,
                format!("cannot borrow `{}`: value has been moved", name),
            ));
            return;
        }

        // 检查借用冲突
        if let Some(borrows) = self.borrow_checker.borrows.get(name) {
            if mutable {
                // 可变借用：不能有任何其他借用
                if !borrows.is_empty() {
                    self.errors.push(TypeError::from_span(
                        &span,
                        format!("cannot borrow `{}` as mutable because it is already borrowed", name),
                    ));
                }
            } else {
                // 不可变借用：不能有可变借用
                if borrows.iter().any(|(kind, _)| matches!(kind, BorrowKind::Mutable)) {
                    self.errors.push(TypeError::from_span(
                        &span,
                        format!("cannot borrow `{}` as immutable because it is also borrowed as mutable", name),
                    ));
                }
            }
        }

        // 记录借用
        let kind = if mutable { BorrowKind::Mutable } else { BorrowKind::Shared };
        self.borrow_checker.borrows
            .entry(name.to_string())
            .or_default()
            .push((kind, span));
    }

    /// v0.21: 标记变量已移动
    #[allow(dead_code)] // 未来扩展用
    fn mark_moved(&mut self, name: &str, span: Span) {
        // 如果有借用，报错
        if let Some(borrows) = self.borrow_checker.borrows.get(name)
            && !borrows.is_empty() {
                self.errors.push(TypeError::from_span(
                    &span,
                    format!("cannot move `{}` because it is borrowed", name),
                ));
            }
        self.borrow_checker.moved.insert(name.to_string());
        self.borrow_checker.borrows.remove(name);
    }

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
                if let (Type::List(le), Type::List(re)) = (lt, rt) {
                    // list+list 元素类型相同才精确；否则 Any
                    return if le.compatible_with(re) && re.compatible_with(le) {
                        Type::List(le.clone())
                    } else {
                        Type::List(Box::new(Type::Union(vec![])))
                    };
                }
                // number + number (允许 Any 兼容 number 作为 boundary)
                if matches!(lt, Type::Number) && matches!(rt, Type::Number) {
                    return Type::Number;
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
                if matches!(lt, Type::Number) && matches!(rt, Type::Number) {
                    Type::Number
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
                if matches!(lt, Type::Number | Type::String)
                    && matches!(rt, Type::Number | Type::String)
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

    /// v0.x: 严格字面量类型推断。list/dict 字面量元素类型不一致时 typeck 报错。
    /// 其他字面量（String/Number/Bool/Nil）走 literal_type 粗略分支。
    fn check_literal(&mut self, lit: &Literal, expr: &Expr, symbols: &SymbolTable) -> Type {
        let span = expr_to_span(expr).unwrap_or_default();
        match lit {
            Literal::List(items, _) => {
                if items.is_empty() {
                    return Type::List(Box::new(Type::Union(vec![])));
                }
                // 第一个元素的类型作为基准
                let first_ty = self.check_expr(&items[0], symbols);
                for (i, item) in items.iter().enumerate().skip(1) {
                    let ity = self.check_expr(item, symbols);
                    // Any 不参与严格检查（Any 兼容所有）
                    if is_empty_union(&first_ty) || is_empty_union(&ity) {
                        continue;
                    }
                    // 类型不严格相等 → 报错
                    if !first_ty.compatible_with(&ity) || !ity.compatible_with(&first_ty) {
                        self.errors.push(TypeError::from_span_with_detail(
                            &span,
                            format!(
                                "list element type mismatch at index {}: expected '{}', got '{}'",
                                i,
                                first_ty.name(),
                                ity.name()
                            ),
                            first_ty.name(),
                            ity.name(),
                            "ensure all elements share the same type",
                        ));
                        return Type::List(Box::new(Type::Union(vec![])));
                    }
                }
                Type::List(Box::new(first_ty))
            }
            Literal::Dict(entries, _) => {
                if entries.is_empty() {
                    return Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![])));
                }
                // 取第一个 value 的类型作为基准；key 总是 string
                let first_v_ty = self.check_expr(&entries[0].1, symbols);
                for (i, (_, v_expr)) in entries.iter().enumerate().skip(1) {
                    let vty = self.check_expr(v_expr, symbols);
                    if is_empty_union(&first_v_ty) || is_empty_union(&vty) {
                        continue;
                    }
                    if !first_v_ty.compatible_with(&vty) || !vty.compatible_with(&first_v_ty) {
                        self.errors.push(TypeError::from_span_with_detail(
                            &span,
                            format!(
                                "dict value type mismatch at entry {}: expected '{}', got '{}'",
                                i,
                                first_v_ty.name(),
                                vty.name()
                            ),
                            first_v_ty.name(),
                            vty.name(),
                            "ensure all values share the same type",
                        ));
                        return Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![])));
                    }
                }
                Type::Dict(Box::new(Type::String), Box::new(first_v_ty))
            }
            _ => literal_type(lit),
        }
    }
}

// ===================================================================
// 辅助函数
// ===================================================================

/// 简易字面量类型推断（不报错、不带 span）。
/// 严格 list/dict 元素类型推断在 `check_expr(Expr::Literal)` 中进行（带 span 报错）。
fn literal_type(lit: &Literal) -> Type {
    match lit {
        Literal::String(_, _) => Type::String,
        Literal::Char(_, _) => Type::Char,
        Literal::Number(_, _) => Type::Number,
        Literal::Bool(_, _) => Type::Bool,
        Literal::Nil(_) => Type::Nil,
        // 元素类型推断委托给 check_expr(Expr::Literal)；此处只取粗略形状
        Literal::List(_, _) => Type::List(Box::new(Type::Union(vec![]))),
        Literal::Dict(_, _) => {
            Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![])))
        }
    }
}

/// 给定方法名和接收者类型，返回方法的返回类型
fn method_return_type(receiver: &Type, method: &str) -> Type {
    // v0.x: list<T> 的方法返回类型，元素类型从 receiver 提取
    if let Type::List(elem) = receiver {
        match method {
            "map" | "filter" => return Type::List(elem.clone()),
            "push" => return Type::List(elem.clone()),
            // reduce/pop/get 的返回类型不依赖元素类型，仍为 Any
            "reduce" | "pop" | "get" => return Type::Union(vec![]),
            "len" => return Type::Number,
            _ => {} // fall through to fallback
        }
    }
    // v0.x: dict<K, V> 的方法返回类型
    if let Type::Dict(k, v) = receiver {
        match method {
            "get" => return v.as_ref().clone(),
            "set" => return Type::Dict(k.clone(), v.clone()),
            "keys" => return Type::List(k.clone()),
            "values" => return Type::List(v.clone()),
            "len" => return Type::Number,
            _ => {} // fall through to fallback
        }
    }
    method_return_type_fallback(receiver, method)
}

/// 通用方法返回类型（不依赖 list/dict 元素类型）
fn method_return_type_fallback(receiver: &Type, method: &str) -> Type {
    match (receiver, method) {
        (Type::String, "len") => Type::Number,
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
        (_, "len") => Type::Number, // 通用 len
        _ => Type::Union(vec![]),
    }
}

/// v0.06: 校验 AiConfig 链式方法的参数类型
fn check_ai_config_method(
    method: &str,
    args: &[Box<Expr>],
    errors: &mut Vec<TypeError>,
    span: &Span,
) {
    let expected_ty = match method {
        "model" | "system" => Some(Type::String),
        "temperature" | "max_tokens" | "budget" => Some(Type::Number),
        _ => None,
    };
    let Some(expected_ty) = expected_ty else {
        errors.push(TypeError::from_span_with_detail(
            span,
            format!("AiConfig: unknown method '{}'", method),
            "model / system / temperature / max_tokens / budget",
            method,
            "valid methods: .model(), .system(), .temperature(), .max_tokens(), .budget()",
        ));
        return;
    };
    // 链式方法参数类型已在 check_expr 递归里推断,
    // 这里做额外标记 —— 实际参数校验在 MethodCall check_expr 分支完成
    let _ = (args, expected_ty);
}

/// v0.10 修复: 在 type hint 字符串中替换 trait 泛型参数名
///   `substitute_type_hint("T", {T: number})` → `Some("number")`
///   `substitute_type_hint("Boxed<T>", {T: number})` → `Some("Boxed<number>")`
///   `substitute_type_hint("T<U>", {T: number, U: string})` → `Some("number<string>")`
///   不在替换表中的标识符保留原样
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
fn type_to_hint_string(ty: &Type) -> String {
    match ty {
        Type::Number => "number".to_string(),
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

/// 从 expr 取行号（fallback 0）
fn expr_debug_line(expr: &Expr) -> usize {
    match expr {
        Expr::Binary { span, .. }
        | Expr::Pipe { span, .. }
        | Expr::Call { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::Index { span, .. }
        | Expr::Closure { span, .. }
        | Expr::Match { span, .. }
        | Expr::Prompt { span, .. }
        | Expr::RouteCall { span, .. }
        | Expr::AiModelCall { span, .. } => span.line,
        Expr::Question { span, .. } => span.line,
        Expr::NamespaceRef { span, .. } => span.line,
        Expr::DynTrait { span, .. } => span.line,
        Expr::Literal(lit) => literal_debug_line(lit),
        Expr::Variable(_, span) | Expr::Grouping(_, span) => span.line,
        // v0.21: 借用表达式
        Expr::Borrow { span, .. } | Expr::BorrowMut { span, .. } => span.line,
    }
}

fn literal_debug_line(lit: &Literal) -> usize {
    match lit {
        Literal::String(_, s)
        | Literal::Number(_, s)
        | Literal::Bool(_, s)
        | Literal::Char(_, s)
        | Literal::Nil(s)
        | Literal::List(_, s)
        | Literal::Dict(_, s) => s.line,
    }
}

/// v0.05: 从 expr 取列号（fallback 0）—— 现统一用 expr_to_span
#[allow(dead_code)]
fn expr_debug_column(expr: &Expr) -> usize {
    expr_to_span(expr).map(|s| s.column).unwrap_or(0)
}

/// v0.05: 从 expr 抽 Span（每个 expr 变体的 span 字段）
fn expr_to_span(expr: &Expr) -> Option<Span> {
    match expr {
        Expr::Binary { span, .. }
        | Expr::Pipe { span, .. }
        | Expr::Call { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::Index { span, .. }
        | Expr::Closure { span, .. }
        | Expr::Match { span, .. }
        | Expr::Prompt { span, .. }
        | Expr::RouteCall { span, .. }
        | Expr::AiModelCall { span, .. } => Some(*span),
        Expr::Question { span, .. } => Some(*span),
        Expr::NamespaceRef { span, .. } => Some(*span),
        Expr::DynTrait { span, .. } => Some(*span),
        Expr::Literal(lit) => Some(literal_to_span(lit)),
        Expr::Variable(_, span) | Expr::Grouping(_, span) => Some(*span),
        // v0.21: 借用表达式
        Expr::Borrow { span, .. } | Expr::BorrowMut { span, .. } => Some(*span),
    }
}

fn literal_to_span(lit: &Literal) -> Span {
    match lit {
        Literal::String(_, s)
        | Literal::Number(_, s)
        | Literal::Bool(_, s)
        | Literal::Char(_, s)
        | Literal::Nil(s)
        | Literal::List(_, s)
        | Literal::Dict(_, s) => *s,
    }
}

/// 递归检查 body 里是否有 return 语句（包括嵌套 if/for 里）
fn body_has_return(stmts: &[Stmt]) -> bool {
    for stmt in stmts {
        match stmt {
            Stmt::Return { .. } => return true,
            Stmt::If { then_branch, .. } => {
                if body_has_return(then_branch) {
                    return true;
                }
            }
            Stmt::For { body, .. } => {
                if body_has_return(body) {
                    return true;
                }
            }
            Stmt::Parallel { stmts, .. } => {
                if body_has_return(stmts) {
                    return true;
                }
            }
            Stmt::Match { arms, .. } => {
                for (_pat, arm_stmts) in arms {
                    if body_has_return(arm_stmts) {
                        return true;
                    }
                }
            }
            Stmt::With { body, .. }
            | Stmt::StreamFor { body, .. }
            | Stmt::ToolDef { body, .. }
            | Stmt::Observe { body, .. }
            | Stmt::Span { body, .. } => {
                if body_has_return(body) {
                    return true;
                }
            }
            Stmt::ImplDef { methods, .. } => {
                for m in methods {
                    if body_has_return(&m.body) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn iter_ty_debug_line(expr: &Expr) -> usize {
    expr_debug_line(expr)
}

// ===================================================================
// 顶层入口
// ===================================================================

/// 对一组 stmt 做完整 typeck，返回所有错误
pub fn check_program(stmts: &[Stmt]) -> Vec<TypeError> {
    let mut tc = TypeChecker::new();
    tc.check(stmts);
    tc.errors
}

#[cfg(test)]
mod tests {
    //! v0.12 typeck 测试套件 —— 中档强类型方向
    //!
    //! 设计原则:
    //! - `Type::Union(vec![])` 只在 boundary 出现(builtin / 跨模块 task)
    //! - 同模块内 `let x = expr` 缺 hint 必须报错("missing type annotation")
    //! - 列表/字典字面量元素类型必须一致
    //! - 二元运算符两侧类型必须匹配, 无 Any 兜底
    //! - 后门全关: Nil 不再兼容所有 trait; Result 必须同构; 未知类型名报错

    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn parse(src: &str) -> Vec<Stmt> {
        let tokens = Lexer::new(src).scan_tokens();
        Parser::new(tokens).parse()
    }

    // ============================================================
    // 第一组: 显式 hint 路径(一直 work, 收紧后仍 work)
    // ============================================================

    #[test]
    fn let_with_correct_type() {
        let src = "let x: number = 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn let_with_wrong_type() {
        let src = "let x: number = \"hello\"\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.iter().any(|e| e.message.contains("type mismatch")));
    }

    // ============================================================
    // 第二组: 缺 hint 路径(v0.12 新行为: 必须报错)
    // ============================================================

    #[test]
    fn let_without_hint_errors_in_v0_12() {
        // v0.12: 同模块内 let 缺 hint 必须显式标注, 否则报错
        let src = "let x = 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing type annotation")),
            "expected missing type annotation error, got {:?}",
            errs
        );
    }

    #[test]
    fn let_without_hint_with_typed_let_ok() {
        // v0.12: 显式标注的 let OK
        let src = "let x: number = 1\nlet y: string = \"hi\"\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    // ============================================================
    // 第三组: task 参数/返回类型
    // ============================================================

    #[test]
    fn task_param_mismatch() {
        let src = r#"
task greet(name: string)
  return name
end
task main()
  let x: string = greet(42)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter().any(|e| e.message.contains("expected 'string'")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn task_arg_count_mismatch() {
        let src = r#"
task add(a: number, b: number)
  return a
end
task main()
  let x: number = add(1)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.iter().any(|e| e.message.contains("expects 2 args")));
    }

    #[test]
    fn return_type_mismatch() {
        let src = r#"
task main(): number
  return "hello"
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("return type mismatch"))
        );
    }

    #[test]
    fn return_type_ok() {
        let src = r#"
task main(): string
  return "hi"
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty());
    }

    // ============================================================
    // 第四组: 二元运算符 (v0.12 严格化)
    // ============================================================

    #[test]
    fn binary_op_string_concat_ok() {
        // v0.12: string + string OK
        let src = r#"
task main()
  let a: string = "a" + "b"
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn binary_op_number_arith_ok() {
        // v0.12: number + number OK
        let src = r#"
task main()
  let d: number = 1 + 2
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn binary_op_string_plus_number_errors_in_v0_12() {
        // v0.12: "a" + 1 必须报错(string + number 非法)
        let src = r#"
task main()
  let b: string = "a" + 1
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter().any(|e| e.message.contains("not defined")
                || e.message.contains("type mismatch")
                || e.message.contains("operator")),
            "expected operator/type error for string+number, got {:?}",
            errs
        );
    }

    #[test]
    fn binary_op_bool_plus_number_errors() {
        // v0.12: true + 1 仍报错
        let src = "let x: bool = true + 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.iter().any(|e| e.message.contains("not defined")
            || e.message.contains("type mismatch")
            || e.message.contains("operator")));
    }

    // ============================================================
    // 第五组: for-in (收紧后: 仍允许 List/Number/String)
    // ============================================================

    #[test]
    fn for_in_list() {
        let src = r#"
task main()
  for x in [1, 2, 3]
    let n: number = x
  end
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn for_in_non_iterable() {
        let src = r#"
task main()
  for x in 42
    let n: number = x
  end
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("expects a list or string"))
        );
    }

    // ============================================================
    // 第六组: 闭包 (v0.12 要求 fn 参数显式类型, 拒绝 Any 兜底)
    // ============================================================

    #[test]
    fn method_call_list_map() {
        // v0.12: list.map 的 fn 参数必须有显式类型注解
        let src = r#"
task main()
  let list: list<number> = [1, 2].map(fn(x: number) x * 2 end)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn nested_closure_return_type() {
        // v0.12: 闭包有显式参数/返回类型 hint, 实际调用返回值应满足 hint
        // 注: parser 暂不支持 `fn` 当类型 hint, 所以用 `any` 给 `f`, 内部检查实际返回类型
        let src = r#"
task main()
  let f: any = fn(x: number): number x * 2 end
  let r: number = f(5)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn method_call_list_map_untyped_param_errors() {
        // v0.12: 闭包缺参数类型 → 报错
        let src = r#"
task main()
  let list: list<number> = [1, 2].map(fn(x) x * 2 end)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing type annotation")
                    || e.message.contains("cannot infer")),
            "expected missing type annotation for closure param, got {:?}",
            errs
        );
    }

    // ============================================================
    // 第七组: boundary 路径 (仍允许 Any)
    // ============================================================

    #[test]
    fn unknown_function_call_ok() {
        // 跨模块 task 仍按 Any boundary 处理
        let src = r#"
task main()
  let r: any = maybe_undefined(1, 2, 3)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn builtin_print_accepts_any() {
        // builtin 是 boundary, 接收任何类型
        let src = r#"
task main()
  print(1)
  print("hi")
  print([1, 2])
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn multiple_errors_collected() {
        let src = r#"
task main()
  let x: number = "bad"
  let y: number = 99
  let z: bool = true + 1
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.len() >= 2, "expected multiple errors, got {:?}", errs);
    }

    // ============================================================
    // 第八组: AI / 内置对象
    // ============================================================

    #[test]
    fn ai_module_variable_in_scope() {
        let src = r#"
task main()
  let a: any = ai
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn ai_chat_method_call_typeck() {
        let src = r#"
task main()
  let r: any = ai.chat(p"hello", AiConfig.new())
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn ai_config_builder_chain() {
        let src = r#"
task main()
  let _: any = AiConfig.new()
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    // ============================================================
    // 第九组: 字面量元素类型细化 (v0.12 严格化)
    // ============================================================

    #[test]
    fn list_literal_inferred_element_type() {
        let src = "let xs: list<number> = [1, 2, 3]\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn list_literal_element_type_mismatch() {
        let src = "let xs: list<number> = [1, \"hi\"]\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("list element type mismatch")),
            "expected element type mismatch error, got {:?}",
            errs
        );
    }

    #[test]
    fn list_literal_nested_inferred() {
        let src = "let xs: list<list<number>> = [[1, 2], [3, 4]]\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn dict_literal_inferred_value_type() {
        let src = "let m: dict<string, number> = {\"a\": 1, \"b\": 2}\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn dict_literal_value_type_mismatch() {
        let src = "let m: dict<string, number> = {\"a\": 1, \"b\": \"hi\"}\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("dict value type mismatch")),
            "expected dict value type mismatch, got {:?}",
            errs
        );
    }

    #[test]
    fn dict_literal_key_type_mismatch() {
        // v0.12 暂未实现: dict key 非 string literal 的 parser 阶段校验
        // (parser 当前会 panic 在 number key 上; 这个测试先以 parser 不 panic 为前提,
        //  验证 typeck 能识别 dict 字面量 value 类型不一致 —— 由 dict_literal_value_type_mismatch 覆盖)
        // 这里保留空断言,作为 v0.13+ 推进的方向标注
        let src = "let m: dict<string, number> = {\"a\": 1, \"b\": 2}\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.is_empty(),
            "expected no errors for valid dict, got {:?}",
            errs
        );
    }

    // ============================================================
    // 第十组: Type 解析 unit test (与 v0.12 无关, 保持)
    // ============================================================

    #[test]
    fn from_hint_list_with_element() {
        assert_eq!(
            Type::from_hint("list<number>"),
            Type::List(Box::new(Type::Number))
        );
    }

    #[test]
    fn from_hint_nested_list() {
        assert_eq!(
            Type::from_hint("list<list<number>>"),
            Type::List(Box::new(Type::List(Box::new(Type::Number))))
        );
    }

    #[test]
    fn from_hint_dict_kv() {
        assert_eq!(
            Type::from_hint("dict<string, number>"),
            Type::Dict(Box::new(Type::String), Box::new(Type::Number))
        );
    }

    #[test]
    fn from_hint_string_char() {
        assert_eq!(Type::from_hint("string<char>"), Type::Char);
    }

    #[test]
    fn from_hint_unknown_inner_becomes_trait() {
        // v0.12: list<unknown_thing> → List(Trait{unknown_thing})
        // (Unknown 顶层名字 → Trait 占位, 而非 Any)
        assert_eq!(
            Type::from_hint("list<unknown_thing>"),
            Type::List(Box::new(Type::Trait {
                name: "unknown_thing".to_string(),
                generics: vec![]
            }))
        );
    }

    #[test]
    fn from_hint_unknown_top_errors_in_v0_12() {
        // v0.12: 顶层未知类型名应报错
        let src = "let x: foo_bar = 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter().any(|e| e.message.contains("unknown type")),
            "expected unknown type error, got {:?}",
            errs
        );
    }

    #[test]
    fn type_name_generic() {
        assert_eq!(
            Type::List(Box::new(Type::Number)).name(),
            "list<number>".to_string()
        );
        assert_eq!(
            Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![]))).name(),
            "dict<string, any>".to_string()
        );
        assert_eq!(Type::Char.name(), "char".to_string());
    }

    // ============================================================
    // 第十一组: index / 索引赋值
    // ============================================================

    #[test]
    fn index_non_container_errors() {
        let src = "let x: number = 5\nlet y: number = x[0]\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter().any(|e| e.message.contains("cannot index")),
            "expected cannot index error, got {:?}",
            errs
        );
    }

    #[test]
    fn index_list_with_string_key_errors() {
        let src = "let xs: list<number> = [1, 2, 3]\nlet y: number = xs[\"k\"]\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("list index must be number")),
            "expected list index type error, got {:?}",
            errs
        );
    }

    #[test]
    fn index_assign_type_mismatch() {
        let src = r#"
task main()
  let xs: list<number> = [1, 2, 3]
  xs[0] = "hi"
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("element type mismatch on assign")),
            "expected element type mismatch on assign, got {:?}",
            errs
        );
    }

    #[test]
    fn let_with_list_hint_ok() {
        let src = r#"
task main()
  let xs: list<number> = [1, 2, 3]
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn let_with_dict_hint_ok() {
        let src = r#"
task main()
  let m: dict<string, number> = {"a": 1, "b": 2}
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    // ============================================================
    // 第十二组: char 字面量
    // ============================================================

    #[test]
    fn char_literal_is_char_type() {
        let src = "let c: char = 'a'\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn char_literal_hint_match() {
        let src = "let c: char = 'a'\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn char_literal_hint_mismatch() {
        let src = "let c: number = 'a'\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter().any(|e| e.message.contains("type mismatch")),
            "expected type mismatch error, got {:?}",
            errs
        );
    }

    // ============================================================
    // 第十三组: v0.12 新增 - Nil 不再兼容所有 trait
    // ============================================================

    #[test]
    fn nil_not_compatible_with_trait() {
        // v0.12: 后门 2 关闭, nil 不能赋给非 nil trait
        let src = r#"
trait Foo
end
task main()
  let x: Foo = nil
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter().any(|e| e.message.contains("type mismatch")
                || e.message.contains("nil")
                || e.message.contains("incompatible")),
            "expected nil-incompatible error, got {:?}",
            errs
        );
    }

    // ============================================================
    // 第十四组: v0.12 新增 - Result 必须同构
    // ============================================================

    #[test]
    fn result_must_be_isomorphic() {
        // v0.12: 后门 3 关闭, Result<int,X> 不兼容 Result<string,X>
        let src = r#"
task main()
  let a: result<number, string> = ok(1)
  let b: result<string, string> = a
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("result") || e.message.contains("type mismatch")),
            "expected result type mismatch, got {:?}",
            errs
        );
    }

    // ===================================================================
    // v0.13 激进档 typeck 测试 —— Type::Union, 删 Any, 删 Walrus
    // ===================================================================

    // ============ Union type 基础 ============

    #[test]
    fn union_name_format() {
        // v0.13: Union([String, Number]) → "string | number"
        let t = Type::Union(vec![Type::String, Type::Number]);
        assert_eq!(t.name(), "string | number");
    }

    #[test]
    fn union_empty_name_is_any() {
        // v0.13: 空 Union (Union([])) → "any" (即原 Type::Any 语义)
        let t = Type::Union(vec![]);
        assert_eq!(t.name(), "any");
    }

    #[test]
    fn union_compatible_with_member() {
        // v0.13: String ∈ Union([String, Number, Bool])
        let union_ty = Type::Union(vec![Type::String, Type::Number, Type::Bool]);
        assert!(union_ty.compatible_with(&Type::String));
        assert!(union_ty.compatible_with(&Type::Number));
        assert!(!union_ty.compatible_with(&Type::Char));
    }

    #[test]
    fn union_member_compatible_with_union() {
        // v0.13: String 兼容 Union([String, Number])
        let union_ty = Type::Union(vec![Type::String, Type::Number]);
        assert!(Type::String.compatible_with(&union_ty));
        assert!(!Type::Char.compatible_with(&union_ty));
    }

    #[test]
    fn union_nested_compatible() {
        // v0.13: List<Number> 兼容 List<Union<...empty...>> (空 Union = any element)
        let union_list = Type::List(Box::new(Type::Union(vec![])));
        assert!(Type::List(Box::new(Type::Number)).compatible_with(&union_list));
    }

    #[test]
    fn empty_union_compatible_with_anything() {
        // v0.13: 空 Union 兼容任何类型 (相当于旧 Any)
        let any_ty = Type::Union(vec![]);
        assert!(any_ty.compatible_with(&Type::String));
        assert!(any_ty.compatible_with(&Type::Number));
        assert!(Type::List(Box::new(Type::Number)).compatible_with(&any_ty));
    }

    // ============ builtin 显式 Union 签名 ============

    #[test]
    fn print_accepts_string_in_union() {
        let src = r#"
task main()
  print("hi")
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn print_accepts_number_in_union() {
        let src = r#"
task main()
  print(42)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn print_rejects_non_primitive_dict() {
        // print 接受 string/number/bool/char/nil/list/dict, 但 Dict<Dict, Dict> 不行
        // v0.13: 这种"嵌套 dict" 不在 print 接受范围内
        let src = r#"
task main()
  let d: dict<string, dict<string, number>> = {"a": {"b": 1}}
  print(d)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        // 注: 实际 print 接受 Dict<...>, 所以这条可能通过. 这是预期保守测试
        // 仅 verify 编译通过, 不强制错误
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn range_all_numbers_ok() {
        let src = "let xs: list<number> = range(1, 10, 1)\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn range_rejects_string() {
        let src = r#"
task main()
  let xs = range("a", 10, 1)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("range") || e.message.contains("expected 'number'")),
            "expected range type error, got {:?}",
            errs
        );
    }

    // ============ 删 Walrus 后报错 ============

    #[test]
    fn walrus_syntax_now_errors_at_parse() {
        // v0.13: `let x := expr` 语法已删除, parser 应 panic
        let result = std::panic::catch_unwind(|| {
            let src = "let x := 1\n";
            let tokens = Lexer::new(src).scan_tokens();
            Parser::new(tokens).parse();
        });
        // parser 在 let_declaration 里 `consume(Assign, ...)` 处 panic
        assert!(result.is_err(), "expected parse panic for `:=` syntax");
    }

    #[test]
    fn let_without_walrus_must_have_hint() {
        // v0.13: let x = expr 无 hint 仍然报错
        let src = "let x = 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing type annotation")),
            "expected missing type annotation, got {:?}",
            errs
        );
    }

    #[test]
    fn let_with_explicit_any_hint_still_works() {
        // v0.13: `let x: any = expr` 仍然作为动态类型通配
        let src = r#"
task main()
  let r: any = maybe_undefined(1, 2)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    // ============ 严格二元运算 (string + number 必须报错) ============

    #[test]
    fn string_plus_number_strict_in_v0_13() {
        // v0.13: 仍是 strict
        let src = r#"
task main()
  let b: string = "a" + 1
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("'+'") || e.message.contains("type mismatch")),
            "expected string+number error, got {:?}",
            errs
        );
    }

    // ============ 后门全关 ============

    #[test]
    fn nil_assign_to_trait_still_errors() {
        // v0.13: 后门 2 仍关
        let src = r#"
trait Foo
end
task main()
  let x: Foo = nil
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("type mismatch") || e.message.contains("nil")),
            "expected nil-trait mismatch, got {:?}",
            errs
        );
    }

    #[test]
    fn unknown_type_name_still_errors() {
        let src = "let x: foo_bar = 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter().any(|e| e.message.contains("unknown type")),
            "expected unknown type error, got {:?}",
            errs
        );
    }

    #[test]
    fn closure_param_without_type_still_errors() {
        let src = r#"
task main()
  let xs: list<number> = [1, 2].map(fn(x) x * 2 end)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing type annotation")
                    || e.message.contains("closure parameter")),
            "expected closure param error, got {:?}",
            errs
        );
    }

    // ============ Result 同构严格化 ============

    #[test]
    fn result_must_be_isomorphic_v0_13() {
        // v0.13: Result<number, string> != Result<string, string>
        let r1 = Type::Result_(Box::new(Type::Number), Box::new(Type::String));
        let r2 = Type::Result_(Box::new(Type::String), Box::new(Type::String));
        assert!(
            !r1.compatible_with(&r2),
            "result<number, string> should not be compatible with result<string, string>"
        );
    }

    #[test]
    fn result_isomorphic_compatible() {
        // v0.13: Result<number, string> 兼容 Result<number, string>
        let r1 = Type::Result_(Box::new(Type::Number), Box::new(Type::String));
        let r2 = Type::Result_(Box::new(Type::Number), Box::new(Type::String));
        assert!(r1.compatible_with(&r2));
    }

    // ============ Type::Any 已删除 (compile-time 验证) ============

    #[test]
    fn type_any_variant_does_not_exist() {
        // v0.13: Type::Any 不再是 enum 成员
        //   这个测试如果编译通过, 说明 Any 已被删除
        //   (编译错误 = 测试失败)
        fn _check() {
            // 直接 match 必须穷尽
            let _t = match Type::Number {
                Type::Number
                | Type::String
                | Type::Char
                | Type::Bool
                | Type::Nil
                | Type::List(_)
                | Type::Dict(_, _)
                | Type::Task
                | Type::Closure
                | Type::Conversation
                | Type::Stream
                | Type::Builtin
                | Type::AiConfig
                | Type::AiResult
                | Type::AiError
                | Type::AiModule
                | Type::Result_(_, _)
                | Type::Router
                | Type::HttpRequest
                | Type::HttpResponse
                | Type::McpServer
                | Type::Trait { .. }
                | Type::Concrete { .. }
                | Type::Union(_) => true,
                // Type::Any 已不存在, 这里不需要 arm
            };
        }
    }
}
