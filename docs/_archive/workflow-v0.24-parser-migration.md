# Mora v0.24 ParserV2 

> ****:  Parser (2459 )  ParserV2 Arena  + NodeId 
> ****:  v0.23 → v0.24 

---

## 

```
 .mora → Lexer → Token  → ParserV2 → ASTv2 →  → AST → 
                            ↑              ↑              ↑
                            Arena + NodeId    
```

### 

|  |  |
|------|------|
| Arena  |  |
| NodeId  |  ID  |
|  |  AST |
|  |  ast_v2 |

---

##  1:  (ast_v2.rs)

### 1.1  NodeId  Arena

```rust
// src/ast_v2.rs

///  IDArena 
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

/// AST Arena - 
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

    ///  NodeId
    pub fn alloc_stmt(&mut self, kind: StmtKind, span: Span) -> NodeId {
        let id = NodeId(self.stmts.len());
        self.stmts.push(TypedStmt { id, kind, span, ty: None });
        id
    }

    ///  NodeId
    pub fn alloc_expr(&mut self, kind: ExprKind, span: Span) -> NodeId {
        let id = NodeId(self.exprs.len());
        self.exprs.push(TypedExpr { id, kind, span, ty: None });
        id
    }
}
```

### 1.2  StmtKind  ExprKind

```rust
// src/ast_v2.rs

///  Span Type
#[derive(Debug, Clone)]
pub enum StmtKind {
    // 
    Let { name: String, type_hint: Option<String>, init: NodeId, exported: bool },
    Assign { name: String, value: NodeId },
    Return(Option<NodeId>),
    Break,
    Continue,
    Commit,
    Rollback,

    // 
    If { condition: NodeId, then_body: Vec<NodeId>, else_body: Vec<NodeId> },
    For { var: String, iter: NodeId, body: Vec<NodeId> },
    While { condition: NodeId, body: Vec<NodeId> },

    // 
    TaskDef(FnDef),
    TraitDef { name: String, methods: Vec<TraitMethod> },
    ImplDef { trait_name: Option<String>, type_name: String, methods: Vec<FnDef> },

    // 
    TypeAlias { name: String, generics: Vec<String>, target: String },
    EnumDef { name: String, generics: Vec<String>, variants: Vec<EnumVariant> },
    StructDef { name: String, generics: Vec<String>, fields: Vec<StructField> },

    // AI/
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

    // 
    ToolDef { name: String, params: Vec<(String, String)>, return_type: String, body: Vec<NodeId> },

    // 
    Expr(NodeId),
}

///  Span Type
#[derive(Debug, Clone)]
pub enum ExprKind {
    // 
    Literal(Literal),
    Char(char),

    // 
    Variable(String),
    NamespaceRef { module: String, method: String },

    // 
    Binary { left: NodeId, op: BinaryOp, right: NodeId },
    Unary { op: UnaryOp, operand: NodeId },
    Index { object: NodeId, index: NodeId },
    MethodCall { object: NodeId, method: String, args: Vec<NodeId> },
    Call { callee: NodeId, args: Vec<NodeId> },

    // 
    List(Vec<NodeId>),
    Dict(Vec<(NodeId, NodeId)>),
    Closure(FnDef),
    Pipe { left: NodeId, right: NodeId },

    // AI 
    Prompt(Vec<PromptPart>),
    FormatString(Vec<FormatPart>),
    AiModel { model: NodeId, args: Vec<(String, NodeId)> },

    // 
    Match { expr: NodeId, arms: Vec<MatchArm> },

    // 
    Question(NodeId),
}
```

---

##  2: ParserV2  (parser_v2.rs)

### 2.1 

```rust
// src/parser_v2.rs

use crate::ast::{BinaryOp, Literal, Span};
use crate::ast_v2::{AstArena, ExprKind, FnDef, NodeId, ObserveConfig, Pattern, StmtKind, TraitMethod};
use crate::lexer::{Token, TokenType};

/// Parser v2 -  ast_v2 
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

    ///  ast_v2  ID 
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

    ///  Arena
    pub fn into_arena(self) -> AstArena {
        self.arena
    }
}
```

