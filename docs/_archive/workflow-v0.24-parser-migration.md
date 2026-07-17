# Mora v0.24 ParserV2 完整迁移工作流

> **目标**: 将旧 Parser (2459 行) 完整迁移到 ParserV2，实现 Arena 分配 + NodeId 引用架构。
> **可复现**: 按本文档步骤可完整复现 v0.23 → v0.24 的迁移过程。

---

## 架构概览

```
源码 .mora → Lexer → Token 流 → ParserV2 → ASTv2 → 反向适配器 → AST → 解释器
                            ↑              ↑              ↑
                        手写扫描器    Arena + NodeId    向后兼容层
```

### 核心设计决策

| 决策 | 原因 |
|------|------|
| Arena 分配 | 所有节点在连续内存，减少堆分配，支持增量编译 |
| NodeId 引用 | 通过 ID 引用节点，解耦生命周期 |
| 反向适配器 | 解释器继续使用旧 AST，渐进式迁移 |
| 渐进式替换 | 新函数直接输出 ast_v2，旧函数逐步迁移 |

---

## 阶段 1: 基础架构 (ast_v2.rs)

### 1.1 定义 NodeId 和 Arena

```rust
// src/ast_v2.rs

/// 节点 ID（Arena 中的索引）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

/// AST Arena - 连续内存存储所有节点
pub struct AstArena {
    pub stmts: Vec<TypedStmt>,
    pub exprs: Vec<TypedExpr>,
}

impl AstArena {
    pub fn new() -> Self {
        Self {
            stmts: Vec::new(),
            exprs: Vec::new(),
        }
    }

    /// 分配语句节点，返回 NodeId
    pub fn alloc_stmt(&mut self, kind: StmtKind, span: Span) -> NodeId {
        let id = NodeId(self.stmts.len());
        self.stmts.push(TypedStmt { id, kind, span, ty: None });
        id
    }

    /// 分配表达式节点，返回 NodeId
    pub fn alloc_expr(&mut self, kind: ExprKind, span: Span) -> NodeId {
        let id = NodeId(self.exprs.len());
        self.exprs.push(TypedExpr { id, kind, span, ty: None });
        id
    }
}
```

### 1.2 定义 StmtKind 和 ExprKind

```rust
// src/ast_v2.rs

/// 语句种类（无 Span、无 Type）
#[derive(Debug, Clone)]
pub enum StmtKind {
    // 基础语句
    Let { name: String, type_hint: Option<String>, init: NodeId, exported: bool },
    Assign { name: String, value: NodeId },
    Return(Option<NodeId>),
    Break,
    Continue,
    Commit,
    Rollback,

    // 复合语句
    If { condition: NodeId, then_body: Vec<NodeId>, else_body: Vec<NodeId> },
    For { var: String, iter: NodeId, body: Vec<NodeId> },
    While { condition: NodeId, body: Vec<NodeId> },

    // 函数定义
    TaskDef(FnDef),
    TraitDef { name: String, methods: Vec<TraitMethod> },
    ImplDef { trait_name: Option<String>, type_name: String, methods: Vec<FnDef> },

    // 类型定义
    TypeAlias { name: String, generics: Vec<String>, target: String },
    EnumDef { name: String, generics: Vec<String>, variants: Vec<EnumVariant> },
    StructDef { name: String, generics: Vec<String>, fields: Vec<StructField> },

    // AI/云原生
    Match { expr: NodeId, arms: Vec<MatchArm> },
    With { config: WithConfig, body: Vec<NodeId> },
    Parallel { workers: Vec<WorkerDef> },
    Transaction { body: Vec<NodeId>, compensation: Vec<NodeId> },
    Observe { config: ObserveConfig, body: Vec<NodeId> },
    Span { name: String, tags: Vec<(String, NodeId)>, body: Vec<NodeId> },

    // IO
    Import(String),
    Save { path: NodeId, value: NodeId },
    Load { path: NodeId, target: String },
    Read { path: NodeId, target: String },
    Write { path: NodeId, content: NodeId },
    Append { path: NodeId, content: NodeId },
    ReadBytes { path: NodeId, target: String },
    WriteBytes { path: NodeId, content: NodeId },
    Stream { expr: NodeId, var: String, body: Vec<NodeId> },
    RecordTokens { input: NodeId, output: NodeId },

    // 工具定义
    ToolDef { name: String, params: Vec<(String, String)>, return_type: String, body: Vec<NodeId> },

    // 表达式语句
    Expr(NodeId),
}

/// 表达式种类（无 Span、无 Type）
#[derive(Debug, Clone)]
pub enum ExprKind {
    // 字面量
    Literal(Literal),
    Char(char),

    // 变量
    Variable(String),
    NamespaceRef { module: String, method: String },

    // 运算
    Binary { left: NodeId, op: BinaryOp, right: NodeId },
    Unary { op: UnaryOp, operand: NodeId },
    Index { object: NodeId, index: NodeId },
    MethodCall { object: NodeId, method: String, args: Vec<NodeId> },
    Call { callee: NodeId, args: Vec<NodeId> },

    // 复合
    List(Vec<NodeId>),
    Dict(Vec<(NodeId, NodeId)>),
    Closure(FnDef),
    Pipe { left: NodeId, right: NodeId },

    // AI 原语
    Prompt(Vec<PromptPart>),
    FormatString(Vec<FormatPart>),
    AiModel { model: NodeId, args: Vec<(String, NodeId)> },

    // 模式匹配
    Match { expr: NodeId, arms: Vec<MatchArm> },

    // 问号表达式
    Question(NodeId),
}
```

