//! v0.24: Parser v2 - 直接输出 ast_v2 节点
//!
//! 渐进式迁移：新解析函数直接输出 ast_v2，旧函数通过适配层转换

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
        } else if self.check_index_assignment() {
            Some(self.index_assignment())
        } else if self.check_assignment() {
            Some(self.assignment_statement())
        } else {
            Some(self.expression_statement())
        }
    }

    // ===================================================================
    // 语句
    // ===================================================================

    fn let_declaration_exported(&mut self, exported: bool) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'let'
        let name = self.consume_identifier("Expected variable name");
        let mut type_hint = None;
        if self.match_token(&[TokenType::Colon]) {
            // 支持 dyn Trait 泛型 hint
            let hint = if self.match_token(&[TokenType::Dyn]) {
                let tname = self.consume_identifier("Expected trait name after 'dyn'");
                let generics_suffix = if self.check(&TokenType::Less) {
                    self.parse_type_list()
                } else {
                    vec![]
                };
                if generics_suffix.is_empty() {
                    format!("dyn:{}", tname)
                } else {
                    format!("dyn:{}<{}>", tname, generics_suffix.join(","))
                }
            } else {
                self.parse_type_name_recursive()
            };
            type_hint = Some(hint);
        }
        self.consume(&TokenType::Assign, "Expected '='");
        let init = self.expression();
        let kind = StmtKind::Let {
            name,
            type_hint,
            init,
            exported,
        };
        self.arena.alloc_stmt(kind, span)
    }

    fn task_declaration_exported(&mut self, exported: bool) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'task'
        let name = self.consume_identifier("Expected task name");

        // 解析生命周期参数
        let mut lifetime_params = Vec::new();
        if self.check(&TokenType::Less) {
            self.advance(); // consume '<'
            loop {
                if let Some(Token { token_type: TokenType::Lifetime(lt), .. }) = self.peek().cloned() {
                    self.advance();
                    lifetime_params.push(lt);
                    if self.match_token(&[TokenType::Comma]) {
                        continue;
                    }
                }
                if self.check(&TokenType::Greater) {
                    self.advance(); // consume '>'
                    break;
                }
                if self.is_at_end() { break; }
            }
        }

        self.consume(&TokenType::LParen, "Expected '('");
        let mut params = Vec::new();
        if !self.check(&TokenType::RParen) {
            params.push(self.parameter());
            while self.match_token(&[TokenType::Comma]) {
                params.push(self.parameter());
            }
        }
        self.consume(&TokenType::RParen, "Expected ')'");

        let mut return_type = None;
        if self.check(&TokenType::Colon) {
            self.advance();
            return_type = Some(self.consume_identifier("Expected return type"));
        }

        while self.check(&TokenType::Newline) { self.advance(); }

        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::TaskDef {
            name,
            lifetime_params,
            params,
            return_type,
            body,
            exported,
        };
        self.arena.alloc_stmt(kind, span)
    }

    fn return_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'return'
        let value = if self.check(&TokenType::Newline) || self.check(&TokenType::End) {
            None
        } else {
            Some(self.expression())
        };
        let kind = StmtKind::Return { value };
        self.arena.alloc_stmt(kind, span)
    }

    fn if_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'if'
        let condition = self.expression();
        self.consume(&TokenType::Then, "Expected 'then'");
        while self.check(&TokenType::Newline) { self.advance(); }

        let mut then_branch = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                then_branch.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::If {
            condition,
            then_branch,
            else_branch: vec![],
        };
        self.arena.alloc_stmt(kind, span)
    }

    fn for_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'for'
        let var = self.consume_identifier("Expected variable name");
        let mut var_type = None;
        if self.match_token(&[TokenType::Colon]) {
            var_type = Some(self.consume_identifier("Expected type"));
        }
        self.consume(&TokenType::In, "Expected 'in'");
        let iterable = self.expression();
        while self.check(&TokenType::Newline) { self.advance(); }

        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::For {
            var,
            var_type,
            iterable,
            body,
        };
        self.arena.alloc_stmt(kind, span)
    }

    fn import_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'import'
        let path = self.consume_identifier("Expected import path");
        let kind = StmtKind::Import { path };
        self.arena.alloc_stmt(kind, span)
    }

    fn check_index_assignment(&mut self) -> bool {
        let save = self.current;
        let result = if let Some(Token { token_type: TokenType::Identifier(_), .. }) = self.peek() {
            self.advance();
            self.match_token(&[TokenType::LBracket])
        } else {
            false
        };
        self.current = save;
        result
    }

    fn index_assignment(&mut self) -> NodeId {
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected identifier");
        self.consume(&TokenType::LBracket, "Expected '['");
        let index = self.expression();
        self.consume(&TokenType::RBracket, "Expected ']'");
        self.consume(&TokenType::Assign, "Expected '='");
        let value = self.expression();
        let object = self.arena.alloc_expr(ExprKind::Variable(name), span);
        let kind = StmtKind::IndexAssign { object, index, value };
        self.arena.alloc_stmt(kind, span)
    }

    fn check_assignment(&self) -> bool {
        if let Some(Token { token_type: TokenType::Identifier(_), .. }) = self.peek()
            && let Some(Token { token_type: TokenType::Assign, .. }) = self.peek_next() {
            return true;
        }
        false
    }

    fn assignment_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected identifier");
        self.advance(); // consume '='
        let value = self.expression();
        let kind = StmtKind::Assign { name, value };
        self.arena.alloc_stmt(kind, span)
    }

    fn expression_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        let expr = self.expression();
        let kind = StmtKind::Expr(expr);
        self.arena.alloc_stmt(kind, span)
    }

    fn match_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'match'
        let expr = self.expression();
        self.consume(&TokenType::WithKeyword, "Expected 'with'");

        let mut arms = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) { self.advance(); }
            if self.check(&TokenType::End) { break; }
            // 简化：只支持变量模式
            let pattern = self.consume_identifier("Expected pattern");
            let arm_expr = self.expression();
            arms.push((Pattern::Variable(pattern), vec![arm_expr]));
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::Match { expr, arms };
        self.arena.alloc_stmt(kind, span)
    }

    fn with_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'with'

        let mut bindings = Vec::new();
        loop {
            let key = self.consume_identifier("Expected config key");
            self.consume(&TokenType::Assign, "Expected '='");
            let value = self.expression();
            bindings.push((key, value));
            if !self.match_token(&[TokenType::Comma]) { break; }
        }

        while self.check(&TokenType::Newline) { self.advance(); }

        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::With { bindings, body };
        self.arena.alloc_stmt(kind, span)
    }

    fn parallel_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'parallel'
        while self.check(&TokenType::Newline) { self.advance(); }

        let mut stmts = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            // v0.19: 检查 worker 声明
            if self.check(&TokenType::Worker) {
                stmts.push(self.worker_statement());
            } else if let Some(stmt_id) = self.declaration() {
                stmts.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::Parallel { stmts };
        self.arena.alloc_stmt(kind, span)
    }

    fn worker_statement(&mut self) -> NodeId {
        self.advance(); // consume 'worker'
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected worker name");
        while self.check(&TokenType::Newline) { self.advance(); }

        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::Worker { name, body };
        self.arena.alloc_stmt(kind, span)
    }

    fn transaction_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'transaction'
        while self.check(&TokenType::Newline) { self.advance(); }

        let mut body = Vec::new();
        let mut compensation = Vec::new();
        let mut in_compensation = false;

        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if self.check(&TokenType::Compensation) {
                self.advance();
                in_compensation = true;
                while self.check(&TokenType::Newline) { self.advance(); }
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                if in_compensation {
                    compensation.push(stmt_id);
                } else {
                    body.push(stmt_id);
                }
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::Transaction { body, compensation };
        self.arena.alloc_stmt(kind, span)
    }

    fn macro_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'macro'
        let name = self.consume_identifier("Expected macro name");
        self.consume(&TokenType::LParen, "Expected '('");
        let mut params = Vec::new();
        if !self.check(&TokenType::RParen) {
            params.push(self.consume_identifier("Expected parameter"));
            while self.match_token(&[TokenType::Comma]) {
                params.push(self.consume_identifier("Expected parameter"));
            }
        }
        self.consume(&TokenType::RParen, "Expected ')'");
        while self.check(&TokenType::Newline) { self.advance(); }

        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::MacroDef { name, params, body };
        self.arena.alloc_stmt(kind, span)
    }

    fn route_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'route'
        let method = self.consume_identifier("Expected method");
        let path = self.consume_identifier("Expected path");
        self.consume(&TokenType::Arrow, "Expected '->'");
        let target = self.expression();
        let kind = StmtKind::Route { name: format!("{} {}", method, path), target };
        self.arena.alloc_stmt(kind, span)
    }

    fn trait_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'trait'
        let name = self.consume_identifier("Expected trait name");
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_generic_params()
        } else {
            vec![]
        };
        let parents = if self.match_token(&[TokenType::Colon]) {
            let mut ps = vec![self.consume_identifier("Expected parent trait name")];
            while self.match_token(&[TokenType::Comma]) {
                ps.push(self.consume_identifier("Expected parent trait name"));
            }
            ps
        } else {
            vec![]
        };
        let trait_where = if self.match_token(&[TokenType::Where]) {
            self.parse_where_clause()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut methods = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) { self.advance(); }
            if self.check(&TokenType::End) || self.is_at_end() { break; }
            if !self.check(&TokenType::Fn) { break; }
            self.advance(); // consume 'fn'
            let mname = self.consume_identifier("Expected method name");
            self.consume(&TokenType::LParen, "Expected '('");
            let params = if self.check(&TokenType::RParen) {
                vec![]
            } else {
                let mut p = vec![self.parameter()];
                while self.match_token(&[TokenType::Comma]) {
                    p.push(self.parameter());
                }
                p
            };
            self.consume(&TokenType::RParen, "Expected ')'");
            let return_type = if self.match_token(&[TokenType::Arrow]) {
                Some(self.consume_identifier("Expected return type"))
            } else if self.check(&TokenType::Colon) {
                self.advance();
                Some(self.consume_identifier("Expected return type"))
            } else {
                None
            };
            // 支持 `= expr` 单行方法和 `do ... end` 多行方法
            let body = if self.match_token(&[TokenType::Assign]) {
                let expr = self.expression();
                let span = self.span_of_current();
                vec![self.arena.alloc_stmt(StmtKind::Expr(expr), span)]
            } else if self.match_token(&[TokenType::Do]) {
                let mut body = Vec::new();
                while !self.check(&TokenType::End) && !self.is_at_end() {
                    if self.check(&TokenType::Newline) { self.advance(); continue; }
                    if let Some(stmt_id) = self.declaration() {
                        body.push(stmt_id);
                    }
                }
                self.consume(&TokenType::End, "Expected 'end'");
                body
            } else {
                vec![]
            };
            while self.check(&TokenType::Newline) { self.advance(); }
            methods.push(TraitMethod {
                name: mname,
                params,
                return_type,
                body,
                generics: vec![],
                span: self.span_of_current(),
            });
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::TraitDef { name, generics, parents, trait_where, methods };
        self.arena.alloc_stmt(kind, span)
    }

    fn impl_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'impl'
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_generic_params()
        } else {
            vec![]
        };
        let trait_name = self.consume_identifier("Expected trait name");
        let trait_generics = if self.check(&TokenType::Less) && self.peek_type_list_can_close() {
            self.parse_type_list()
        } else {
            vec![]
        };
        self.consume(&TokenType::For, "Expected 'for'");
        let for_type = self.consume_identifier("Expected type");
        let for_generics = if self.check(&TokenType::Less) && self.peek_type_list_can_close() {
            self.parse_type_list()
        } else {
            vec![]
        };
        let where_clause = if self.match_token(&[TokenType::Where]) {
            self.parse_where_clause()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut methods = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) { self.advance(); }
            if self.check(&TokenType::End) || self.is_at_end() { break; }
            if !self.check(&TokenType::Fn) { break; }
            self.advance(); // consume 'fn'
            let mname = self.consume_identifier("Expected method name");
            self.consume(&TokenType::LParen, "Expected '('");
            let params = if self.check(&TokenType::RParen) {
                vec![]
            } else {
                let mut p = vec![self.parameter()];
                while self.match_token(&[TokenType::Comma]) {
                    p.push(self.parameter());
                }
                p
            };
            self.consume(&TokenType::RParen, "Expected ')'");
            let return_type = if self.match_token(&[TokenType::Arrow]) {
                Some(self.consume_identifier("Expected return type"))
            } else if self.check(&TokenType::Colon) {
                self.advance();
                Some(self.consume_identifier("Expected return type"))
            } else {
                None
            };
            // 支持 `= expr` 单行方法、`= do ... end` 和 `do ... end` 多行方法
            let body = if self.match_token(&[TokenType::Assign]) {
                if self.check(&TokenType::Do) {
                    // `= do ... end` 混合语法
                    self.advance(); // consume 'do'
                    let mut b = Vec::new();
                    while !self.check(&TokenType::End) && !self.is_at_end() {
                        while self.check(&TokenType::Newline) { self.advance(); }
                        if self.check(&TokenType::End) || self.is_at_end() { break; }
                        if let Some(stmt_id) = self.declaration() {
                            b.push(stmt_id);
                        }
                    }
                    self.consume(&TokenType::End, "Expected 'end'");
                    b
                } else {
                    let expr = self.expression();
                    let span = self.span_of_current();
                    vec![self.arena.alloc_stmt(StmtKind::Expr(expr), span)]
                }
            } else if self.match_token(&[TokenType::Do]) {
                let mut b = Vec::new();
                while !self.check(&TokenType::End) && !self.is_at_end() {
                    while self.check(&TokenType::Newline) { self.advance(); }
                    if self.check(&TokenType::End) || self.is_at_end() { break; }
                    if let Some(stmt_id) = self.declaration() {
                        b.push(stmt_id);
                    }
                }
                self.consume(&TokenType::End, "Expected 'end'");
                b
            } else {
                vec![]
            };
            while self.check(&TokenType::Newline) { self.advance(); }
            methods.push(FnDef {
                name: mname,
                params,
                return_type,
                body,
                span: self.span_of_current(),
            });
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::ImplDef { generics, trait_generics, trait_name, for_type, for_generics, where_clause, methods };
        self.arena.alloc_stmt(kind, span)
    }

    fn type_alias_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'type'
        let name = self.consume_identifier("Expected type name");
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_type_list()
        } else {
            vec![]
        };
        self.consume(&TokenType::Assign, "Expected '='");
        let target = self.consume_identifier("Expected target type");
        let kind = StmtKind::TypeAlias { name, generics, target };
        self.arena.alloc_stmt(kind, span)
    }

    fn enum_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'enum'
        let name = self.consume_identifier("Expected enum name");
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_type_list()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut variants = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) { self.advance(); }
            if self.check(&TokenType::End) || self.is_at_end() { break; }
            let vname = self.consume_identifier("Expected variant name");
            let vtype = if self.check(&TokenType::LParen) {
                self.advance();
                let t = self.consume_identifier("Expected variant type");
                self.consume(&TokenType::RParen, "Expected ')'");
                Some(t)
            } else {
                None
            };
            variants.push(crate::ast::EnumVariant { name: vname, data: vtype });
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::EnumDef { name, generics, variants };
        self.arena.alloc_stmt(kind, span)
    }

    fn struct_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'struct'
        let name = self.consume_identifier("Expected struct name");
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_type_list()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut fields = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) { self.advance(); }
            if self.check(&TokenType::End) || self.is_at_end() { break; }
            let fname = self.consume_identifier("Expected field name");
            self.consume(&TokenType::Colon, "Expected ':'");
            let ftype = self.consume_identifier("Expected field type");
            fields.push(crate::ast::StructField { name: fname, type_hint: ftype });
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::StructDef { name, generics, fields };
        self.arena.alloc_stmt(kind, span)
    }

    fn save_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'save'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let value = self.expression();
        let kind = StmtKind::Save { path, value };
        self.arena.alloc_stmt(kind, span)
    }

    fn load_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'load'
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into'");
        let var = self.consume_identifier("Expected variable name");
        let kind = StmtKind::Load { path, var };
        self.arena.alloc_stmt(kind, span)
    }

    fn read_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'read'
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into'");
        let var = self.consume_identifier("Expected variable name");
        let kind = StmtKind::ReadFile { path, var };
        self.arena.alloc_stmt(kind, span)
    }

    fn write_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'write'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let content = self.expression();
        let kind = StmtKind::WriteFile { path, content };
        self.arena.alloc_stmt(kind, span)
    }

    fn append_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'append'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let content = self.expression();
        let kind = StmtKind::AppendFile { path, content };
        self.arena.alloc_stmt(kind, span)
    }

    fn read_bytes_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'read_bytes'
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into'");
        let var = self.consume_identifier("Expected variable name");
        let kind = StmtKind::ReadBytesFile { path, var };
        self.arena.alloc_stmt(kind, span)
    }

    fn write_bytes_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'write_bytes'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let content = self.expression();
        let kind = StmtKind::WriteBytesFile { path, content };
        self.arena.alloc_stmt(kind, span)
    }

    fn stream_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'stream'
        let prompt = self.expression();
        self.consume(&TokenType::As, "Expected 'as'");
        let var = self.consume_identifier("Expected variable name");
        self.consume(&TokenType::Do, "Expected 'do'");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::StreamFor { prompt, var, body };
        self.arena.alloc_stmt(kind, span)
    }

    fn tool_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'tool'
        let name = self.consume_identifier("Expected tool name");
        let params = if self.match_token(&[TokenType::LParen]) {
            let mut p = Vec::new();
            if !self.check(&TokenType::RParen) {
                p.push(self.parameter());
                while self.match_token(&[TokenType::Comma]) {
                    p.push(self.parameter());
                }
            }
            self.consume(&TokenType::RParen, "Expected ')'");
            p
        } else {
            Vec::new()
        };
        let return_type = if self.match_token(&[TokenType::Colon]) {
            Some(self.consume_identifier("Expected return type"))
        } else {
            None
        };
        self.consume(&TokenType::Do, "Expected 'do'");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::ToolDef {
            name,
            params,
            return_type,
            body,
            exported: false,
        };
        self.arena.alloc_stmt(kind, span)
    }

    fn observe_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'observe'
        let config = if self.match_token(&[TokenType::Trace]) {
            ObserveConfig::Trace
        } else if self.match_token(&[TokenType::Metrics]) {
            ObserveConfig::Metrics
        } else if self.match_token(&[TokenType::Otel]) {
            self.consume_identifier("Expected 'endpoint'");
            let endpoint = if let Some(Token { token_type: TokenType::String(s), .. }) = self.peek().cloned() {
                self.advance();
                self.arena.alloc_expr(ExprKind::Literal(Literal::String(s, self.span_of_current())), self.span_of_current())
            } else {
                panic!("Expected string endpoint");
            };
            ObserveConfig::Otel { endpoint }
        } else {
            panic!("Expected trace / metrics / otel after 'observe'");
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let body = if self.match_token(&[TokenType::Do]) {
            let mut b = Vec::new();
            while !self.check(&TokenType::End) && !self.is_at_end() {
                if self.check(&TokenType::Newline) {
                    self.advance();
                    continue;
                }
                if let Some(stmt_id) = self.declaration() {
                    b.push(stmt_id);
                }
            }
            self.consume(&TokenType::End, "Expected 'end'");
            b
        } else {
            self.consume(&TokenType::End, "Expected 'end'");
            Vec::new()
        };
        let kind = StmtKind::Observe { config, body };
        self.arena.alloc_stmt(kind, span)
    }

    fn span_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'span'
        let name = match self.advance() {
            Some(Token { token_type: TokenType::String(s), .. }) => s.clone(),
            _ => panic!("Expected span name string"),
        };
        let attributes = if self.match_token(&[TokenType::Tags]) {
            self.consume(&TokenType::LBrace, "Expected '{'");
            let mut attrs = Vec::new();
            loop {
                let key = match self.advance() {
                    Some(Token { token_type: TokenType::Identifier(n), .. }) => n.clone(),
                    Some(Token { token_type: TokenType::String(s), .. }) => s.clone(),
                    _ => panic!("Expected tag key"),
                };
                self.consume(&TokenType::Colon, "Expected ':'");
                let val = self.expression();
                attrs.push((key, val));
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(&TokenType::RBrace, "Expected '}'");
            attrs
        } else {
            Vec::new()
        };
        self.consume(&TokenType::Do, "Expected 'do'");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::Span { name, attributes, body };
        self.arena.alloc_stmt(kind, span)
    }

    fn record_tokens_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        // record_tokens 已经被 match_identifier 消耗
        self.consume(&TokenType::LParen, "Expected '(' after 'record_tokens'");
        let input = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let output = self.expression();
        self.consume(&TokenType::RParen, "Expected ')'");
        let kind = StmtKind::RecordTokens { input, output };
        self.arena.alloc_stmt(kind, span)
    }

    // ===================================================================
    // 表达式
    // ===================================================================

    fn expression(&mut self) -> NodeId {
        self.pipe()
    }

    fn pipe(&mut self) -> NodeId {
        let mut left = self.call();
        while self.match_token(&[TokenType::Pipe]) {
            let right = self.call();
            let kind = ExprKind::Pipe { left, right };
            left = self.arena.alloc_expr(kind, self.span_of_current());
        }
        left
    }

    fn call(&mut self) -> NodeId {
        let mut expr = self.binary();
        loop {
            if self.check(&TokenType::LParen) {
                // 函数调用
                let span = self.span_of_current();
                self.advance();
                // 检查是否是 ai_model 调用
                if let Some(e) = self.arena.get_expr(expr)
                    && let ExprKind::Variable(name) = &e.kind
                    && name == "ai_model" {
                        expr = self.parse_ai_model_call(span);
                        continue;
                    }
                let mut args = Vec::new();
                if !self.check(&TokenType::RParen) {
                    args.push(self.expression());
                    while self.match_token(&[TokenType::Comma]) {
                        args.push(self.expression());
                    }
                }
                self.consume(&TokenType::RParen, "Expected ')'");
                // 检查是否是变量调用
                if let Some(e) = self.arena.get_expr(expr) {
                    match &e.kind {
                        ExprKind::Variable(name) => {
                            let kind = ExprKind::Call {
                                callee: name.clone(),
                                args,
                            };
                            expr = self.arena.alloc_expr(kind, span);
                            continue;
                        }
                        ExprKind::NamespaceRef { namespace, name } => {
                            let callee = format!("{}::{}", namespace, name);
                            let kind = ExprKind::Call {
                                callee,
                                args,
                            };
                            expr = self.arena.alloc_expr(kind, span);
                            continue;
                        }
                        _ => {}
                    }
                }
            } else if self.check(&TokenType::Dot) {
                // 方法调用
                self.advance(); // consume '.'
                let method = self.consume_method_name("Expected method name");
                let mut args = Vec::new();
                if self.check(&TokenType::LParen) {
                    self.advance();
                    if !self.check(&TokenType::RParen) {
                        args.push(self.expression());
                        while self.match_token(&[TokenType::Comma]) {
                            args.push(self.expression());
                        }
                    }
                    self.consume(&TokenType::RParen, "Expected ')'");
                }
                let span = self.span_of_current();
                let kind = ExprKind::MethodCall {
                    object: expr,
                    method,
                    args,
                };
                expr = self.arena.alloc_expr(kind, span);
            } else if self.check(&TokenType::LBracket) {
                // 索引访问
                self.advance(); // consume '['
                let index = self.expression();
                self.consume(&TokenType::RBracket, "Expected ']'");
                let span = self.span_of_current();
                let kind = ExprKind::Index { object: expr, index };
                expr = self.arena.alloc_expr(kind, span);
            } else if self.check(&TokenType::Question) {
                // 错误传播
                self.advance(); // consume '?'
                let span = self.span_of_current();
                let kind = ExprKind::Question { expr };
                expr = self.arena.alloc_expr(kind, span);
            } else {
                break;
            }
        }
        expr
    }

    fn closure_expression(&mut self, span: Span) -> NodeId {
        self.advance(); // consume 'fn'
        self.consume(&TokenType::LParen, "Expected '('");
        let mut params = Vec::new();
        if !self.check(&TokenType::RParen) {
            params.push(self.parameter());
            while self.match_token(&[TokenType::Comma]) {
                params.push(self.parameter());
            }
        }
        self.consume(&TokenType::RParen, "Expected ')'");

        let mut return_type = None;
        if self.match_token(&[TokenType::Colon]) {
            return_type = Some(self.consume_identifier("Expected return type"));
        }

        while self.check(&TokenType::Newline) { self.advance(); }

        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = ExprKind::Closure {
            params,
            return_type,
            body,
        };
        self.arena.alloc_expr(kind, span)
    }

    fn binary(&mut self) -> NodeId {
        let mut left = self.unary();
        while self.match_binary_op() {
            let op = self.previous_binary_op();
            let right = self.unary();
            let kind = ExprKind::Binary {
                left,
                op,
                right,
            };
            left = self.arena.alloc_expr(kind, self.span_of_current());
        }
        left
    }

    fn unary(&mut self) -> NodeId {
        if self.check(&TokenType::Match) {
            self.match_expression()
        } else {
            self.primary()
        }
    }

    fn match_expression(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'match'
        let expr = self.expression();
        self.consume(&TokenType::WithKeyword, "Expected 'with'");

        let mut arms = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) { self.advance(); }
            if self.check(&TokenType::End) { break; }
            let mut pattern = self.pattern();
            // v0.16: 解析 when 守卫条件
            if self.peek_is_identifier("when") {
                self.advance(); // consume 'when'
                let condition = self.expression();
                pattern = Pattern::Guard {
                    pattern: Box::new(pattern),
                    condition,
                };
            }
            self.consume(&TokenType::Arrow, "Expected '->'");
            let arm_expr = self.expression();
            arms.push((pattern, arm_expr));
            while self.check(&TokenType::Newline) { self.advance(); }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = ExprKind::Match { expr, arms };
        self.arena.alloc_expr(kind, span)
    }

    fn pattern(&mut self) -> Pattern {
        if let Some(Token { token_type: TokenType::Identifier(name), .. }) = self.peek().cloned()
            && name == "_" {
            self.advance();
            return Pattern::Wildcard;
        }

        if self.match_token(&[TokenType::True]) {
            Pattern::Literal(Literal::Bool(true, Span::default()))
        } else if self.match_token(&[TokenType::False]) {
            Pattern::Literal(Literal::Bool(false, Span::default()))
        } else if self.match_token(&[TokenType::Nil]) {
            Pattern::Literal(Literal::Nil(Span::default()))
        } else if let Some(Token { token_type: TokenType::Number(n), .. }) = self.peek().cloned() {
            self.advance();
            Pattern::Literal(Literal::Number(n, Span::default()))
        } else if let Some(Token { token_type: TokenType::String(s), .. }) = self.peek().cloned() {
            self.advance();
            Pattern::Literal(Literal::String(s, Span::default()))
        } else if self.match_token(&[TokenType::LBracket]) {
            let mut items = Vec::new();
            let mut rest = None;
            if !self.check(&TokenType::RBracket) {
                if self.check(&TokenType::DotDotDot) {
                    self.advance();
                    rest = Some(self.consume_identifier("Expected variable name after '...'"));
                } else {
                    items.push(self.pattern());
                    while self.match_token(&[TokenType::Comma]) {
                        if self.check(&TokenType::DotDotDot) {
                            self.advance();
                            rest = Some(self.consume_identifier("Expected variable name after '...'"));
                            break;
                        }
                        items.push(self.pattern());
                    }
                }
            }
            self.consume(&TokenType::RBracket, "Expected ']'");
            Pattern::List { prefix: items, rest }
        } else if self.match_token(&[TokenType::LBrace]) {
            let mut entries = Vec::new();
            if !self.check(&TokenType::RBrace) {
                entries.push(self.dict_pattern_entry());
                while self.match_token(&[TokenType::Comma]) {
                    entries.push(self.dict_pattern_entry());
                }
            }
            self.consume(&TokenType::RBrace, "Expected '}'");
            Pattern::Dict(entries)
        } else if let Some(Token { token_type: TokenType::Identifier(name), .. }) = self.peek().cloned() {
            self.advance();
            Pattern::Variable(name)
        } else {
            Pattern::Wildcard
        }
    }

    fn dict_pattern_entry(&mut self) -> (String, Pattern) {
        let key = match self.peek().cloned() {
            Some(Token { token_type: TokenType::Identifier(name), .. }) => {
                self.advance();
                name
            }
            Some(Token { token_type: TokenType::String(s), .. }) => {
                self.advance();
                s
            }
            _ => panic!("Expected pattern key"),
        };
        self.consume(&TokenType::Colon, "Expected ':'");
        let pattern = self.pattern();
        (key, pattern)
    }

    fn primary(&mut self) -> NodeId {
        let span = self.span_of_current();

        if self.match_token(&[TokenType::True]) {
            self.arena.alloc_expr(ExprKind::Literal(Literal::Bool(true, span)), span)
        } else if self.match_token(&[TokenType::False]) {
            self.arena.alloc_expr(ExprKind::Literal(Literal::Bool(false, span)), span)
        } else if self.match_token(&[TokenType::Nil]) {
            self.arena.alloc_expr(ExprKind::Literal(Literal::Nil(span)), span)
        } else if let Some(Token { token_type: TokenType::Number(n), .. }) = self.peek().cloned() {
            self.advance();
            self.arena.alloc_expr(ExprKind::Literal(Literal::Number(n, span)), span)
        } else if let Some(Token { token_type: TokenType::Char(ch), .. }) = self.peek().cloned() {
            self.advance();
            self.arena.alloc_expr(ExprKind::Literal(Literal::Char(ch, span)), span)
        } else if let Some(Token { token_type: TokenType::String(s), .. }) = self.peek().cloned() {
            self.advance();
            if has_format_interpolation(&s) {
                self.parse_format_string(&s, span)
            } else {
                self.arena.alloc_expr(ExprKind::Literal(Literal::String(s, span)), span)
            }
        } else if let Some(Token { token_type: TokenType::Identifier(name), .. }) = self.peek().cloned() {
            self.advance();
            let mut ns_or_name = name;
            // 检查是否带泛型 <T, U>
            if self.check(&TokenType::Less) && self.peek_type_list_can_close() {
                let generics = self.parse_type_list();
                ns_or_name = format!("{}<{}>", ns_or_name, generics.join(","));
            }
            // 检查是否是 NamespaceRef (IDENT::IDENT)
            if self.match_token(&[TokenType::ColonColon]) {
                let method = self.consume_identifier("Expected name after '::'");
                let kind = ExprKind::NamespaceRef { namespace: ns_or_name, name: method };
                self.arena.alloc_expr(kind, span)
            } else {
                self.arena.alloc_expr(ExprKind::Variable(ns_or_name), span)
            }
        } else if self.check(&TokenType::LParen) {
            self.advance();
            let expr = self.expression();
            self.consume(&TokenType::RParen, "Expected ')'");
            self.arena.alloc_expr(ExprKind::Grouping(expr), span)
        } else if self.check(&TokenType::Fn) {
            self.closure_expression(span)
        } else if let Some(Token { token_type: TokenType::PromptString(s), .. }) = self.peek().cloned() {
            self.advance();
            let inner = if has_format_interpolation(&s) {
                self.parse_format_string(&s, span)
            } else {
                self.arena.alloc_expr(ExprKind::Literal(Literal::String(s, span)), span)
            };
            // 无论是否有插值，都包成 Prompt 节点，让解释器走 ai.chat
            let parts = self.flatten_prompt_parts(inner);
            let kind = ExprKind::Prompt { parts };
            self.arena.alloc_expr(kind, span)
        } else if self.check(&TokenType::LBracket) {
            self.list_literal(span)
        } else if self.check(&TokenType::LBrace) {
            self.dict_literal(span)
        } else {
            self.advance(); // skip unknown token
            self.arena.alloc_expr(ExprKind::Literal(Literal::Nil(span)), span)
        }
    }

    fn list_literal(&mut self, span: Span) -> NodeId {
        self.advance(); // consume '['
        let mut items = Vec::new();
        if !self.check(&TokenType::RBracket) {
            items.push(self.expression());
            while self.match_token(&[TokenType::Comma]) {
                if self.check(&TokenType::RBracket) { break; }
                items.push(self.expression());
            }
        }
        self.consume(&TokenType::RBracket, "Expected ']'");
        self.arena.alloc_expr(ExprKind::List(items), span)
    }

    fn dict_literal(&mut self, span: Span) -> NodeId {
        self.advance(); // consume '{'
        let mut entries = Vec::new();
        while self.check(&TokenType::Newline) { self.advance(); }
        if !self.check(&TokenType::RBrace) {
            let (key, val) = self.dict_entry();
            entries.push((key, val));
            while self.match_token(&[TokenType::Comma]) {
                while self.check(&TokenType::Newline) { self.advance(); }
                if self.check(&TokenType::RBrace) { break; }
                let (key, val) = self.dict_entry();
                entries.push((key, val));
            }
        }
        while self.check(&TokenType::Newline) { self.advance(); }
        self.consume(&TokenType::RBrace, "Expected '}'");
        self.arena.alloc_expr(ExprKind::Dict(entries), span)
    }

    fn dict_entry(&mut self) -> (String, NodeId) {
        let key = match self.peek().cloned() {
            Some(Token { token_type: TokenType::Identifier(name), .. }) => {
                self.advance();
                name
            }
            Some(Token { token_type: TokenType::String(s), .. }) => {
                self.advance();
                s
            }
            _ => panic!("Expected dict key"),
        };
        self.consume(&TokenType::Colon, "Expected ':'");
        let val = self.expression();
        (key, val)
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
        if self.check(&TokenType::Plus) || self.check(&TokenType::Minus) ||
           self.check(&TokenType::Star) || self.check(&TokenType::Slash) ||
           self.check(&TokenType::Percent) || self.check(&TokenType::Equal) ||
           self.check(&TokenType::NotEqual) || self.check(&TokenType::Greater) ||
           self.check(&TokenType::Less) || self.check(&TokenType::GreaterEqual) ||
           self.check(&TokenType::LessEqual) {
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
            || self.tokens.get(self.current).map(|t| t.token_type == TokenType::EOF).unwrap_or(true)
    }

    fn check(&self, token_type: &TokenType) -> bool {
        self.peek().map(|t| &t.token_type == token_type).unwrap_or(false)
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
            eprintln!("Parse error: {} at line {}", message,
                self.peek().map(|t| t.line).unwrap_or(0));
        }
    }

    fn consume_identifier(&mut self, message: &str) -> String {
        match self.peek().cloned() {
            Some(Token { token_type: TokenType::Identifier(name), .. }) => {
                self.advance();
                name
            }
            _ => {
                eprintln!("Parse error: {} at line {}", message,
                    self.peek().map(|t| t.line).unwrap_or(0));
                String::new()
            }
        }
    }

    fn span_of_current(&self) -> Span {
        self.peek().map(|t| Span { line: t.line, column: t.column }).unwrap_or(Span { line: 0, column: 0 })
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
        let is_less = self.peek().map(|t| matches!(t.token_type, TokenType::Less)).unwrap_or(false);
        let next_is_ident = self.peek_next().map(|t| matches!(t.token_type, TokenType::Identifier(_))).unwrap_or(false);
        is_less && next_is_ident
    }

    fn peek_type_list_can_close(&self) -> bool {
        let tokens = &self.tokens;
        let start = self.current;
        if !matches!(tokens.get(start).map(|t| &t.token_type), Some(TokenType::Less)) {
            return false;
        }
        fn skip_type(tokens: &[crate::lexer::Token], mut i: usize) -> Option<usize> {
            match tokens.get(i).map(|t| &t.token_type) {
                Some(TokenType::Identifier(_)) => { i += 1; }
                _ => return None,
            }
            if matches!(tokens.get(i).map(|t| &t.token_type), Some(TokenType::Less)) {
                i += 1;
                i = skip_type(tokens, i)?;
                loop {
                    match tokens.get(i).map(|t| &t.token_type) {
                        Some(TokenType::Greater) => { i += 1; break; }
                        Some(TokenType::Comma) => { i += 1; }
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
                Some(TokenType::Comma) => { i += 1; }
                _ => return false,
            }
            i = match skip_type(tokens, i) {
                Some(v) => v,
                None => return false,
            };
        }
    }

    fn parse_generic_params(&mut self) -> Vec<crate::ast::GenericParam> {
        use crate::ast::GenericParam;
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
            params.push(GenericParam { name: pname, bound: pbound, span: pspan });
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

    fn parse_where_clause(&mut self) -> Vec<crate::ast::GenericParam> {
        use crate::ast::GenericParam;
        let mut clauses = Vec::new();
        loop {
            let pspan = self.span_of_current();
            let pname = self.consume_identifier("Expected where clause param name");
            self.consume(&TokenType::Colon, "Expected ':'");
            let pbound = Some(self.consume_identifier("Expected bound trait name"));
            clauses.push(GenericParam { name: pname, bound: pbound, span: pspan });
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        clauses
    }

    fn consume_method_name(&mut self, message: &str) -> String {
        match self.peek().cloned() {
            Some(Token { token_type: TokenType::Identifier(name), .. }) => {
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
                        parts.push(self.arena.alloc_expr(ExprKind::Literal(Literal::String(current.clone(), span)), span));
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
            parts.push(self.arena.alloc_expr(ExprKind::Literal(Literal::String(current, span)), span));
        }

        if parts.is_empty() {
            self.arena.alloc_expr(ExprKind::Literal(Literal::String(String::new(), span)), span)
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