### 2.2  (declaration)

```rust
// src/parser_v2.rs - 

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
            // 
            Some(self.expression_statement())
        }
    }
}
```

### 2.3  (expression)

```rust
// src/parser_v2.rs - 

impl ParserV2 {
    ///  - 
    fn expression(&mut self) -> NodeId {
        self.pipe_expression()
    }

    /// 
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

    /// match 
    fn match_expression(&mut self) -> NodeId {
        if self.check(&TokenType::Match) {
            self.match_expr()
        } else {
            self.binary_expression()
        }
    }

    ///  - 
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

    /// 
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

    ///  - 
    fn call_expression(&mut self) -> NodeId {
        let mut expr = self.primary();

        loop {
            if self.match_token(&[TokenType::Dot]) {
                // : obj.method(args)
                let method = self.consume_identifier("Expected method name");
                self.consume(&TokenType::LeftParen, "Expected '(' after method name");
                let args = self.parse_args();
                let span = self.span_of(expr);
                expr = self.arena.alloc_expr(
                    ExprKind::MethodCall { object: expr, method, args },
                    span,
                );
            } else if self.check(&TokenType::LeftParen) {
                // : callee(args)
                self.advance();
                let args = self.parse_args();
                let span = self.span_of(expr);
                expr = self.arena.alloc_expr(
                    ExprKind::Call { callee: expr, args },
                    span,
                );
            } else if self.match_token(&[TokenType::LeftBracket]) {
                // : obj[index]
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

    /// 
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

##  3:  (ast_v2_to_v1.rs)

### 3.1 

```rust
// src/ast_v2_to_v1.rs

use crate::ast::{self, Expr, FnDef, Literal, ObserveConfig, Stmt, TraitMethod};
use crate::ast_v2::{AstArena, ExprKind, NodeId, StmtKind};

///  ast_v2  ast
pub struct AstV2ToV1 {
    arena: AstArena,
}

impl AstV2ToV1 {
    pub fn new(arena: AstArena) -> Self {
        Self { arena }
    }

    /// 
    pub fn convert_program(&self, stmts: &[NodeId]) -> Vec<Stmt> {
        stmts.iter().map(|s| self.convert_stmt(*s)).collect()
    }

    /// 
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

            // ... 

            StmtKind::Expr(expr_id) => Stmt::ExprStmt {
                expr: self.convert_expr(*expr_id),
                span,
            },
        }
    }

    /// 
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
            // ... 
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

##  4:  (main.rs)

### 4.1  parse_code

```rust
// src/lib.rs  src/main.rs

///  ParserV2 AST
pub fn parse_code(source: &str) -> Vec<Stmt> {
    let tokens = Lexer::new(source).tokenize();
    let mut parser = ParserV2::new(tokens);
    let stmt_ids = parser.parse();
    let arena = parser.into_arena();
    let adapter = AstV2ToV1::new(arena);
    adapter.convert_program(&stmt_ids)
}

/// 
pub fn parse(source: &str) -> Vec<Stmt> {
    //  parser  parse_code
    parse_code(source)
}
```

### 4.2 

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
            // ... 
        }
    }
}
```

---

##  5:  (typeck.rs)

### 5.1  parse_code

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

### 5.2 

```rust
// src/typeck.rs - let 

fn check_let(&mut self, name: &str, type_hint: &Option<String>, init: &Expr) {
    let init_type = self.infer_type(init);

    match (type_hint, &init_type) {
        // 
        (Some(hint), _) => {
            if !self.types_compatible(hint, &init_type) {
                self.errors.push(TypeError {
                    message: format!("Type mismatch: expected {}, got {}", hint, init_type),
                    line: 0,
                });
            }
        },
        // 
        (None, _) => {
            self.env.define_type(name.to_string(), init_type);
        }
    }
}
```

---

##  6: Bug 

### 6.1 

```rust
// src/parser_v2.rs - 

/// 
/// expression
///   → pipe_expression
///   → match_expression
///   → binary_expression (/)
///   → unary_expression
///   → call_expression (//)  ← 
///   → primary