---

## 阶段 2: ParserV2 实现 (parser_v2.rs)

### 2.1 基础结构

```rust
// src/parser_v2.rs

use crate::ast::{BinaryOp, Literal, Span};
use crate::ast_v2::{AstArena, ExprKind, FnDef, NodeId, ObserveConfig, Pattern, StmtKind, TraitMethod};
use crate::lexer::{Token, TokenType};

/// Parser v2 - 直接输出 ast_v2 节点
pub struct ParserV2 {
    tokens: Vec<Token>,
    current: usize,
    arena: AstArena,
}

impl ParserV2 {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            current: 0,
            arena: AstArena::new(),
        }
    }

    /// 解析整个程序，返回 ast_v2 节点 ID 列表
    pub fn parse(&mut self) -> Vec<NodeId> {
        let mut stmts = Vec::new();
        while !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                stmts.push(stmt_id);
            }
        }
        stmts
    }

    /// 转换完成，返回 Arena
    pub fn into_arena(self) -> AstArena {
        self.arena
    }
}
```

### 2.2 声明解析 (declaration)

```rust
// src/parser_v2.rs - 核心解析函数

impl ParserV2 {
    fn declaration(&mut self) -> Option<NodeId> {
        let exported = self.match_token(&[TokenType::Export]);

        if self.check(&TokenType::Let) {
            Some(self.let_declaration_exported(exported))
        } else if self.check(&TokenType::Task) {
            Some(self.task_declaration_exported(exported))
        } else if exported {
            panic!("Expected 'let' or 'task' after 'export'");
        } else if self.check(&TokenType::Trait) {
            Some(self.trait_statement())
        } else if self.check(&TokenType::Return) {
            Some(self.return_statement())
        } else if self.check(&TokenType::If) {
            Some(self.if_statement())
        } else if self.check(&TokenType::For) {
            Some(self.for_statement())
        } else if self.check(&TokenType::Import) {
            Some(self.import_statement())
        } else if self.check(&TokenType::Break) {
            let span = self.span_of_current();
            self.advance();
            Some(self.arena.alloc_stmt(StmtKind::Break, span))
        } else if self.check(&TokenType::Continue) {
            let span = self.span_of_current();
            self.advance();
            Some(self.arena.alloc_stmt(StmtKind::Continue, span))
        } else if self.check(&TokenType::Commit) {
            let span = self.span_of_current();
            self.advance();
            Some(self.arena.alloc_stmt(StmtKind::Commit, span))
        } else if self.check(&TokenType::Rollback) {
            let span = self.span_of_current();
            self.advance();
            Some(self.arena.alloc_stmt(StmtKind::Rollback, span))
        } else if self.check(&TokenType::Match) {
            Some(self.match_statement())
        } else if self.check(&TokenType::WithKeyword) {
            Some(self.with_statement())
        } else if self.check(&TokenType::Parallel) {
            Some(self.parallel_statement())
        } else if self.check(&TokenType::Transaction) {
            Some(self.transaction_statement())
        } else if self.check(&TokenType::Observe) {
            Some(self.observe_statement())
        } else if self.check(&TokenType::Span) {
            Some(self.span_statement())
        } else if self.check(&TokenType::Save) {
            Some(self.save_statement())
        } else if self.check(&TokenType::Load) {
            Some(self.load_statement())
        } else if self.check(&TokenType::Read) {
            Some(self.read_statement())
        } else if self.check(&TokenType::Write) {
            Some(self.write_statement())
        } else if self.check(&TokenType::Append) {
            Some(self.append_statement())
        } else if self.check(&TokenType::ReadBytes) {
            Some(self.read_bytes_statement())
        } else if self.check(&TokenType::WriteBytes) {
            Some(self.write_bytes_statement())
        } else if self.check(&TokenType::Stream) {
            Some(self.stream_statement())
        } else if self.check(&TokenType::Tool) {
            Some(self.tool_statement())
        } else if self.check(&TokenType::Type) {
            Some(self.type_alias_statement())
        } else if self.check(&TokenType::Enum) {
            Some(self.enum_statement())
        } else if self.check(&TokenType::Struct) {
            Some(self.struct_statement())
        } else if self.check(&TokenType::RecordTokens) {
            Some(self.record_tokens_statement())
        } else if self.check(&TokenType::Impl) {
            Some(self.impl_statement())
        } else {
            // 默认作为表达式语句
            Some(self.expression_statement())
        }
    }
}
```

