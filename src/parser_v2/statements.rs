use super::*;

impl ParserV2 {
    pub(super) fn let_declaration_exported(&mut self, exported: bool) -> NodeId {
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

    pub(super) fn task_declaration_exported(&mut self, exported: bool) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'task'
        let name = self.consume_identifier("Expected task name");

        // 解析生命周期参数
        let mut lifetime_params = Vec::new();
        if self.check(&TokenType::Less) {
            self.advance(); // consume '<'
            loop {
                if let Some(Token {
                    token_type: TokenType::Lifetime(lt),
                    ..
                }) = self.peek().cloned()
                {
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
                if self.is_at_end() {
                    break;
                }
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

    pub(super) fn return_statement(&mut self) -> NodeId {
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

    pub(super) fn if_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'if'
        let condition = self.expression();
        self.consume(&TokenType::Then, "Expected 'then'");
        while self.check(&TokenType::Newline) {
            self.advance();
        }

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

    pub(super) fn for_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'for'
        let var = self.consume_identifier("Expected variable name");
        let mut var_type = None;
        if self.match_token(&[TokenType::Colon]) {
            var_type = Some(self.consume_identifier("Expected type"));
        }
        self.consume(&TokenType::In, "Expected 'in'");
        let iterable = self.expression();
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

        let kind = StmtKind::For {
            var,
            var_type,
            iterable,
            body,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn import_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'import'
        let path = self.consume_identifier("Expected import path");
        let kind = StmtKind::Import { path };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn check_index_assignment(&mut self) -> bool {
        let save = self.current;
        let result = if let Some(Token {
            token_type: TokenType::Identifier(_),
            ..
        }) = self.peek()
        {
            self.advance();
            self.match_token(&[TokenType::LBracket])
        } else {
            false
        };
        self.current = save;
        result
    }

    pub(super) fn index_assignment(&mut self) -> NodeId {
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected identifier");
        self.consume(&TokenType::LBracket, "Expected '['");
        let index = self.expression();
        self.consume(&TokenType::RBracket, "Expected ']'");
        self.consume(&TokenType::Assign, "Expected '='");
        let value = self.expression();
        let object = self.arena.alloc_expr(ExprKind::Variable(name), span);
        let kind = StmtKind::IndexAssign {
            object,
            index,
            value,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn check_assignment(&self) -> bool {
        if let Some(Token {
            token_type: TokenType::Identifier(_),
            ..
        }) = self.peek()
            && let Some(Token {
                token_type: TokenType::Assign,
                ..
            }) = self.peek_next()
        {
            return true;
        }
        false
    }

    pub(super) fn assignment_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected identifier");
        self.advance(); // consume '='
        let value = self.expression();
        let kind = StmtKind::Assign { name, value };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn expression_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        let expr = self.expression();
        let kind = StmtKind::Expr(expr);
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn match_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'match'
        let expr = self.expression();
        self.consume(&TokenType::WithKeyword, "Expected 'with'");

        let mut arms = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) {
                break;
            }
            // 简化：只支持变量模式
            let pattern = self.consume_identifier("Expected pattern");
            let arm_expr = self.expression();
            arms.push((Pattern::Variable(pattern), vec![arm_expr]));
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::Match { expr, arms };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn with_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'with'

        let mut bindings = Vec::new();
        loop {
            let key = self.consume_identifier("Expected config key");
            self.consume(&TokenType::Assign, "Expected '='");
            let value = self.expression();
            bindings.push((key, value));
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }

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

        let kind = StmtKind::With { bindings, body };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn parallel_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'parallel'
        while self.check(&TokenType::Newline) {
            self.advance();
        }

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

    pub(super) fn worker_statement(&mut self) -> NodeId {
        self.advance(); // consume 'worker'
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected worker name");
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

        let kind = StmtKind::Worker { name, body };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn transaction_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'transaction'
        while self.check(&TokenType::Newline) {
            self.advance();
        }

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
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
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

    pub(super) fn macro_statement(&mut self) -> NodeId {
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

        let kind = StmtKind::MacroDef { name, params, body };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn route_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'route'
        let method = self.consume_identifier("Expected method");
        let path = self.consume_identifier("Expected path");
        self.consume(&TokenType::Arrow, "Expected '->'");
        let target = self.expression();
        let kind = StmtKind::Route {
            name: format!("{} {}", method, path),
            target,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn trait_statement(&mut self) -> NodeId {
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
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut methods = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }
            if !self.check(&TokenType::Fn) {
                break;
            }
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
                    if self.check(&TokenType::Newline) {
                        self.advance();
                        continue;
                    }
                    if let Some(stmt_id) = self.declaration() {
                        body.push(stmt_id);
                    }
                }
                self.consume(&TokenType::End, "Expected 'end'");
                body
            } else {
                vec![]
            };
            while self.check(&TokenType::Newline) {
                self.advance();
            }
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
        let kind = StmtKind::TraitDef {
            name,
            generics,
            parents,
            trait_where,
            methods,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn impl_statement(&mut self) -> NodeId {
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
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut methods = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }
            if !self.check(&TokenType::Fn) {
                break;
            }
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
                        while self.check(&TokenType::Newline) {
                            self.advance();
                        }
                        if self.check(&TokenType::End) || self.is_at_end() {
                            break;
                        }
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
                    while self.check(&TokenType::Newline) {
                        self.advance();
                    }
                    if self.check(&TokenType::End) || self.is_at_end() {
                        break;
                    }
                    if let Some(stmt_id) = self.declaration() {
                        b.push(stmt_id);
                    }
                }
                self.consume(&TokenType::End, "Expected 'end'");
                b
            } else {
                vec![]
            };
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            methods.push(FnDef {
                name: mname,
                params,
                return_type,
                body,
                span: self.span_of_current(),
            });
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::ImplDef {
            generics,
            trait_generics,
            trait_name,
            for_type,
            for_generics,
            where_clause,
            methods,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn type_alias_statement(&mut self) -> NodeId {
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
        let kind = StmtKind::TypeAlias {
            name,
            generics,
            target,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn enum_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'enum'
        let name = self.consume_identifier("Expected enum name");
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_type_list()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut variants = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }
            let vname = self.consume_identifier("Expected variant name");
            let vtype = if self.check(&TokenType::LParen) {
                self.advance();
                let t = self.consume_identifier("Expected variant type");
                self.consume(&TokenType::RParen, "Expected ')'");
                Some(t)
            } else {
                None
            };
            variants.push(crate::common::EnumVariant {
                name: vname,
                data: vtype,
            });
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::EnumDef {
            name,
            generics,
            variants,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn struct_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'struct'
        let name = self.consume_identifier("Expected struct name");
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_type_list()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut fields = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }
            let fname = self.consume_identifier("Expected field name");
            self.consume(&TokenType::Colon, "Expected ':'");
            let ftype = self.consume_identifier("Expected field type");
            fields.push(crate::common::StructField {
                name: fname,
                type_hint: ftype,
            });
        }
        self.consume(&TokenType::End, "Expected 'end'");
        let kind = StmtKind::StructDef {
            name,
            generics,
            fields,
        };
        self.arena.alloc_stmt(kind, span)
    }

    /// v0.25: orchestrate 块
    pub(super) fn orchestrate_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'orchestrate'

        // 解析模式: sequential / graph / loop
        let mode = if self.check(&TokenType::Loop) {
            self.advance();
            "loop".to_string()
        } else {
            self.consume_identifier("Expected 'sequential', 'graph', or 'loop'")
        };

        // 解析 input -> result
        let input_var = self.consume_identifier("Expected input variable name");
        self.consume(&TokenType::Arrow, "Expected '->'");
        let result_var = self.consume_identifier("Expected result variable name");

        while self.check(&TokenType::Newline) {
            self.advance();
        }

        let kind = match mode.as_str() {
            "sequential" => {
                let agents = self.parse_agent_decls();
                self.consume(&TokenType::End, "Expected 'end'");
                OrchestrateKind::Sequential { agents }
            }
            "graph" => {
                let agents = self.parse_agent_decls();
                // 解析 edges 块
                let mut edges = Vec::new();
                if self.match_identifier("edges") {
                    while !self.check(&TokenType::End) && !self.is_at_end() {
                        while self.check(&TokenType::Newline) {
                            self.advance();
                        }
                        if self.check(&TokenType::End) || self.is_at_end() {
                            break;
                        }
                        edges.push(self.parse_edge_decl());
                    }
                }
                self.consume(&TokenType::End, "Expected 'end'");
                OrchestrateKind::Graph { agents, edges }
            }
            "loop" => {
                // 解析 max_rounds
                let mut max_rounds = 10; // 默认值
                if self.match_token(&[TokenType::Comma]) && self.check(&TokenType::MaxRounds) {
                    self.advance(); // consume 'max_rounds'
                    self.consume(&TokenType::Colon, "Expected ':'");
                    if let Some(Token {
                        token_type: TokenType::Number(n),
                        ..
                    }) = self.peek().cloned()
                    {
                        max_rounds = n as usize;
                        self.advance();
                    }
                }
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                // 解析单个 agent
                let agents = self.parse_agent_decls();
                let agent = agents
                    .into_iter()
                    .next()
                    .expect("loop requires exactly one agent");
                // 解析 exit_when
                let mut exit_when = None;
                if self.check(&TokenType::ExitWhen) {
                    self.advance(); // consume 'exit_when'
                    self.consume(&TokenType::Colon, "Expected ':'");
                    exit_when = Some(self.expression());
                }
                self.consume(&TokenType::End, "Expected 'end'");
                OrchestrateKind::Loop {
                    agent,
                    max_rounds,
                    exit_when,
                }
            }
            _ => {
                panic!("Expected 'sequential', 'graph', or 'loop', got '{}'", mode);
            }
        };

        let stmt_kind = StmtKind::Orchestrate {
            input_var,
            result_var,
            kind,
        };
        self.arena.alloc_stmt(stmt_kind, span)
    }

    /// 解析 agent 声明列表
    pub(super) fn parse_agent_decls(&mut self) -> Vec<OrchestrateAgent> {
        let mut agents = Vec::new();
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        while !self.check(&TokenType::End) && !self.check(&TokenType::Edges) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.check(&TokenType::Edges) || self.is_at_end() {
                break;
            }
            if self.match_identifier("agent") {
                let name = self.consume_identifier("Expected agent name");
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                // 解析可选的 with 配置
                let mut with_config = None;
                if self.check(&TokenType::WithKeyword) {
                    with_config = Some(self.parse_with_bindings());
                }
                // 解析 task(...)
                self.consume_identifier("Expected 'task'");
                self.consume(&TokenType::LParen, "Expected '('");
                let task_expr = self.expression();
                self.consume(&TokenType::RParen, "Expected ')'");
                // 解析可选的 verify(...)
                let mut verify_expr = None;
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                if self.match_identifier("verify") {
                    self.consume(&TokenType::LParen, "Expected '('");
                    verify_expr = Some(self.expression());
                    self.consume(&TokenType::RParen, "Expected ')'");
                }
                self.consume(&TokenType::End, "Expected 'end'");
                agents.push(OrchestrateAgent {
                    name,
                    with_config,
                    task_expr,
                    verify_expr,
                });
            } else {
                break;
            }
            while self.check(&TokenType::Newline) {
                self.advance();
            }
        }
        agents
    }

    /// 解析 with 绑定（复用现有 with 块逻辑）
    pub(super) fn parse_with_bindings(&mut self) -> Vec<(String, NodeId)> {
        self.advance(); // consume 'with'
        let mut bindings = Vec::new();
        let key = self.consume_identifier("Expected config key");
        self.consume(&TokenType::LParen, "Expected '('");
        let val = self.expression();
        self.consume(&TokenType::RParen, "Expected ')'");
        bindings.push((key, val));
        while self.match_token(&[TokenType::Comma]) {
            let key = self.consume_identifier("Expected config key");
            self.consume(&TokenType::LParen, "Expected '('");
            let val = self.expression();
            self.consume(&TokenType::RParen, "Expected ')'");
            bindings.push((key, val));
        }
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        bindings
    }

    /// 解析边声明
    pub(super) fn parse_edge_decl(&mut self) -> OrchestrateEdge {
        let from = if self.match_identifier("@start") {
            "@start".to_string()
        } else {
            self.consume_identifier("Expected agent name or @start")
        };
        self.consume(&TokenType::Arrow, "Expected '->'");
        let to = if self.match_identifier("@exit") {
            "@exit".to_string()
        } else {
            self.consume_identifier("Expected agent name or @exit")
        };
        // 解析可选的 when 条件
        let mut condition = None;
        if self.match_identifier("when") {
            condition = Some(self.expression());
        }
        // 消费可选的分号或换行
        if self.check(&TokenType::Newline) {
            self.advance();
        }
        OrchestrateEdge {
            from,
            to,
            condition,
        }
    }

    /// v0.25: eval 块 — Agent 行为回归测试
    pub(super) fn eval_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'eval'

        // 解析评测名称字符串
        let name = if let Some(Token {
            token_type: TokenType::String(s),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            s
        } else {
            self.consume_identifier("Expected eval name")
        };

        while self.check(&TokenType::Newline) {
            self.advance();
        }

        // 解析 given / expect / tolerance / replay
        let mut given = None;
        let mut expects = Vec::new();
        let mut tolerance = None;
        let mut replay_path = None;

        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }

            if self.match_identifier("given") {
                self.consume(&TokenType::Colon, "Expected ':'");
                given = Some(self.expression());
            } else if self.check(&TokenType::Expect) {
                self.advance(); // consume 'expect'
                self.consume(&TokenType::Colon, "Expected ':'");
                expects.push(self.expression());
            } else if self.check(&TokenType::Tolerance) {
                self.advance(); // consume 'tolerance'
                self.consume(&TokenType::Colon, "Expected ':'");
                if let Some(Token {
                    token_type: TokenType::Number(n),
                    ..
                }) = self.peek().cloned()
                {
                    tolerance = Some(n);
                    self.advance();
                }
            } else if self.match_identifier("replay") {
                self.consume(&TokenType::Colon, "Expected ':'");
                if let Some(Token {
                    token_type: TokenType::String(s),
                    ..
                }) = self.peek().cloned()
                {
                    replay_path = Some(s);
                    self.advance();
                }
            } else {
                // 跳过未知关键字
                self.advance();
            }

            while self.check(&TokenType::Newline) {
                self.advance();
            }
        }

        self.consume(&TokenType::End, "Expected 'end'");

        let given = given.expect("eval requires 'given:'");
        let kind = StmtKind::Eval {
            name,
            given,
            expects,
            tolerance,
            replay_path,
        };
        self.arena.alloc_stmt(kind, span)
    }

    /// v0.25: skill 块 — 可复用能力包
    pub(super) fn skill_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'skill'
        let name = self.consume_identifier("Expected skill name");

        while self.check(&TokenType::Newline) {
            self.advance();
        }

        // 解析 description / version / requires / task / verify
        let mut description = None;
        let mut version = None;
        let mut requires = Vec::new();
        let mut tasks = Vec::new();
        let mut verify = None;

        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }

            if self.match_identifier("description") {
                self.consume(&TokenType::Colon, "Expected ':'");
                if let Some(Token {
                    token_type: TokenType::String(s),
                    ..
                }) = self.peek().cloned()
                {
                    description = Some(s);
                    self.advance();
                }
            } else if self.match_identifier("version") {
                self.consume(&TokenType::Colon, "Expected ':'");
                if let Some(Token {
                    token_type: TokenType::String(s),
                    ..
                }) = self.peek().cloned()
                {
                    version = Some(s);
                    self.advance();
                }
            } else if self.match_identifier("requires") {
                self.consume(&TokenType::Colon, "Expected ':'");
                self.consume(&TokenType::LBracket, "Expected '['");
                while !self.check(&TokenType::RBracket) && !self.is_at_end() {
                    let dep = self.consume_identifier("Expected dependency name");
                    requires.push(dep);
                    self.match_token(&[TokenType::Comma]);
                }
                self.consume(&TokenType::RBracket, "Expected ']'");
            } else if self.check(&TokenType::Task) {
                self.advance(); // consume 'task'
                // 解析 task 定义: task <name>(<params>) [: <return_type>]
                let task_name = self.consume_identifier("Expected task name");
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
                let return_type = if self.match_token(&[TokenType::Colon]) {
                    Some(self.consume_identifier("Expected return type"))
                } else {
                    None
                };
                // task body: 解析到下一个 task/verify/end
                let body = self.parse_skill_task_body();
                tasks.push(SkillTask {
                    name: task_name,
                    params,
                    return_type,
                    body,
                });
            } else if self.peek_is_identifier("verify") {
                // verify 放在最后，跳出主循环
                break;
            } else {
                self.advance();
            }
        }

        // 解析 verify（可选）
        if self.match_identifier("verify") {
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
            let body = self.parse_skill_task_body();
            verify = Some(SkillVerify { params, body });
        }

        self.consume(&TokenType::End, "Expected 'end'");

        let kind = StmtKind::SkillDef {
            name,
            description,
            version,
            requires,
            tasks,
            verify,
        };
        self.arena.alloc_stmt(kind, span)
    }

