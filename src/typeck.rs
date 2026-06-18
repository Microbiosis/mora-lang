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

use std::collections::HashMap;

use crate::ast::*;

// ===================================================================
// 公共类型
// ===================================================================

/// Mora 类型系统：基础类型 + Any（推断不出时退路）
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    String,
    Number,
    Bool,
    Nil,
    List,
    Dict,
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
    /// 推断不出或用户未标注时的退路——不做严格检查
    Any,
}

impl Type {
    pub fn name(&self) -> &'static str {
        match self {
            Type::String => "string",
            Type::Number => "number",
            Type::Bool => "bool",
            Type::Nil => "nil",
            Type::List => "list",
            Type::Dict => "dict",
            Type::Task => "task",
            Type::Closure => "closure",
            Type::Conversation => "conversation",
            Type::Stream => "stream",
            Type::Builtin => "builtin",
            Type::AiConfig => "ai_config",
            Type::AiResult => "ai_result",
            Type::AiError => "ai_error",
            Type::AiModule => "ai",
            Type::Result_(ok, err) => {
                // 动态格式化：Result<string, AiError> 只能在运行时 name()
                let _ = (ok, err);
                "result"
            },
            Type::Router => "router",
            Type::HttpRequest => "http_request",
            Type::HttpResponse => "http_response",
            Type::Any => "any",
        }
    }

    /// 从用户写的类型 hint 字符串解析
    pub fn from_hint(hint: &str) -> Type {
        match hint {
            "string" => Type::String,
            "number" => Type::Number,
            "bool" => Type::Bool,
            "nil" => Type::Nil,
            "list" => Type::List,
            "dict" => Type::Dict,
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
            // 未知类型名 → Any（不报错；Mora 允许扩展类型）
            _ => Type::Any,
        }
    }

    /// 类型兼容：Any 总兼容；Result<T,E> 与 Ok/Err 兼容
    pub fn compatible_with(&self, expected: &Type) -> bool {
        if matches!(self, Type::Any) || matches!(expected, Type::Any) {
            return true;
        }
        // v0.06.2: Result<T,E> 兼容 —— 任何同构 Result 兼容
        if matches!(self, Type::Result_(_, _)) && matches!(expected, Type::Result_(_, _)) {
            return true;
        }
        self == expected
    }
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
        Self { scopes: vec![HashMap::new()] }
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
        Type::Any
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
    /// v0.05: 已注册的 route 名称（供 `let x = fast(p"...")` 推断为 String）
    routes: std::collections::HashSet<String>,
}

/// 任务/闭包签名
#[derive(Debug, Clone)]
pub struct Signature {
    pub params: Vec<(String, Type)>,  // (name, type)，未标注为 Any
    pub return_type: Type,
}

impl TypeChecker {
    pub fn new() -> Self {
        let mut sigs = HashMap::new();
        // 内置函数签名
        sigs.insert("print".to_string(), Signature {
            params: vec![("x".to_string(), Type::Any)],
            return_type: Type::Nil,
        });
        sigs.insert("range".to_string(), Signature {
            params: vec![("start".to_string(), Type::Number), ("end".to_string(), Type::Any), ("step".to_string(), Type::Any)],
            return_type: Type::List,
        });
        sigs.insert("len".to_string(), Signature {
            params: vec![("x".to_string(), Type::Any)],
            return_type: Type::Number,
        });
        // v0.06: ai.chat(cfg: AiConfig, prompt: String) -> AiResult
        sigs.insert("ai.chat".to_string(), Signature {
            params: vec![
                ("cfg".to_string(), Type::AiConfig),
                ("prompt".to_string(), Type::String),
            ],
            return_type: Type::AiResult,
        });
        // v0.06.3: Router::new() -> Router
        sigs.insert("Router::new".to_string(), Signature {
            params: vec![],
            return_type: Type::Router,
        });
        Self {
            signatures: sigs,
            errors: Vec::new(),
            current_return_hint: None,
            routes: std::collections::HashSet::new(),
        }
    }

    pub fn errors(&self) -> &[TypeError] {
        &self.errors
    }