### 2.3 表达式解析 (expression)

```rust
// src/parser_v2.rs - 表达式优先级

impl ParserV2 {
    /// 表达式入口 - 最低优先级
    fn expression(&mut self) -> NodeId {
        self.pipe_expression()
    }

    /// 管道表达式
    fn pipe_expression(&mut self) -> NodeId {
        let mut left = self.match_expression();
        while self.match_token(&[TokenType::Pipe]) {
            let right = self.match_expression();
            let span = self.span_of(left);
            left = self.arena.alloc_expr(
                ExprKind::Pipe { left, right },
                span,
            );
        }
        left
    }

    /// match 表达式
    fn match_expression(&mut self) -> NodeId {
        if self.check(&TokenType::Match) {
            self.match_expr()
        } else {
            self.binary_expression()
        }
    }

    /// 二元表达式 - 比较和逻辑运算
    fn binary_expression(&mut self) -> NodeId {
        let mut left = self.unary_expression();
        while let Some(op) = self.match_binary_op() {
            let right = self.unary_expression();
            let span = self.span_of(left);
            left = self.arena.alloc_expr(
                ExprKind::Binary { left, op, right },
                span,
            );
        }
        left
    }

    /// 一元表达式
    fn unary_expression(&mut self) -> NodeId {
        if self.match_token(&[TokenType::Minus]) {
            let operand = self.unary_expression();
            let span = self.span_of(operand);
            self.arena.alloc_expr(
                ExprKind::Unary { op: UnaryOp::Neg, operand },
                span,
            )
        } else if self.match_token(&[TokenType::Not]) {
            let operand = self.unary_expression();
            let span = self.span_of(operand);
            self.arena.alloc_expr(
                ExprKind::Unary { op: UnaryOp::Not, operand },
                span,
            )
        } else if self.match_token(&[TokenType::Ampersand]) {
            let operand = self.unary_expression();
            let span = self.span_of(operand);
            self.arena.alloc_expr(
                ExprKind::Unary { op: UnaryOp::Borrow, operand },
                span,
            )
        } else if self.match_token(&[TokenType::Mut]) {
            let operand = self.unary_expression();
            let span = self.span_of(operand);
            self.arena.alloc_expr(
                ExprKind::Unary { op: UnaryOp::MutBorrow, operand },
                span,
            )
        } else {
            self.call_expression()
        }
    }

    /// 调用表达式 - 方法调用和函数调用
    fn call_expression(&mut self) -> NodeId {
        let mut expr = self.primary();

        loop {
            if self.match_token(&[TokenType::Dot]) {
                // 方法调用: obj.method(args)
                let method = self.consume_identifier("Expected method name");
                self.consume(&TokenType::LeftParen, "Expected '(' after method name");
                let args = self.parse_args();
                let span = self.span_of(expr);
                expr = self.arena.alloc_expr(
                    ExprKind::MethodCall { object: expr, method, args },
                    span,
                );
            } else if self.check(&TokenType::LeftParen) {
                // 函数调用: callee(args)
                self.advance();
                let args = self.parse_args();
                let span = self.span_of(expr);
                expr = self.arena.alloc_expr(
                    ExprKind::Call { callee: expr, args },
                    span,
                );
            } else if self.match_token(&[TokenType::LeftBracket]) {
                // 索引访问: obj[index]
                let index = self.expression();
                self.consume(&TokenType::RightBracket, "Expected ']'");
                let span = self.span_of(expr);
                expr = self.arena.alloc_expr(
                    ExprKind::Index { object: expr, index },
                    span,
                );
            } else {
                break;
            }
        }

        expr
    }

    /// 基础表达式
    fn primary(&mut self) -> NodeId {
        let span = self.span_of_current();

        if self.match_token(&[TokenType::Number]) {
            let value = self.previous().literal.unwrap();
            self.arena.alloc_expr(ExprKind::Literal(Literal::Number(value)), span)
        } else if self.match_token(&[TokenType::String]) {
            let value = self.previous().literal.unwrap();
            self.arena.alloc_expr(ExprKind::Literal(Literal::String(value)), span)
        } else if self.match_token(&[TokenType::True]) {
            self.arena.alloc_expr(ExprKind::Literal(Literal::Bool(true)), span)
        } else if self.match_token(&[TokenType::False]) {
            self.arena.alloc_expr(ExprKind::Literal(Literal::Bool(false)), span)
        } else if self.match_token(&[TokenType::Nil]) {
            self.arena.alloc_expr(ExprKind::Literal(Literal::Nil), span)
        } else if self.match_token(&[TokenType::Identifier]) {
            let name = self.previous().lexeme.clone();
            self.arena.alloc_expr(ExprKind::Variable(name), span)
        } else if self.match_token(&[TokenType::LeftBracket]) {
            self.list_literal()
        } else if self.match_token(&[TokenType::LeftBrace]) {
            self.dict_literal()
        } else if self.match_token(&[TokenType::Fn]) {
            self.closure_expression()
        } else if self.match_token(&[TokenType::Prompt]) {
            self.prompt_expression()
        } else if self.match_token(&[TokenType::LeftParen]) {
            let expr = self.expression();
            self.consume(&TokenType::RightParen, "Expected ')'");
            expr
        } else {
            panic!("Unexpected token: {:?}", self.current_token());
        }
    }
}
```