    /// 解析 skill task 的 body（到下一个 task/verify/end 为止）
    pub(super) fn parse_skill_task_body(&mut self) -> Vec<NodeId> {
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }
            // 遇到下一个 task/verify 时停止
            if self.check(&TokenType::Task) || self.peek_is_identifier("verify") {
                break;
            }
            if let Some(stmt_id) = self.declaration() {
                body.push(stmt_id);
            }
        }
        body
    }

    pub(super) fn save_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'save'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let value = self.expression();
        let kind = StmtKind::Save { path, value };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn load_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'load'
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into'");
        let var = self.consume_identifier("Expected variable name");
        let kind = StmtKind::Load { path, var };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn read_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'read'
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into'");
        let var = self.consume_identifier("Expected variable name");
        let kind = StmtKind::ReadFile { path, var };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn write_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'write'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let content = self.expression();
        let kind = StmtKind::WriteFile { path, content };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn append_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'append'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let content = self.expression();
        let kind = StmtKind::AppendFile { path, content };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn read_bytes_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'read_bytes'
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into'");
        let var = self.consume_identifier("Expected variable name");
        let kind = StmtKind::ReadBytesFile { path, var };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn write_bytes_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'write_bytes'
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ','");
        let content = self.expression();
        let kind = StmtKind::WriteBytesFile { path, content };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn stream_statement(&mut self) -> NodeId {
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

    pub(super) fn tool_statement(&mut self) -> NodeId {
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

    pub(super) fn observe_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'observe'
        let config = if self.match_token(&[TokenType::Trace]) {
            ObserveConfig::Trace
        } else if self.match_token(&[TokenType::Metrics]) {
            ObserveConfig::Metrics
        } else if self.match_token(&[TokenType::Otel]) {
            self.consume_identifier("Expected 'endpoint'");
            let endpoint = if let Some(Token {
                token_type: TokenType::String(s),
                ..
            }) = self.peek().cloned()
            {
                self.advance();
                self.arena.alloc_expr(
                    ExprKind::Literal(Literal::String(s, self.span_of_current())),
                    self.span_of_current(),
                )
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

    pub(super) fn span_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'span'
        let name = match self.advance() {
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => s.clone(),
            _ => panic!("Expected span name string"),
        };
        let attributes = if self.match_token(&[TokenType::Tags]) {
            self.consume(&TokenType::LBrace, "Expected '{'");
            let mut attrs = Vec::new();
            loop {
                let key = match self.advance() {
                    Some(Token {
                        token_type: TokenType::Identifier(n),
                        ..
                    }) => n.clone(),
                    Some(Token {
                        token_type: TokenType::String(s),
                        ..
                    }) => s.clone(),
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
        let kind = StmtKind::Span {
            name,
            attributes,
            body,
        };
        self.arena.alloc_stmt(kind, span)
    }

    pub(super) fn record_tokens_statement(&mut self) -> NodeId {
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

    /// v0.26: `prompt "name" do ... end` — 声明一段 system prompt 分段
    /// 块内允许 4 种形态:
    ///   set role: <expr>
    ///   set budget: <expr>
    ///   read <expr>
    ///   tail(<path>, max: <n>)   -- 走标准 Call,callee = "tail"
    pub(super) fn prompt_section_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'prompt'
        // section name 必须是字符串字面量
        let name = match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => {
                self.advance();
                s
            }
            _ => {
                eprintln!(
                    "Parse error: Expected string section name after 'prompt' at line {}",
                    span.line
                );
                String::new()
            }
        };
        self.consume(&TokenType::Do, "Expected 'do' after prompt section name");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            match self.parse_prompt_section_inner() {
                Some(stmt_id) => body.push(stmt_id),
                None => break,
            }
        }
        self.consume(&TokenType::End, "Expected 'end' to close prompt section");
        let kind = StmtKind::PromptSection { name, body };
        self.arena.alloc_stmt(kind, span)
    }

    /// v0.26: 解析 prompt section 块内子语句
    /// 返回 None 表示遇到未知形态,调用方应当终止循环
    fn parse_prompt_section_inner(&mut self) -> Option<NodeId> {
        // 'set <key>: <value>'
        if self.match_identifier("set") {
            let span = self.span_of_current();
            let key = self.consume_identifier("Expected 'role' or 'budget' after 'set'");
            self.consume(&TokenType::Colon, "Expected ':' after key");
            let value = self.expression();
            return Some(self.arena.alloc_stmt(
                StmtKind::PromptSet { key, value },
                span,
            ));
        }
        // 'read <expr>'
        if self.match_token(&[TokenType::Read]) {
            let span = self.span_of_current();
            let path = self.expression();
            return Some(self.arena.alloc_stmt(StmtKind::PromptRead(path), span));
        }
        // 'tail(<path>, max: <n>)' — 标准 Call,callee = "tail"
        if self.peek_is_identifier("tail")
            && let Some(next) = self.tokens.get(self.current + 1)
            && next.token_type == TokenType::LParen
        {
            let span = self.span_of_current();
            return Some(self.expression_statement_for_span(span));
        }
        // 兜底: 也允许普通表达式语句(如外部函数调用)
        if self.check(&TokenType::Identifier("".into()))
            || matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Identifier(_)) | Some(TokenType::String(_))
            )
        {
            let span = self.span_of_current();
            return Some(self.expression_statement_for_span(span));
        }
        eprintln!(
            "Parse error: unsupported statement inside prompt section at line {}",
            self.span_of_current().line
        );
        None
    }

    /// v0.26: 包装 expression_statement 接受显式 span — 复用顶层逻辑
    fn expression_statement_for_span(&mut self, _span: Span) -> NodeId {
        // 顶层 expression_statement 不接受外部 span,所以插入临时 advance 模式:
        // 直接调用 expression 然后 wrap 成 Expr stmt
        let expr_id = self.expression();
        let span = self.arena.get_expr(expr_id).map(|e| e.span).unwrap_or(_span);
        self.arena.alloc_stmt(StmtKind::Expr(expr_id), span)
    }
}