fn call_expression(&mut self) -> NodeId {
    let mut expr = self.primary();

    loop {
        // : obj.method(args)
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
        // : callee(args)
        else if self.check(&TokenType::LeftParen) {
            self.advance();
            let args = self.parse_args();
            let span = self.span_of(expr);
            expr = self.arena.alloc_expr(
                ExprKind::Call { callee: expr, args },
                span,
            );
        }
        // : obj[index]
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

### 6.2 

```rust
// 1. trait/impl  break guard
fn parse_impl_methods(&mut self) -> Vec<FnDef> {
    let mut methods = Vec::new();
    let mut guard = 0;
    while !self.check(&TokenType::End) && guard < 100 {
        methods.push(self.parse_fn_def());
        guard += 1;
    }
    methods
}

// 2. transaction rollback/commit 
fn transaction_statement(&mut self) -> NodeId {
    let span = self.span_of_current();
    self.advance(); // consume 'transaction'

    let body = self.parse_block();

    //  compensation 
    let compensation = if self.match_token(&[TokenType::Compensation]) {
        self.parse_block()
    } else {
        Vec::new()
    };

    self.consume(&TokenType::End, "Expected 'end' after transaction");
    self.arena.alloc_stmt(StmtKind::Transaction { body, compensation }, span)
}

// 3. match when 
fn match_pattern(&mut self) -> Pattern {
    let pattern = self.parse_pattern();

    //  when 
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

##  7: LSP 

### 7.1 providers.rs  ParserV2

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
    // ... hover 
}
```

---

##  8: 

### 8.1 

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
    // 
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

### 8.2 

```bash
# 
cargo test

# 
cargo test parser_v2

# 
cargo test 2>&1 | grep "test result"

#  clippy
cargo clippy --all-targets --all-features -- -D warnings
```

### 8.3 

```bash
# 
for f in examples/*.mora; do
  echo "Testing $f..."
  cargo run -- "$f" || echo "FAILED: $f"
done
```

---

##  9: 

### 9.1 CHANGELOG.md

```markdown
## [v0.24] - 2026-06-30

### ParserV2  (Complete)

 parser.rs (2459 )  ParserV2

#### 
- append_statement: 
- read_bytes_statement: 
- write_bytes_statement: 
- stream_statement: 
- tool_statement: 
- observe_statement: 
- span_statement: 
- record_tokens_statement:  token 
- assignment_statement: 
- index_assignment: 
- commit/rollback: /

#### 
- match_expression: 
- pattern: 
- parse_format_string: 
- parse_ai_model_call: ai_model 
- flatten_prompt_parts: Prompt 
- list_literal / dict_literal: 
- char_literal: 
- NamespaceRef: 

#### 
- parse_generic_params: 
- parse_type_list: 
- parse_type_name_recursive: 
- parse_where_clause: where 

#### 
- ObserveConfig:  ast_v2.rs 
- FnDef / TraitMethod:  ast_v2.rs 
- Pattern:  ast_v2.rs 
- consume_method_name: 
- : 
- : ast_v2_to_v1.rs  AST 
```

### 9.2 

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

##  10: CI/CD 

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

## 

```bash
# 1. 
cargo build

# 2. 
cargo test

# 3. clippy 
cargo clippy --all-targets --all-features -- -D warnings

# 4. 
cargo fmt -- --check

# 5. 
for f in examples/*.mora; do
  cargo run -- "$f" || exit 1
done

# 6. 
cargo test 2>&1 | grep "test result"

# 7. 
git add -A
git commit -m "feat(v0.24): ParserV2 "

# 8. 
git push origin main
```

---

## 

|  |  |
|------|-----|
| ParserV2  | 2151 |
| AST v2  | 541 |
|  | 502 |
|  Parser  | 2459 () |
|  | -265  |
|  | 188 passed |
|  | 5 passed |

---

## 

1. ****:  AST
2. **Arena **: 
3. **NodeId **:  ID 
4. ****: expression → pipe → match → binary → unary → call → primary
5. **break guard**: 

---

*v0.24 ParserV2  — 2026-06-30*