---

## 阶段 3: 反向适配器 (ast_v2_to_v1.rs)

### 3.1 核心转换逻辑

```rust
// src/ast_v2_to_v1.rs

use crate::ast::{self, Expr, FnDef, Literal, ObserveConfig, Stmt, TraitMethod};
use crate::ast_v2::{AstArena, ExprKind, NodeId, StmtKind};

/// 反向适配器：将 ast_v2 转换为 ast
pub struct AstV2ToV1 {
    arena: AstArena,
}

impl AstV2ToV1 {
    pub fn new(arena: AstArena) -> Self {
        Self { arena }
    }

    /// 转换整个程序
    pub fn convert_program(&self, stmts: &[NodeId]) -> Vec<Stmt> {
        stmts.iter().map(|s| self.convert_stmt(*s)).collect()
    }

    /// 转换语句
    fn convert_stmt(&self, id: NodeId) -> Stmt {
        let stmt = self.arena.stmts.get(id.0).unwrap_or_else(|| {
            panic!("Invalid statement NodeId({}), stmts.len={}", id.0, self.arena.stmts.len())
        });
        let span = stmt.span;

        match &stmt.kind {
            StmtKind::Let { name, type_hint, init, exported } => Stmt::Let {
                name: name.clone(),
                type_hint: type_hint.clone(),
                init: self.convert_expr(*init),
                exported: *exported,
                span,
            },
            StmtKind::Assign { name, value } => Stmt::Assign {
                name: name.clone(),
                value: self.convert_expr(*value),
                span,
            },
            StmtKind::Return(expr) => Stmt::Return {
                value: expr.map(|e| self.convert_expr(e)),
                span,
            },
            StmtKind::Break => Stmt::Break { span },
            StmtKind::Continue => Stmt::Continue { span },
            StmtKind::Commit => Stmt::Commit { span },
            StmtKind::Rollback => Stmt::Rollback { span },

            StmtKind::If { condition, then_body, else_body } => Stmt::If {
                condition: self.convert_expr(*condition),
                then_body: self.convert_stmts(then_body),
                else_body: self.convert_stmts(else_body),
                span,
            },
            StmtKind::For { var, iter, body } => Stmt::For {
                var: var.clone(),
                iter: self.convert_expr(*iter),
                body: self.convert_stmts(body),
                span,
            },

            // ... 其他语句类型类似转换

            StmtKind::Expr(expr_id) => Stmt::ExprStmt {
                expr: self.convert_expr(*expr_id),
                span,
            },
        }
    }

    /// 转换表达式
    fn convert_expr(&self, id: NodeId) -> Expr {
        let expr = self.arena.exprs.get(id.0).unwrap_or_else(|| {
            panic!("Invalid expression NodeId({}), exprs.len={}", id.0, self.arena.exprs.len())
        });
        let span = expr.span;

        match &expr.kind {
            ExprKind::Literal(lit) => Expr::Literal(lit.clone(), span),
            ExprKind::Char(c) => Expr::Char(*c, span),
            ExprKind::Variable(name) => Expr::Variable(name.clone(), span),
            ExprKind::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.convert_expr(*left)),
                op: op.clone(),
                right: Box::new(self.convert_expr(*right)),
                span,
            },
            ExprKind::Unary { op, operand } => Expr::Unary {
                op: op.clone(),
                operand: Box::new(self.convert_expr(*operand)),
                span,
            },
            ExprKind::Call { callee, args } => Expr::Call {
                callee: Box::new(self.convert_expr(*callee)),
                args: self.convert_exprs(args),
                span,
            },
            ExprKind::MethodCall { object, method, args } => Expr::MethodCall {
                object: Box::new(self.convert_expr(*object)),
                method: method.clone(),
                args: self.convert_exprs(args),
                span,
            },
            ExprKind::Index { object, index } => Expr::Index {
                object: Box::new(self.convert_expr(*object)),
                index: Box::new(self.convert_expr(*index)),
                span,
            },
            ExprKind::List(items) => Expr::List {
                items: self.convert_exprs(items),
                span,
            },
            ExprKind::Dict(entries) => Expr::Dict {
                entries: entries.iter().map(|(k, v)| {
                    (self.convert_expr(*k), self.convert_expr(*v))
                }).collect(),
                span,
            },
            ExprKind::Closure(fn_def) => Expr::Closure {
                params: fn_def.params.clone(),
                body: self.convert_stmts(&fn_def.body),
                span,
            },
            ExprKind::Pipe { left, right } => Expr::Pipe {
                left: Box::new(self.convert_expr(*left)),
                right: Box::new(self.convert_expr(*right)),
                span,
            },
            ExprKind::Prompt(parts) => Expr::Prompt {
                parts: parts.iter().map(|p| match p {
                    crate::ast_v2::PromptPart::Text(s) => ast::PromptPart::Text(s.clone()),
                    crate::ast_v2::PromptPart::Expr(e) => ast::PromptPart::Expr(self.convert_expr(*e)),
                }).collect(),
                span,
            },
            // ... 其他表达式类型类似转换
        }
    }

    fn convert_stmts(&self, ids: &[NodeId]) -> Vec<Stmt> {
        ids.iter().map(|id| self.convert_stmt(*id)).collect()
    }

    fn convert_exprs(&self, ids: &[NodeId]) -> Vec<Expr> {
        ids.iter().map(|id| self.convert_expr(*id)).collect()
    }
}
```