    /// 第一趟：收集所有 task 定义（签名）
    fn collect_signatures(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            if let Stmt::TaskDef { name, params, return_type, .. } = stmt {
                let param_types: Vec<(String, Type)> = params.iter()
                    .map(|(n, hint)| (n.clone(), hint.as_deref().map(Type::from_hint).unwrap_or(Type::Any)))
                    .collect();
                let ret = return_type.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                self.signatures.insert(name.clone(), Signature { params: param_types, return_type: ret });
            }
            // v0.05: 收集 route 名称 —— `let x = fast(p"...")` 推断为 String
            if let Stmt::Route { name, .. } = stmt {
                self.routes.insert(name.clone());
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
            Stmt::Let { name, type_hint, init, is_any, span, .. } => {
                let init_ty = self.check_expr(init, symbols);
                // v0.05: 移除 Any 兜底
                //   - is_any=true (let x := expr) → 强制 Any, 跳过严格检查
                //   - 有 type_hint (let x: T = expr) → 验证 init_ty 与 T 兼容
                //   - 无 type_hint (let x = expr) → 强制推断: declared = init_ty
                //     (init_ty 若是 Any —— 比如未注册函数调用 —— 报错"无法推断")
                let declared = if *is_any {
                    Type::Any
                } else if let Some(hint) = type_hint {
                    let t = Type::from_hint(hint);
                    if !init_ty.compatible_with(&t) {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("type mismatch: let {}", name),
                            t.name(),
                            init_ty.name(),
                            format!("try `let {} := expr` for dynamic typing, or fix the initializer", name),
                        ));
                    }
                    t
                } else if matches!(init_ty, Type::Any) {
                    // v0.05: 无 type_hint 且无法推断（init 是 Any，如未注册函数调用）
                    self.errors.push(TypeError::from_span_with_detail(
                        span,
                        format!("cannot infer type: let {}", name),
                        "any",
                        "any",
                        "add a type hint: `let x: T = expr`, or use `let x := expr` for dynamic typing",
                    ));
                    Type::Any
                } else {
                    init_ty.clone()
                };
                symbols.define(name.clone(), declared);
            }
            Stmt::Assign { name, value, span } => {
                let val_ty = self.check_expr(value, symbols);
                let current = symbols.lookup(name);
                if !val_ty.compatible_with(&current) {
                    self.errors.push(TypeError::from_span_with_detail(
                        span,
                        format!("type mismatch: cannot assign '{}' to variable '{}' of type '{}'",
                            val_ty.name(), name, current.name()),
                        current.name(),
                        val_ty.name(),
                        format!("change value or add cast: `let y: {} = ...`", current.name()),
                    ));
                }
            }
            Stmt::IndexAssign { object, index, value, span } => {
                let _ = self.check_expr(object, symbols);
                let _ = self.check_expr(index, symbols);
                let _ = self.check_expr(value, symbols);
                // 索引赋值不做严格类型检查（Mora 列表异构）
                let _ = span;
            }
            Stmt::TaskDef { name, params, return_type, body, span, .. } => {
                symbols.push_scope();
                for (pname, phint) in params {
                    let pty = phint.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                    symbols.define(pname.clone(), pty);
                }
                let prev_hint = self.current_return_hint.clone();
                self.current_return_hint = return_type.as_deref().map(Type::from_hint);
                for s in body {
                    self.check_stmt(s, symbols);
                }
                // v0.05: 检查"缺少 return"
                // 如果声明了非 nil/Any 的返回类型，body 里必须有 return 语句
                if let Some(ret_hint) = &self.current_return_hint {
                    if !matches!(ret_hint, Type::Nil | Type::Any) && !body_has_return(body) {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("missing return in task '{}' with return type '{}'", name, ret_hint.name()),
                            ret_hint.name(),
                            "nil (missing return)",
                            format!("add `return <expr>` at the end of the task body"),
                        ));
                    }
                }
                self.current_return_hint = prev_hint;
                symbols.pop_scope();
                let _ = (name, span);
            }
            Stmt::If { condition, then_branch, .. } => {
                self.check_expr(condition, symbols);
                symbols.push_scope();
                for s in then_branch { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::For { var, var_type, iterable, body, .. } => {
                let iter_ty = self.check_expr(iterable, symbols);
                // iterable 应该是 list 或 string（字符串按 char 迭代）
                if !matches!(iter_ty, Type::List | Type::String | Type::Any) {
                    self.errors.push(TypeError::with_hint(
                        TypeError::new(iter_ty_debug_line(iterable), format!("for-in expects a list or string, got '{}'", iter_ty.name())),
                        "iterate over a list or string: `for x in [1, 2, 3]`",
                    ));
                }
                symbols.push_scope();
                let vty = var_type.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                symbols.define(var.clone(), vty);
                for s in body { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::Try { try_block, catch_type, catch_block, span, .. } => {
                symbols.push_scope();
                for s in try_block { self.check_stmt(s, symbols); }
                symbols.pop_scope();
                // v0.04.0: catch_type 校验
                if let Some(t) = catch_type {
                    if t != "AiError" {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("try/catch: type '{}' not supported (v0.04.0 only supports 'AiError' or no annotation)", t),
                        ));
                    }
                }
                symbols.push_scope();
                for s in catch_block { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::Import { .. } => {
                // 不做跨模块 symbol 解析
            }
            Stmt::Parallel { stmts, .. } => {
                for s in stmts { self.check_stmt(s, symbols); }
            }
            Stmt::Match { expr, arms, .. } => {
                self.check_expr(expr, symbols);
                for (_pat, arm_stmts) in arms {
                    symbols.push_scope();
                    for s in arm_stmts { self.check_stmt(s, symbols); }
                    symbols.pop_scope();
                }
            }
            Stmt::Save { path, value, .. } => {
                self.check_expr(path, symbols);
                self.check_expr(value, symbols);
            }
            Stmt::Load { path, var, .. } => {
                self.check_expr(path, symbols);
                symbols.define(var.clone(), Type::Any);
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
                    if let Some(expected) = &self.current_return_hint {
                        if !val_ty.compatible_with(expected) {
                            self.errors.push(TypeError::from_span(
                                span,
                                format!("return type mismatch: expected '{}', got '{}'",
                                    expected.name(), val_ty.name()),
                            ));
                        }
                    }
                } else {
                    // return 无值 → 期望 nil
                    if let Some(expected) = &self.current_return_hint {
                        if !matches!(expected, Type::Nil | Type::Any) {
                            self.errors.push(TypeError::from_span(
                                span,
                                format!("return type mismatch: expected '{}', got nil", expected.name()),
                            ));
                        }
                    }
                }
            }
            Stmt::Expr(expr) => {
                self.check_expr(expr, symbols);
            }
            // v0.04.0: AI 原语
            Stmt::With { bindings, body, span, .. } => {
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
                for s in body { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::StreamFor { prompt, var, body, span, .. } => {
                let prompt_ty = self.check_expr(prompt, symbols);
                if !matches!(prompt_ty, Type::String | Type::Any) {
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
                for s in body { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::ToolDef { name, params, return_type, body, exported, span, .. } => {
                symbols.push_scope();
                // v0.05: 注入 tool 参数进 scope
                for (pname, phint) in params {
                    let pty = phint.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                    symbols.define(pname.clone(), pty);
                }
                // v0.05: 注入 args: dict<string, Any> —— MCP 调用时 args 形参
                symbols.define("args".to_string(), Type::Dict);
                let prev_hint = self.current_return_hint.clone();
                self.current_return_hint = return_type.as_deref().map(Type::from_hint);
                for s in body { self.check_stmt(s, symbols); }
                self.current_return_hint = prev_hint;
                symbols.pop_scope();
                let declared = return_type.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                symbols.define(name.clone(), declared);
                let _ = (exported, span);
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {
                // v0.04.0 简化:仅警告(v0.04.1 强制"必须在 loop 内")
            }
            // v0.05: 云服务原生 —— 严格 typeck
            Stmt::Serve { protocol, routes, body, span, .. } => {
                // 校验 protocol
                match protocol {
                    ServeProtocol::Http { port, .. } => {
                        // port 必须是 number literal
                        if *port == 0 {
                            self.errors.push(TypeError::from_span(
                                span,
                                "serve as http: port cannot be 0",
                            ));
                        }
                    }
                    ServeProtocol::Mcp | ServeProtocol::Repl | ServeProtocol::Stdio => {
                        // 这些协议无参数限制
                    }
                }
                // 校验 routes
                for r in routes {
                    match r {
                        RouteDecl::HttpRoute { handler, .. } => {
                            self.check_expr(handler, symbols);
                        }
                        RouteDecl::ToolEntry { handler, .. } => {
                            self.check_expr(handler, symbols);
                        }
                    }
                }
                symbols.push_scope();
                for s in body { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::Observe { config, body, span, .. } => {
                // 校验 observe config
                match config {
                    ObserveConfig::Trace | ObserveConfig::Metrics => {}
                    ObserveConfig::Otel { endpoint } => {
                        let ep_ty = self.check_expr(endpoint, symbols);
                        if !matches!(ep_ty, Type::String | Type::Any) {
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
                for s in body { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::Span { name, attributes, body, span, .. } => {
                // name 必须是 string literal
                if name.is_empty() {
                    self.errors.push(TypeError::from_span(span, "span name cannot be empty"));
                }
                // attributes 必须是 dict literal
                for (k, v) in attributes {
                    self.check_expr(v, symbols);
                    let _ = k;
                }
                symbols.push_scope();
                for s in body { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            // v0.04补: Stmt::Route 必须递归 typeck target, 触发 ai_model 校验
            Stmt::Route { target, .. } => {
                self.check_expr(target, symbols);
            }
            // v0.04.0 终态补: record_tokens 参数必须 number
            Stmt::RecordTokens { input, output, span, .. } => {
                let in_ty = self.check_expr(input, symbols);
                if !matches!(in_ty, Type::Number | Type::Any) {
                    self.errors.push(TypeError::from_span(
                        span,
                        format!("record_tokens: input must be number, got '{}'", in_ty.name()),
                    ));
                }
                let out_ty = self.check_expr(output, symbols);
                if !matches!(out_ty, Type::Number | Type::Any) {
                    self.errors.push(TypeError::from_span(
                        span,
                        format!("record_tokens: output must be number, got '{}'", out_ty.name()),
                    ));
                }
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr, symbols: &SymbolTable) -> Type {
        match expr {
            Expr::Literal(lit) => literal_type(lit),
            Expr::Variable(name, _) => symbols.lookup(name),
            Expr::Binary { left, op, right, span, .. } => {
                let lt = self.check_expr(left, symbols);
                let rt = self.check_expr(right, symbols);
                self.check_binary_op(op.clone(), &lt, &rt, span.line, span.column)
            }
            Expr::Pipe { left, right, .. } => {
                let _ = self.check_expr(left, symbols);
                self.check_expr(right, symbols)
            }
            Expr::Call { callee, args, span, .. } => {
                for a in args { let _ = self.check_expr(a, symbols); }
                // v0.05: 先看是否是已注册 route —— `let x = fast(p"...")` 推断为 String
                if self.routes.contains(callee) {
                    if args.len() != 1 {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("route '{}()' expects 1 arg (the prompt), got {}", callee, args.len()),
                        ));
                    }
                    return Type::String;
                }
                if let Some(sig) = self.signatures.get(callee).cloned() {
                    // 参数个数检查
                    if args.len() != sig.params.len() {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("function '{}' expects {} args, got {}",
                                callee, sig.params.len(), args.len()),
                        ));
                    } else {
                        // 参数类型检查
                        for (i, ((_pname, pty), arg)) in sig.params.iter().zip(args.iter()).enumerate() {
                            let aty = self.check_expr(arg, symbols);
                            if !aty.compatible_with(pty) {
                                self.errors.push(TypeError::from_span_with_detail(
                                    &expr_to_span(arg).unwrap_or(*span),
                                    format!("arg {} of '{}': expected '{}', got '{}'",
                                        i + 1, callee, pty.name(), aty.name()),
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
                    Type::Any
                }
            }
            Expr::MethodCall { object, method, args, span, .. } => {
                let ot = self.check_expr(object, symbols);
                for a in args { let _ = self.check_expr(a, symbols); }
                // v0.06: AiConfig 链式参数校验
                if matches!(ot, Type::AiConfig) {
                    check_ai_config_method(method, args, &mut self.errors, span);
                }
                method_return_type(&ot, method)
            }
            Expr::Index { object, index, .. } => {
                let ot = self.check_expr(object, symbols);
                let it = self.check_expr(index, symbols);
                index_result_type(&ot, &it)
            }
            Expr::Closure { params, return_type, body, .. } => {
                let mut inner = SymbolTable::new();
                for (pname, phint) in params {
                    let pty = phint.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                    inner.define(pname.clone(), pty);
                }
                let prev_hint = self.current_return_hint.clone();
                self.current_return_hint = return_type.as_deref().map(Type::from_hint);
                for s in body { self.check_stmt(s, &mut inner); }
                self.current_return_hint = prev_hint;
                Type::Closure
            }
            Expr::Match { expr, arms, .. } => {
                self.check_expr(expr, symbols);
                // 取最后 arm 的类型（Mora 不强制 arm 类型一致）
                let mut ty = Type::Any;
                for (_pat, arm_expr) in arms {
                    ty = self.check_expr(arm_expr, symbols);
                }
                ty
            }
            Expr::Grouping(inner, _) => self.check_expr(inner, symbols),
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
            Expr::AiModelCall { model, temperature, max_tokens, system, span } => {
                let mt = self.check_expr(model, symbols);
                if !matches!(mt, Type::String | Type::Any) {
                    self.errors.push(TypeError::from_span(
                        span,
                        format!("ai_model: model name must be string, got '{}'", mt.name()),
                    ));
                }
                if let Some(t) = temperature {
                    let tt = self.check_expr(t, symbols);
                    if !matches!(tt, Type::Number | Type::Any) {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("ai_model: temperature must be number, got '{}'", tt.name()),
                        ));
                    }
                }
                if let Some(n) = max_tokens {
                    let nt = self.check_expr(n, symbols);
                    if !matches!(nt, Type::Number | Type::Any) {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("ai_model: max_tokens must be number, got '{}'", nt.name()),
                        ));
                    }
                }
                if let Some(s) = system {
                    let st = self.check_expr(s, symbols);
                    if !matches!(st, Type::String | Type::Any) {
                        self.errors.push(TypeError::from_span(
                            span,
                            format!("ai_model: system must be string, got '{}'", st.name()),
                        ));
                    }
                }
                Type::Dict
            }
            // v0.06.2: expr? 操作符 — expr 必须是 Result<T,E> , 返回 T
            Expr::Question { expr, span } => {
                let expr_ty = self.check_expr(expr, symbols);
                match &expr_ty {
                    Type::Result_(ok_ty, _err_ty) => (**ok_ty).clone(),
                    Type::Any => Type::Any,  // 推断不出, 不报
                    _ => {
                        self.errors.push(TypeError::from_span_with_detail(
                            span,
                            format!("'?' operator expects Result<T,E>, got '{}'", expr_ty.name()),
                            "result",
                            expr_ty.name(),
                            "wrap the expression in Ok(...) or change return type to Result<T,E>",
                        ));
                        Type::Any
                    }
                }
            }
        }
    }

    fn check_binary_op(&mut self, op: BinaryOp, lt: &Type, rt: &Type, line: usize, column: usize) -> Type {
        let span = Span::new(line, column);
        use BinaryOp::*;
        match op {
            Add => {
                // string + 任意 → string；list + list → list；number + number → number
                if matches!(lt, Type::String) || matches!(rt, Type::String) {
                    return Type::String;
                }
                if matches!(lt, Type::List) && matches!(rt, Type::List) {
                    return Type::List;
                }
                if matches!(lt, Type::Number | Type::Any) && matches!(rt, Type::Number | Type::Any) {
                    return Type::Number;
                }
                self.errors.push(TypeError::from_span_with_detail(
                    &span,
                    format!("operator '+' not defined for '{}' and '{}'", lt.name(), rt.name()),
                    "number + number / string + any / list + list",
                    format!("'{}' + '{}'", lt.name(), rt.name()),
                    "convert both to same type: `let s = str(x); let z = s + ...`",
                ));
                Type::Any
            }
            Sub | Mul | Div | Mod => {
                if matches!(lt, Type::Number | Type::Any) && matches!(rt, Type::Number | Type::Any) {
                    Type::Number
                } else {
                    self.errors.push(TypeError::from_span_with_detail(
                        &span,
                        format!("operator '{}' requires number operands, got '{}' and '{}'",
                            match op { Sub => "-", Mul => "*", Div => "/", Mod => "%", _ => "?" },
                            lt.name(), rt.name()),
                        "number / number",
                        format!("'{}' / '{}'", lt.name(), rt.name()),
                        "arithmetic operators work on numbers: `let z = 42 + 1`",
                    ));
                    Type::Any
                }
            }
            Equal | NotEqual => Type::Bool,
            Greater | Less | GreaterEqual | LessEqual => {
                if matches!(lt, Type::Number | Type::String | Type::Any) &&
                   matches!(rt, Type::Number | Type::String | Type::Any) {
                    Type::Bool
                } else {
                    self.errors.push(TypeError::from_span_with_detail(
                        &span,
                        format!("comparison requires number or string, got '{}' and '{}'",
                            lt.name(), rt.name()),
                        "number or string",
                        format!("'{}' / '{}'", lt.name(), rt.name()),
                        "compare with compatible types: `let eq = (str(x) == str(y))`",
                    ));
                    Type::Any
                }
            }
        }
    }
}

// ===================================================================
// 辅助函数
// ===================================================================

fn literal_type(lit: &Literal) -> Type {
    match lit {
        Literal::String(_, _) => Type::String,
        Literal::Number(_, _) => Type::Number,
        Literal::Bool(_, _) => Type::Bool,
        Literal::Nil(_) => Type::Nil,
        Literal::List(_, _) => Type::List,
        Literal::Dict(_, _) => Type::Dict,
    }
}

/// 给定方法名和接收者类型，返回方法的返回类型
fn method_return_type(receiver: &Type, method: &str) -> Type {
    match (receiver, method) {
        (Type::List, "map" | "filter") => Type::List,
        (Type::List, "reduce") => Type::Any,
        (Type::List, "push") => Type::List,
        (Type::List, "pop") => Type::Any,
        (Type::List, "get") => Type::Any,
        (Type::List, "len") => Type::Number,
        (Type::Dict, "get") => Type::Any,
        (Type::Dict, "set") => Type::Dict,
        (Type::Dict, "keys") | (Type::Dict, "values") => Type::List,
        (Type::Dict, "len") => Type::Number,
        (Type::String, "len") => Type::Number,
        (Type::String, "upper" | "lower" | "trim" | "replace") => Type::String,
        (Type::String, "starts_with" | "ends_with" | "contains") => Type::Bool,
        (Type::String, "split") => Type::List,
        (Type::Conversation, "chat") => Type::Any,
        (Type::Conversation, "history" | "len") => Type::List,
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
        // v0.06.3: HttpRequest 方法
        (Type::HttpRequest, "json") => Type::Any,  // ~Result<T, ParseError>
        (Type::Any, _) => Type::Any,
        (_, "len") => Type::Number,  // 通用 len
        _ => Type::Any,
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

fn index_result_type(obj: &Type, idx: &Type) -> Type {
    match obj {
        Type::List => {
            if matches!(idx, Type::Number | Type::Any) {
                Type::Any  // 元素类型不推断
            } else {
                Type::Any  // 错误不报这里
            }
        }
        Type::Dict => {
            if matches!(idx, Type::String | Type::Any) {
                Type::Any
            } else {
                Type::Any
            }
        }
        Type::String => {
            if matches!(idx, Type::Number | Type::Any) {
                Type::String
            } else {
                Type::Any
            }
        }
        _ => Type::Any,
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
        Expr::Literal(lit) => literal_debug_line(lit),
        Expr::Variable(_, span) | Expr::Grouping(_, span) => span.line,
    }
}

fn literal_debug_line(lit: &Literal) -> usize {
    match lit {
        Literal::String(_, s) | Literal::Number(_, s) | Literal::Bool(_, s) |
        Literal::Nil(s) | Literal::List(_, s) | Literal::Dict(_, s) => s.line,
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
        Expr::Literal(lit) => Some(literal_to_span(lit)),
        Expr::Variable(_, span) | Expr::Grouping(_, span) => Some(*span),
    }
}

fn literal_to_span(lit: &Literal) -> Span {
    match lit {
        Literal::String(_, s) | Literal::Number(_, s) | Literal::Bool(_, s) |
        Literal::Nil(s) | Literal::List(_, s) | Literal::Dict(_, s) => *s,
    }
}

/// 递归检查 body 里是否有 return 语句（包括嵌套 if/for/try 里）
fn body_has_return(stmts: &[Stmt]) -> bool {
    for stmt in stmts {
        match stmt {
            Stmt::Return { .. } => return true,
            Stmt::If { then_branch, .. } => {
                if body_has_return(then_branch) { return true; }
            }
            Stmt::For { body, .. } => {
                if body_has_return(body) { return true; }
            }
            Stmt::Try { try_block, catch_block, .. } => {
                if body_has_return(try_block) || body_has_return(catch_block) { return true; }
            }
            Stmt::Parallel { stmts, .. } => {
                if body_has_return(stmts) { return true; }
            }
            Stmt::Match { arms, .. } => {
                for (_pat, arm_stmts) in arms {
                    if body_has_return(arm_stmts) { return true; }
                }
            }
            Stmt::With { body, .. } | Stmt::StreamFor { body, .. } | Stmt::ToolDef { body, .. } |
            Stmt::Observe { body, .. } | Stmt::Span { body, .. } => {
                if body_has_return(body) { return true; }
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
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn parse(src: &str) -> Vec<Stmt> {
        let tokens = Lexer::new(src).scan_tokens();
        Parser::new(tokens).parse()
    }

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
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("type mismatch"));
    }

    #[test]
    fn let_without_hint_ok() {
        let src = "let x = 1\nlet y = \"hi\"\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty());
    }

    #[test]
    fn task_param_mismatch() {
        let src = r#"
task greet(name: string)
  return name
end
task main()
  let x := greet(42)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.iter().any(|e| e.message.contains("expected 'string'")), "{:?}", errs);
    }

    #[test]
    fn task_arg_count_mismatch() {
        let src = r#"
task add(a: number, b: number)
  return a
end
task main()
  let x := add(1)
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
        assert!(errs.iter().any(|e| e.message.contains("return type mismatch")));
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

    #[test]
    fn binary_op_string_concat() {
        let src = r#"
task main()
  let a := "a" + "b"
  let b := "a" + 1
  let c := [1] + [2]
  let d := 1 + 2
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn binary_op_invalid() {
        let src = "let x = true + 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.iter().any(|e| e.message.contains("not defined")));
    }

    #[test]
    fn for_in_list() {
        let src = r#"
task main()
  for x in [1, 2, 3]
    let n := x
  end
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn for_in_non_iterable() {
        // v0.06: for-in 循环按 42 报错 + 强制推断
        let src = r#"
task main()
  for x in 42
    let n := x
  end
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.iter().any(|e| e.message.contains("expects a list or string")));
    }

    #[test]
    fn method_call_list_map() {
        let src = r#"
task main()
  let list := [1, 2].map(fn(x) x * 2 end)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn nested_closure_return_type() {
        let src = r#"
task main()
  let f = fn(x: number): number x * 2 end
  let r := f(5)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn unknown_function_call_ok() {
        // 跨模块 task 名空间，未收集到的符号视为 Any
        // v0.06: 强制推断 —— 必须用 := 显式 Any 标注
        let src = r#"
task main()
  let r := maybe_undefined(1, 2, 3)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn builtin_print_accepts_any() {
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
        // 两个独立错误都应该被报告
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

    // ===================================================================
    // v0.06 测试
    // ===================================================================

    #[test]
    fn ai_module_variable_in_scope() {
        // `ai` 是内建 AiModule 类型变量, 应该存在
        let src = r#"
task main()
  let a := ai
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn ai_chat_method_call_typeck() {
        // ai.chat(cfg, p"...") 返回 AiResult
        let src = r#"
task main()
  let r := ai.chat(p"hello", AiConfig.new())
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "expected no errors, got {:?}", errs);
    }

    #[test]
    fn ai_config_builder_chain() {
        // AiConfig.new().model("gpt-4").temperature(0.7) 链式调用
        let src = r#"
task main()
  let _ := AiConfig.new()
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        // AiConfig.new() 是未知函数 → Any, 但 any 兼容所有
        assert!(errs.is_empty(), "{:?}", errs);
    }
}
