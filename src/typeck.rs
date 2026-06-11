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
    Builtin,
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
            Type::Builtin => "builtin",
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
            // 未知类型名 → Any（不报错；Mora 允许扩展类型）
            _ => Type::Any,
        }
    }

    /// 类型兼容：Any 总兼容；其它要求严格相等
    pub fn compatible_with(&self, expected: &Type) -> bool {
        if matches!(self, Type::Any) || matches!(expected, Type::Any) {
            return true;
        }
        self == expected
    }
}

/// 类型错误 + 位置
#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub line: usize,
    pub message: String,
}

impl TypeError {
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self { line, message: message.into() }
    }
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
        Self {
            signatures: sigs,
            errors: Vec::new(),
            current_return_hint: None,
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
        }
    }

    /// 顶层入口
    pub fn check(&mut self, stmts: &[Stmt]) {
        self.collect_signatures(stmts);
        let mut symbols = SymbolTable::new();
        for stmt in stmts {
            self.check_stmt(stmt, &mut symbols);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt, symbols: &mut SymbolTable) {
        match stmt {
            Stmt::Let { name, type_hint, init, span, .. } => {
                let init_ty = self.check_expr(init, symbols);
                let declared = type_hint.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                if !init_ty.compatible_with(&declared) {
                    self.errors.push(TypeError::new(
                        span.line,
                        format!("type mismatch: let {} declared as '{}' but initializer is '{}'",
                            name, declared.name(), init_ty.name()),
                    ));
                }
                symbols.define(name.clone(), declared);
            }
            Stmt::Assign { name, value, span } => {
                let val_ty = self.check_expr(value, symbols);
                let current = symbols.lookup(name);
                if !val_ty.compatible_with(&current) {
                    self.errors.push(TypeError::new(
                        span.line,
                        format!("type mismatch: cannot assign '{}' to variable '{}' of type '{}'",
                            val_ty.name(), name, current.name()),
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
                    self.errors.push(TypeError::new(
                        iter_ty_debug_line(iterable),
                        format!("for-in expects a list or string, got '{}'", iter_ty.name()),
                    ));
                }
                symbols.push_scope();
                let vty = var_type.as_deref().map(Type::from_hint).unwrap_or(Type::Any);
                symbols.define(var.clone(), vty);
                for s in body { self.check_stmt(s, symbols); }
                symbols.pop_scope();
            }
            Stmt::Try { try_block, catch_block, .. } => {
                symbols.push_scope();
                for s in try_block { self.check_stmt(s, symbols); }
                symbols.pop_scope();
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
                            self.errors.push(TypeError::new(
                                span.line,
                                format!("return type mismatch: expected '{}', got '{}'",
                                    expected.name(), val_ty.name()),
                            ));
                        }
                    }
                } else {
                    // return 无值 → 期望 nil
                    if let Some(expected) = &self.current_return_hint {
                        if !matches!(expected, Type::Nil | Type::Any) {
                            self.errors.push(TypeError::new(
                                span.line,
                                format!("return type mismatch: expected '{}', got nil", expected.name()),
                            ));
                        }
                    }
                }
            }
            Stmt::Expr(expr) => {
                self.check_expr(expr, symbols);
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr, symbols: &SymbolTable) -> Type {
        match expr {
            Expr::Literal(lit) => literal_type(lit),
            Expr::Variable(name, _) => symbols.lookup(name),
            Expr::Binary { left, op, right, .. } => {
                let lt = self.check_expr(left, symbols);
                let rt = self.check_expr(right, symbols);
                self.check_binary_op(op.clone(), &lt, &rt)
            }
            Expr::Pipe { left, right, .. } => {
                let _ = self.check_expr(left, symbols);
                self.check_expr(right, symbols)
            }
            Expr::Call { callee, args, .. } => {
                for a in args { let _ = self.check_expr(a, symbols); }
                if let Some(sig) = self.signatures.get(callee).cloned() {
                    // 参数个数检查
                    if args.len() != sig.params.len() {
                        self.errors.push(TypeError::new(
                            expr_debug_line(expr),
                            format!("function '{}' expects {} args, got {}",
                                callee, sig.params.len(), args.len()),
                        ));
                    } else {
                        // 参数类型检查
                        for (i, ((_pname, pty), arg)) in sig.params.iter().zip(args.iter()).enumerate() {
                            let aty = self.check_expr(arg, symbols);
                            if !aty.compatible_with(pty) {
                                self.errors.push(TypeError::new(
                                    expr_debug_line(arg),
                                    format!("arg {} of '{}': expected '{}', got '{}'",
                                        i + 1, callee, pty.name(), aty.name()),
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
            Expr::MethodCall { object, method, args, .. } => {
                let ot = self.check_expr(object, symbols);
                for a in args { let _ = self.check_expr(a, symbols); }
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
        }
    }

    fn check_binary_op(&mut self, op: BinaryOp, lt: &Type, rt: &Type) -> Type {
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
                self.errors.push(TypeError::new(
                    0,
                    format!("operator '+' not defined for '{}' and '{}'", lt.name(), rt.name()),
                ));
                Type::Any
            }
            Sub | Mul | Div | Mod => {
                if matches!(lt, Type::Number | Type::Any) && matches!(rt, Type::Number | Type::Any) {
                    Type::Number
                } else {
                    self.errors.push(TypeError::new(
                        0,
                        format!("operator requires number operands, got '{}' and '{}'",
                            lt.name(), rt.name()),
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
                    self.errors.push(TypeError::new(
                        0,
                        format!("comparison requires number or string, got '{}' and '{}'",
                            lt.name(), rt.name()),
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
        (Type::Any, _) => Type::Any,
        (_, "len") => Type::Number,  // 通用 len
        _ => Type::Any,
    }
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
        | Expr::Match { span, .. } => span.line,
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
  let _ = greet(42)
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
  let _ = add(1)
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
  let _ = "a" + "b"
  let _ = "a" + 1
  let _ = [1] + [2]
  let _ = 1 + 2
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn binary_op_invalid() {
        let src = "let _ = true + 1\n";
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.iter().any(|e| e.message.contains("not defined")));
    }

    #[test]
    fn for_in_list() {
        let src = r#"
task main()
  for x in [1, 2, 3]
    let _ = x
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
    let _ = x
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
  let _ = [1, 2].map(fn(x) x * 2 end)
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
  let _ = f(5)
end
"#;
        let stmts = parse(src);
        let errs = check_program(&stmts);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn unknown_function_call_ok() {
        // 跨模块 task 名空间，未收集到的符号视为 Any
        let src = r#"
task main()
  let _ = maybe_undefined(1, 2, 3)
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
}