---

## 阶段 4: 主程序集成 (main.rs)

### 4.1 统一入口 parse_code

```rust
// src/lib.rs 或 src/main.rs

/// 统一解析入口：使用 ParserV2，返回转换后的旧 AST
pub fn parse_code(source: &str) -> Vec<Stmt> {
    let tokens = Lexer::new(source).tokenize();
    let mut parser = ParserV2::new(tokens);
    let stmt_ids = parser.parse();
    let arena = parser.into_arena();
    let adapter = AstV2ToV1::new(arena);
    adapter.convert_program(&stmt_ids)
}

/// 旧入口（已弃用，保留兼容）
pub fn parse(source: &str) -> Vec<Stmt> {
    // 旧 parser 已删除，直接调用 parse_code
    parse_code(source)
}
```

### 4.2 解释器使用

```rust
// src/interpreter.rs

use crate::parse_code;

impl Interpreter {
    pub fn run(&mut self, source: &str) -> Result<Value, String> {
        let ast = parse_code(source);
        self.execute_program(&ast)
    }

    fn execute_program(&mut self, stmts: &[Stmt]) -> Result<Value, String> {
        for stmt in stmts {
            self.execute(stmt)?;
        }
        Ok(Value::Nil)
    }

    fn execute(&mut self, stmt: &Stmt) -> Result<Value, String> {
        match stmt {
            Stmt::Let { name, type_hint, init, exported, span } => {
                let value = self.evaluate(init)?;
                self.env.define(name.clone(), value);
                Ok(Value::Nil)
            },
            Stmt::Assign { name, value, span } => {
                let val = self.evaluate(value)?;
                self.env.assign(name, val)?;
                Ok(Value::Nil)
            },
            Stmt::Return { value, span } => {
                let val = match value {
                    Some(expr) => self.evaluate(expr)?,
                    None => Value::Nil,
                };
                Ok(Value::Return(Box::new(val)))
            },
            // ... 其他语句
        }
    }
}
```

