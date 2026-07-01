//! v0.24: Parser v2 - 直接输出 ast_v2 节点
//!
//! 渐进式迁移：新解析函数直接输出 ast_v2，旧函数通过适配层转换

use crate::ast_v2::{
    AstArena, ExprKind, FnDef, NodeId, ObserveConfig, OrchestrateAgent, OrchestrateEdge,
    OrchestrateKind, Pattern, SkillTask, SkillVerify, StmtKind, TraitMethod,
};
use crate::common::{BinaryOp, Literal, Span};
use crate::lexer::{Token, TokenType};

mod expressions;
mod statements;

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

    /// 获取 Arena
    pub fn arena(&self) -> &AstArena {
        &self.arena
    }

    /// 获取可变 Arena
    pub fn arena_mut(&mut self) -> &mut AstArena {
        &mut self.arena
    }

    /// 转换完成，返回 Arena
    pub fn into_arena(self) -> AstArena {
        self.arena
    }

    // ===================================================================
    // 声明
    // ===================================================================

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
        } else if self.check(&TokenType::Macro) {
            Some(self.macro_statement())
        } else if self.check(&TokenType::Route) {
            Some(self.route_statement())
        } else if self.check(&TokenType::Trait) {
            Some(self.trait_statement())
        } else if self.check(&TokenType::Impl) {
            Some(self.impl_statement())
        } else if self.check(&TokenType::Type) {
            Some(self.type_alias_statement())
        } else if self.check(&TokenType::Enum) {
            Some(self.enum_statement())
        } else if self.check(&TokenType::Struct) {
            Some(self.struct_statement())
        } else if self.check(&TokenType::Orchestrate) {
            Some(self.orchestrate_statement())
        } else if self.check(&TokenType::Eval) {
            Some(self.eval_statement())
        } else if self.check(&TokenType::Skill) {
            Some(self.skill_statement())
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
        } else if self.check(&TokenType::Observe) {
            Some(self.observe_statement())
        } else if self.check(&TokenType::Span) {
            Some(self.span_statement())
        } else if self.match_identifier("record_tokens") {
            Some(self.record_tokens_statement())
        } else if self.check(&TokenType::Prompt) {
            Some(self.prompt_section_statement())
        } else if self.check(&TokenType::Document) {
            Some(self.document_section_statement())
        } else if self.check_index_assignment() {
            Some(self.index_assignment())
        } else if self.check_assignment() {
            Some(self.assignment_statement())
        } else {
            Some(self.expression_statement())
        }
    }

    // ===================================================================
    // 辅助函数
    // ===================================================================

    fn parameter(&mut self) -> (String, Option<String>) {
        let name = self.consume_identifier("Expected parameter name");
        let mut type_hint = None;
        if self.match_token(&[TokenType::Colon]) {
            type_hint = Some(self.consume_identifier("Expected type"));
        }
        (name, type_hint)
    }

    fn match_binary_op(&mut self) -> bool {
        if self.check(&TokenType::Plus)
            || self.check(&TokenType::Minus)
            || self.check(&TokenType::Star)
            || self.check(&TokenType::Slash)
            || self.check(&TokenType::Percent)
            || self.check(&TokenType::Equal)
            || self.check(&TokenType::NotEqual)
            || self.check(&TokenType::Greater)
            || self.check(&TokenType::Less)
            || self.check(&TokenType::GreaterEqual)
            || self.check(&TokenType::LessEqual)
        {
            self.advance();
            true
        } else {
            false
        }
    }

    fn previous_binary_op(&self) -> BinaryOp {
        match &self.tokens[self.current - 1].token_type {
            TokenType::Plus => BinaryOp::Add,
            TokenType::Minus => BinaryOp::Sub,
            TokenType::Star => BinaryOp::Mul,
            TokenType::Slash => BinaryOp::Div,
            TokenType::Percent => BinaryOp::Mod,
            TokenType::Equal => BinaryOp::Equal,
            TokenType::NotEqual => BinaryOp::NotEqual,
            TokenType::Greater => BinaryOp::Greater,
            TokenType::Less => BinaryOp::Less,
            TokenType::GreaterEqual => BinaryOp::GreaterEqual,
            TokenType::LessEqual => BinaryOp::LessEqual,
            _ => BinaryOp::Add, // fallback
        }
    }

    // ===================================================================
    // Token 操作
    // ===================================================================

    fn advance(&mut self) -> Option<&Token> {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.current)
    }

    fn peek_next(&self) -> Option<&Token> {
        self.tokens.get(self.current + 1)
    }

    fn previous(&self) -> Option<&Token> {
        if self.current > 0 {
            self.tokens.get(self.current - 1)
        } else {
            None
        }
    }

    fn is_at_end(&self) -> bool {
        self.current >= self.tokens.len()
            || self
                .tokens
                .get(self.current)
                .map(|t| t.token_type == TokenType::EOF)
                .unwrap_or(true)
    }

    fn check(&self, token_type: &TokenType) -> bool {
        self.peek()
            .map(|t| &t.token_type == token_type)
            .unwrap_or(false)
    }

    fn match_token(&mut self, types: &[TokenType]) -> bool {
        for tt in types {
            if self.check(tt) {
                self.advance();
                return true;
            }
        }
        false
    }

    fn consume(&mut self, token_type: &TokenType, message: &str) {
        if self.check(token_type) {
            self.advance();
        } else {
            eprintln!(
                "Parse error: {} at line {}",
                message,
                self.peek().map(|t| t.line).unwrap_or(0)
            );
        }
    }

    fn consume_identifier(&mut self, message: &str) -> String {
        match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                name
            }
            _ => {
                eprintln!(
                    "Parse error: {} at line {}",
                    message,
                    self.peek().map(|t| t.line).unwrap_or(0)
                );
                String::new()
            }
        }
    }

    fn span_of_current(&self) -> Span {
        self.peek()
            .map(|t| Span {
                line: t.line,
                column: t.column,
            })
            .unwrap_or(Span { line: 0, column: 0 })
    }

    fn peek_is_identifier(&self, name: &str) -> bool {
        matches!(self.peek(), Some(Token { token_type: TokenType::Identifier(n), .. }) if n == name)
    }

    fn match_identifier(&mut self, name: &str) -> bool {
        if self.peek_is_identifier(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn peek_after_less_is_ident(&self) -> bool {
        let is_less = self
            .peek()
            .map(|t| matches!(t.token_type, TokenType::Less))
            .unwrap_or(false);
        let next_is_ident = self
            .peek_next()
            .map(|t| matches!(t.token_type, TokenType::Identifier(_)))
            .unwrap_or(false);
        is_less && next_is_ident
    }

    fn peek_type_list_can_close(&self) -> bool {
        let tokens = &self.tokens;
        let start = self.current;
        if !matches!(
            tokens.get(start).map(|t| &t.token_type),
            Some(TokenType::Less)
        ) {
            return false;
        }
        fn skip_type(tokens: &[crate::lexer::Token], mut i: usize) -> Option<usize> {
            match tokens.get(i).map(|t| &t.token_type) {
                Some(TokenType::Identifier(_)) => {
                    i += 1;
                }
                _ => return None,
            }
            if matches!(tokens.get(i).map(|t| &t.token_type), Some(TokenType::Less)) {
                i += 1;
                i = skip_type(tokens, i)?;
                loop {
                    match tokens.get(i).map(|t| &t.token_type) {
                        Some(TokenType::Greater) => {
                            i += 1;
                            break;
                        }
                        Some(TokenType::Comma) => {
                            i += 1;
                        }
                        _ => return None,
                    }
                    i = skip_type(tokens, i)?;
                }
            }
            Some(i)
        }
        let mut i = start + 1;
        i = match skip_type(tokens, i) {
            Some(v) => v,
            None => return false,
        };
        loop {
            match tokens.get(i).map(|t| &t.token_type) {
                Some(TokenType::Greater) => return true,
                Some(TokenType::Comma) => {
                    i += 1;
                }
                _ => return false,
            }
            i = match skip_type(tokens, i) {
                Some(v) => v,
                None => return false,
            };
        }
    }

    fn parse_generic_params(&mut self) -> Vec<crate::common::GenericParam> {
        use crate::common::GenericParam;
        let mut params = Vec::new();
        self.advance(); // consume '<'
        loop {
            let pspan = self.span_of_current();
            let pname = self.consume_identifier("Expected generic param name");
            let pbound = if self.match_token(&[TokenType::Colon]) {
                Some(self.consume_identifier("Expected bound trait name"))
            } else {
                None
            };
            params.push(GenericParam {
                name: pname,
                bound: pbound,
                span: pspan,
            });
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(&TokenType::Greater, "Expected '>'");
        params
    }

    fn parse_type_list(&mut self) -> Vec<String> {
        let mut types = Vec::new();
        self.advance(); // consume '<'
        loop {
            let tn = self.parse_type_name_recursive();
            types.push(tn);
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(&TokenType::Greater, "Expected '>'");
        types
    }

    fn parse_type_name_recursive(&mut self) -> String {
        let tn = self.consume_identifier("Expected type name");
        if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            let generics = self.parse_type_list();
            format!("{}<{}>", tn, generics.join(","))
        } else {
            tn
        }
    }

    fn parse_where_clause(&mut self) -> Vec<crate::common::GenericParam> {
        use crate::common::GenericParam;
        let mut clauses = Vec::new();
        loop {
            let pspan = self.span_of_current();
            let pname = self.consume_identifier("Expected where clause param name");
            self.consume(&TokenType::Colon, "Expected ':'");
            let pbound = Some(self.consume_identifier("Expected bound trait name"));
            clauses.push(GenericParam {
                name: pname,
                bound: pbound,
                span: pspan,
            });
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        clauses
    }

    fn consume_method_name(&mut self, message: &str) -> String {
        match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                name
            }
            Some(tok) => {
                let name = match &tok.token_type {
                    TokenType::Route => "route",
                    TokenType::ReadBytes => "read_bytes",
                    TokenType::WriteBytes => "write_bytes",
                    TokenType::Read => "read",
                    TokenType::Write => "write",
                    TokenType::Append => "append",
                    TokenType::Let => "let",
                    TokenType::Task => "task",
                    TokenType::If => "if",
                    TokenType::For => "for",
                    TokenType::In => "in",
                    TokenType::Import => "import",
                    TokenType::As => "as",
                    TokenType::Do => "do",
                    TokenType::WithKeyword => "with",
                    TokenType::Save => "save",
                    TokenType::Load => "load",
                    TokenType::Fn => "fn",
                    TokenType::Into => "into",
                    TokenType::Stream => "stream",
                    TokenType::Tool => "tool",
                    TokenType::Break => "break",
                    TokenType::Continue => "continue",
                    TokenType::Observe => "observe",
                    TokenType::Span => "span",
                    TokenType::Tags => "tags",
                    TokenType::Record => "record",
                    TokenType::Trace => "trace",
                    TokenType::Metrics => "metrics",
                    TokenType::Otel => "otel",
                    TokenType::Export => "export",
                    TokenType::Parallel => "parallel",
                    _ => panic!("{}: unexpected token {:?}", message, tok.token_type),
                };
                self.advance();
                name.to_string()
            }
            None => panic!("{} at end of input", message),
        }
    }

    fn flatten_prompt_parts(&mut self, expr: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.collect_prompt_parts(expr, &mut out);
        out
    }

    fn collect_prompt_parts(&mut self, expr: NodeId, out: &mut Vec<NodeId>) {
        let binary_info = self.arena.get_expr(expr).and_then(|e| {
            if let ExprKind::Binary { left, op, right } = &e.kind {
                if *op == BinaryOp::Add {
                    Some((*left, *right))
                } else {
                    None
                }
            } else {
                None
            }
        });
        if let Some((left, right)) = binary_info {
            self.collect_prompt_parts(left, out);
            self.collect_prompt_parts(right, out);
        } else {
            out.push(expr);
        }
    }

    fn parse_format_string(&mut self, s: &str, span: Span) -> NodeId {
        let mut parts: Vec<NodeId> = Vec::new();
        let mut current = String::new();
        let mut chars = s.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '{' {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    current.push('{');
                } else {
                    if !current.is_empty() {
                        parts.push(self.arena.alloc_expr(
                            ExprKind::Literal(Literal::String(current.clone(), span)),
                            span,
                        ));
                        current.clear();
                    }
                    let mut expr_str = String::new();
                    let mut depth = 1;
                    for c in chars.by_ref() {
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        expr_str.push(c);
                    }
                    if depth != 0 {
                        panic!("Unmatched '{{' in format string");
                    }
                    let mut lexer = crate::lexer::Lexer::new(&expr_str);
                    let tokens = lexer.scan_tokens();
                    let mut parser = ParserV2::new(tokens);
                    let expr = parser.expression();
                    parts.push(expr);
                }
            } else {
                current.push(ch);
            }
        }

        if !current.is_empty() {
            parts.push(
                self.arena
                    .alloc_expr(ExprKind::Literal(Literal::String(current, span)), span),
            );
        }

        if parts.is_empty() {
            self.arena.alloc_expr(
                ExprKind::Literal(Literal::String(String::new(), span)),
                span,
            )
        } else {
            let mut result = parts[0];
            for part in &parts[1..] {
                let kind = ExprKind::Binary {
                    left: result,
                    op: BinaryOp::Add,
                    right: *part,
                };
                result = self.arena.alloc_expr(kind, span);
            }
            result
        }
    }

    fn parse_ai_model_call(&mut self, span: Span) -> NodeId {
        // 第一参数必为 model 名字符串
        if self.check(&TokenType::RParen) {
            panic!("ai_model: missing model name argument");
        }
        let model = self.expression();
        // 解析可选 keyword args: temperature: / max_tokens: / system:
        let mut temperature = None;
        let mut max_tokens = None;
        let mut system = None;
        while self.match_token(&[TokenType::Comma]) {
            let key = self.consume_identifier("Expected keyword name");
            self.consume(&TokenType::Colon, "Expected ':'");
            let val = self.expression();
            match key.as_str() {
                "temperature" => temperature = Some(val),
                "max_tokens" => max_tokens = Some(val),
                "system" => system = Some(val),
                other => panic!("ai_model: unknown keyword '{}'", other),
            }
        }
        self.consume(&TokenType::RParen, "Expected ')'");
        let kind = ExprKind::AiModelCall {
            model,
            temperature,
            max_tokens,
            system,
        };
        self.arena.alloc_expr(kind, span)
    }
}
fn has_format_interpolation(s: &str) -> bool {
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
            } else {
                return true;
            }
        }
    }
    false
}