---

## 阶段 5: 类型检查器集成 (typeck.rs)

### 5.1 类型检查使用 parse_code

```rust
// src/typeck.rs

use crate::parse_code;

pub fn type_check(source: &str) -> Vec<TypeError> {
    let ast = parse_code(source);
    let mut checker = TypeChecker::new();
    checker.check_program(&ast);
    checker.errors
}
```

### 5.2 类型推断改进

```rust
// src/typeck.rs - let 推断改进

fn check_let(&mut self, name: &str, type_hint: &Option<String>, init: &Expr) {
    let init_type = self.infer_type(init);

    match (type_hint, &init_type) {
        // 有类型注解：检查是否匹配
        (Some(hint), _) => {
            if !self.types_compatible(hint, &init_type) {
                self.errors.push(TypeError {
                    message: format!("Type mismatch: expected {}, got {}", hint, init_type),
                    line: 0,
                });
            }
        },
        // 无类型注解：自动推断
        (None, _) => {
            self.env.define_type(name.to_string(), init_type);
        }
    }
}
```

---

## 阶段 6: Bug 修复

### 6.1 表达式优先级修复

```rust
// src/parser_v2.rs - 修复方法调用优先级

/// 正确的优先级链：
/// expression
///   → pipe_expression
///   → match_expression
///   → binary_expression (比较/逻辑)
///   → unary_expression
///   → call_expression (方法调用/函数调用/索引)  ← 关键：在这里
///   → primary

fn call_expression(&mut self) -> NodeId {
    let mut expr = self.primary();

    loop {
        // 方法调用: obj.method(args)
        if self.match_token(&[TokenType::Dot]) {
            let method = self.consume_identifier("Expected method name");
            self.consume(&TokenType::LeftParen, "Expected '('");
            let args = self.parse_args();
            let span = self.span_of(expr);
            expr = self.arena.alloc_expr(
                ExprKind::MethodCall { object: expr, method, args },
                span,
            );
        }
        // 函数调用: callee(args)
        else if self.check(&TokenType::LeftParen) {
            self.advance();
            let args = self.parse_args();
            let span = self.span_of(expr);
            expr = self.arena.alloc_expr(
                ExprKind::Call { callee: expr, args },
                span,
            );
        }
        // 索引: obj[index]
        else if self.match_token(&[TokenType::LeftBracket]) {
            let index = self.expression();
            self.consume(&TokenType::RightBracket, "Expected ']'");
            let span = self.span_of(expr);
            expr = self.arena.alloc_expr(
                ExprKind::Index { object: expr, index },
                span,
            );
        }
        else {
            break;
        }
    }

    expr
}
```

### 6.2 其他修复点

```rust
// 1. trait/impl 方法循环 break guard
fn parse_impl_methods(&mut self) -> Vec<FnDef> {
    let mut methods = Vec::new();
    let mut guard = 0;
    while !self.check(&TokenType::End) && guard < 100 {
        methods.push(self.parse_fn_def());
        guard += 1;
    }
    methods
}

// 2. transaction rollback/commit 正确解析
fn transaction_statement(&mut self) -> NodeId {
    let span = self.span_of_current();
    self.advance(); // consume 'transaction'

    let body = self.parse_block();

    // 解析 compensation 块（可选）
    let compensation = if self.match_token(&[TokenType::Compensation]) {
        self.parse_block()
    } else {
        Vec::new()
    };

    self.consume(&TokenType::End, "Expected 'end' after transaction");
    self.arena.alloc_stmt(StmtKind::Transaction { body, compensation }, span)
}

// 3. match when 守卫支持
fn match_pattern(&mut self) -> Pattern {
    let pattern = self.parse_pattern();

    // 支持 when 守卫
    if self.match_token(&[TokenType::When]) {
        let condition = self.expression();
        Pattern::Guard {
            pattern: Box::new(pattern),
            condition,
        }
    } else {
        pattern
    }
}
```

---

## 阶段 7: LSP 集成

### 7.1 providers.rs 使用 ParserV2

```rust
// src/lsp/providers.rs

use crate::parse_code;

pub fn get_diagnostics(source: &str) -> Vec<Diagnostic> {
    let ast = parse_code(source);
    let mut checker = TypeChecker::new();
    checker.check_program(&ast);

    checker.errors.iter().map(|err| {
        Diagnostic {
            range: Range {
                start: Position { line: err.line - 1, character: 0 },
                end: Position { line: err.line - 1, character: 100 },
            },
            severity: Some(DiagnosticSeverity::Error),
            message: err.message.clone(),
        }
    }).collect()
}

pub fn get_hover(source: &str, position: Position) -> Option<Hover> {
    let ast = parse_code(source);
    // ... hover 逻辑
}
```

---

## 阶段 8: 测试

### 8.1 单元测试

```rust
// tests/parser_v2_integration.rs

#[test]
fn test_basic_let() {
    let src = r#"
task main()
  let x = 42
end
"#;
    let ast = parse_code(src);
    assert_eq!(ast.len(), 1); // task main
}

#[test]
fn test_method_call_priority() {
    let src = r#"
task main()
  let result = list.map(fn(x) return x * 2 end).filter(fn(x) return x > 5 end)
end
"#;
    let ast = parse_code(src);
    // 方法调用应该先于二元运算符解析
}

#[test]
fn test_match_guard() {
    let src = r#"
task main()
  match n with x when x > 0 ->
    return "positive"
  end
end
"#;
    let ast = parse_code(src);
}

#[test]
fn test_transaction() {
    let src = r#"
task main()
  transaction
    let x = 1
  compensation
    rollback
  end
end
"#;
    let ast = parse_code(src);
}
```

### 8.2 集成测试

```bash
# 运行所有测试
cargo test

# 运行特定测试
cargo test parser_v2

# 查看测试数量
cargo test 2>&1 | grep "test result"

# 运行 clippy
cargo clippy --all-targets --all-features -- -D warnings
```

### 8.3 验证真实脚本

```bash
# 验证所有示例脚本
for f in examples/*.mora; do
  echo "Testing $f..."
  cargo run -- "$f" || echo "FAILED: $f"
done
```

---

## 阶段 9: 文档更新

### 9.1 CHANGELOG.md

```markdown
## [v0.24] - 2026-06-30

### ParserV2 完整迁移 (Complete)

旧 parser.rs (2459 行) 已删除，主程序和测试全部使用 ParserV2。

#### 新增语句解析
- append_statement: 追加文件写入
- read_bytes_statement: 读取字节文件
- write_bytes_statement: 写入字节文件
- stream_statement: 流式循环
- tool_statement: 工具定义
- observe_statement: 可观测性配置
- span_statement: 追踪范围
- record_tokens_statement: 记录 token 使用量
- assignment_statement: 赋值语句
- index_assignment: 索引赋值
- commit/rollback: 事务提交/回滚

#### 新增表达式解析
- match_expression: 模式匹配表达式
- pattern: 模式解析
- parse_format_string: 格式字符串插值
- parse_ai_model_call: ai_model 调用
- flatten_prompt_parts: Prompt 表达式展平
- list_literal / dict_literal: 列表和字典字面量
- char_literal: 字符字面量
- NamespaceRef: 命名空间引用

#### 新增类型系统支持
- parse_generic_params: 泛型参数
- parse_type_list: 类型列表
- parse_type_name_recursive: 递归解析嵌套泛型
- parse_where_clause: where 子句

#### 重构
- ObserveConfig: 在 ast_v2.rs 中定义新类型
- FnDef / TraitMethod: 在 ast_v2.rs 中定义新类型
- Pattern: 在 ast_v2.rs 中定义新类型
- consume_method_name: 支持关键字作为方法名
- 表达式优先级: 修复方法调用优先级
- 反向适配器: ast_v2_to_v1.rs 支持完整 AST 转换
```

### 9.2 版本号更新

```toml
# Cargo.toml
[package]
version = "0.0.24"
```

```dockerfile
# Dockerfile
# v0.24
```

---

## 阶段 10: CI/CD 更新

### 10.1 GitHub Actions

```yaml
# .github/workflows/ci.yml

name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo check --all-targets

  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test

  fmt:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo fmt --all -- --check

  clippy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo clippy --all-targets --all-features -- -D warnings

  lsp-smoke:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo run --example lsp_smoke
```

---

## 最终验证清单

```bash
# 1. 编译检查
cargo build

# 2. 运行所有测试
cargo test

# 3. clippy 检查
cargo clippy --all-targets --all-features -- -D warnings

# 4. 格式检查
cargo fmt -- --check

# 5. 验证示例脚本
for f in examples/*.mora; do
  cargo run -- "$f" || exit 1
done

# 6. 查看测试统计
cargo test 2>&1 | grep "test result"

# 7. 提交
git add -A
git commit -m "feat(v0.24): ParserV2 完整迁移"

# 8. 推送
git push origin main
```

---

## 统计

| 指标 | 值 |
|------|-----|
| ParserV2 行数 | 2151 |
| AST v2 行数 | 541 |
| 反向适配器行数 | 502 |
| 旧 Parser 行数 | 2459 (已删除) |
| 净变化 | -265 行 |
| 测试数量 | 188 passed |
| 集成测试 | 5 passed |

---

## 关键经验

1. **渐进式迁移**: 先实现反向适配器，让解释器继续使用旧 AST，再逐步替换
2. **Arena 分配**: 所有节点在连续内存，减少堆分配，提升缓存命中率
3. **NodeId 引用**: 通过 ID 引用节点，解耦生命周期，支持增量编译
4. **优先级链**: expression → pipe → match → binary → unary → call → primary
5. **break guard**: 防止解析器无限循环

---

*v0.24 ParserV2 完整迁移工作流 — 2026-06-30*
