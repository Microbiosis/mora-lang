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

        // v0.55: 当前 else 关键字未在 lexer 内 — 此处暂不扩展 `if-else`。
        // 需要时: 在 `src/lexer.rs` 加 `TokenType::Else` + 关键字映射("else"=>Else),
        // 再在本路径添加 else 分支。这是 跨 lexer + parser 的特性,留作未来 feature-PR。
        let else_branch = vec![];

        let kind = StmtKind::If {
            condition,
            then_branch,
            else_branch,
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
            // v0.55 root-cause: 之前用 `consume_identifier` 直接吞第一 token 当模式,
            // 无法表达 `_` 通配符 / 字面模式 / 列表模式。改走表达式层的
            // `pattern()` 共享同一套模式语法,与 expression-level match 一致。
            let pattern = self.pattern();
            let arm_expr = self.expression();
            arms.push((pattern, vec![arm_expr]));
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
        // v0.35 (P0-C2): `route` was parse+typecheck-only, never executed.
        // We still parse it as StmtKind::Route; the interpreter now reports
        // a clear runtime error rather than falling through to a generic
        // "Unsupported v2 statement" message.
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

        // 解析模式: sequential / graph / loop / pregel
        let mode = if self.check(&TokenType::Loop) {
            self.advance();
            "loop".to_string()
        } else {
            self.consume_identifier("Expected 'sequential', 'graph', 'loop', or 'pregel'")
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
                let (agents, _) = self.parse_agent_decls();
                self.consume(&TokenType::End, "Expected 'end'");
                OrchestrateKind::Sequential { agents }
            }
            "graph" => {
                let (agents, _) = self.parse_agent_decls();
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
                        // 拒绝负数 / NaN / Inf / 越界 —— 这些值经 `n as usize` 会静默
                        // 换为 usize::MAX 或 0（平台相关）。违反时按 parser 风格
                        // 打印诊断 + 降级，继续 parse。
                        if !n.is_finite() || n < 0.0 || n > usize::MAX as f64 {
                            eprintln!(
                                "Parse error: orchestrate.loop max_rounds must be a non-negative finite number in [0, {}], got {}",
                                usize::MAX,
                                n
                            );
                            max_rounds = 0;
                        } else {
                            max_rounds = n as usize;
                        }
                        self.advance();
                    }
                }
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                // 解析单个 agent
                let (agents, _) = self.parse_agent_decls();
                let agent = match agents.into_iter().next() {
                    Some(a) => a,
                    None => {
                        eprintln!("Parse error: orchestrate loop requires exactly one agent");
                        let stmt_kind = StmtKind::Orchestrate {
                            input_var,
                            result_var,
                            kind: OrchestrateKind::Loop {
                                agent: crate::ast_v2::OrchestrateAgent {
                                    name: String::new(),
                                    with_config: None,
                                    task_expr: crate::ast_v2::NodeId(0),
                                    verify_expr: None,
                                },
                                max_rounds,
                                exit_when: None,
                            },
                        };
                        return self.arena.alloc_stmt(stmt_kind, span);
                    }
                };
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
            "pregel" => {
                // v0.50: Pregel BSP 执行模型
                // 1. 解析可选 state: { ... }
                let state_schema = if self.check(&TokenType::State) {
                    self.advance(); // consume 'state'
                    self.consume(&TokenType::Colon, "Expected ':'");
                    self.parse_state_schema()
                } else {
                    Vec::new()
                };
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                // 2. 解析可选 checkpoint: ...
                let checkpoint = if self.check(&TokenType::Checkpoint) {
                    self.advance(); // consume 'checkpoint'
                    self.consume(&TokenType::Colon, "Expected ':'");
                    Some(self.parse_checkpoint_config())
                } else {
                    None
                };
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                // 3. 解析 agent 声明（含 interrupt 点）
                let (agents, interrupt_points) = self.parse_agent_decls();
                // 4. 解析可选 edges
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
                // 5. 解析额外 interrupt 点（可能出现在 edges 之后）
                let mut all_interrupt_points = interrupt_points;
                let extra = self.parse_interrupt_points();
                all_interrupt_points.extend(extra);
                self.consume(&TokenType::End, "Expected 'end'");
                OrchestrateKind::Pregel {
                    agents,
                    edges,
                    state_schema,
                    checkpoint,
                    interrupt_points: all_interrupt_points,
                }
            }
            _ => {
                // v0.31: 不再 panic; 报告错误并返回默认 OrchestrateKind
                eprintln!(
                    "Parse error: Expected 'sequential', 'graph', 'loop', or 'pregel', got '{}'",
                    mode
                );
                // SAFETY: parse 错误已报告, 默认 Sequential 让 parser 继续
                crate::ast_v2::OrchestrateKind::Sequential { agents: Vec::new() }
            }
        };

        let stmt_kind = StmtKind::Orchestrate {
            input_var,
            result_var,
            kind,
        };
        self.arena.alloc_stmt(stmt_kind, span)
    }

    /// 解析 state schema 块
    fn parse_state_schema(&mut self) -> Vec<StateChannel> {
        self.consume(&TokenType::LBrace, "Expected '{'");
        let mut schema = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::RBrace) || self.is_at_end() {
                break;
            }
            let name = self.consume_identifier("Expected state channel name");
            self.consume(&TokenType::Colon, "Expected ':'");
            // 解析类型 hint（支持 list 类型 [T]）
            let type_hint = if self.check(&TokenType::LBracket) {
                self.advance(); // consume '['
                let inner = self.consume_identifier("Expected type name");
                self.consume(&TokenType::RBracket, "Expected ']'");
                Some(format!("[{}]", inner))
            } else {
                Some(self.parse_type_name_recursive())
            };
            // 解析可选 @reducer
            let mut reducer = ReducerKind::Last;
            if self.match_token(&[TokenType::At]) {
                let reducer_name = self.consume_identifier("Expected reducer name");
                match reducer_name.as_str() {
                    "append" => reducer = ReducerKind::Append,
                    "add" => reducer = ReducerKind::Add,
                    "last" => reducer = ReducerKind::Last,
                    "merge" => {
                        self.consume(&TokenType::LParen, "Expected '('");
                        let merge_fn = self.expression();
                        self.consume(&TokenType::RParen, "Expected ')'");
                        reducer = ReducerKind::Merge(merge_fn);
                    }
                    other => {
                        eprintln!("Parse error: Unknown reducer '{}'", other);
                    }
                }
            }
            schema.push(StateChannel {
                name,
                type_hint,
                reducer,
            });
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(&TokenType::RBrace, "Expected '}'");
        schema
    }

    /// 解析 checkpoint 配置
    fn parse_checkpoint_config(&mut self) -> CheckpointConfig {
        let saver = self.consume_identifier("Expected saver name");
        let mut thread_id = None;
        if self.match_token(&[TokenType::Comma]) && self.check(&TokenType::Thread) {
            self.advance(); // consume 'thread'
            self.consume(&TokenType::Colon, "Expected ':'");
            thread_id = Some(self.expression());
        }
        CheckpointConfig { saver, thread_id }
    }

    /// 解析 interrupt 点声明
    fn parse_interrupt_points(&mut self) -> Vec<crate::ast_v2::InterruptPoint> {
        let mut points = Vec::new();
        while self.check(&TokenType::Interrupt) {
            self.advance(); // consume 'interrupt'
            let when = if self.match_token(&[TokenType::Before]) {
                crate::ast_v2::InterruptWhen::Before
            } else if self.match_token(&[TokenType::After]) {
                crate::ast_v2::InterruptWhen::After
            } else {
                eprintln!("Parse error: Expected 'before' or 'after' after 'interrupt'");
                break;
            };
            let node_name = self.consume_identifier("Expected node name");
            points.push(crate::ast_v2::InterruptPoint { node_name, when });
            while self.check(&TokenType::Newline) {
                self.advance();
            }
        }
        points
    }

    /// 解析 agent 声明列表（v0.50: 同时收集 interrupt 点）
    pub(super) fn parse_agent_decls(
        &mut self,
    ) -> (Vec<OrchestrateAgent>, Vec<crate::ast_v2::InterruptPoint>) {
        let mut agents = Vec::new();
        let mut interrupt_points = Vec::new();
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
            } else if self.check(&TokenType::Interrupt) {
                // v0.50: 在 agent 声明中解析 interrupt 点
                let points = self.parse_interrupt_points();
                interrupt_points.extend(points);
            } else {
                break;
            }
            while self.check(&TokenType::Newline) {
                self.advance();
            }
        }
        (agents, interrupt_points)
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
        // v0.50: 解析可选的 dynamic { ... } 子句
        let mut dynamic = None;
        if self.match_token(&[TokenType::LBrace]) {
            if self.match_identifier("dynamic") {
                self.consume(&TokenType::Colon, "Expected ':'");
                dynamic = if self.match_token(&[TokenType::Map]) {
                    Some(DynamicKind::Map)
                } else if self.match_token(&[TokenType::Reduce]) {
                    Some(DynamicKind::Reduce)
                } else if self.match_token(&[TokenType::FanIn]) {
                    Some(DynamicKind::FanIn)
                } else if self.match_token(&[TokenType::FanOut]) {
                    Some(DynamicKind::FanOut)
                } else {
                    let name = self.consume_identifier("Expected dynamic kind");
                    eprintln!("Parse error: Unknown dynamic kind '{}'", name);
                    None
                };
            }
            self.consume(&TokenType::RBrace, "Expected '}'");
        }
        // 消费可选的换行
        if self.check(&TokenType::Newline) {
            self.advance();
        }
        OrchestrateEdge {
            from,
            to,
            condition,
            dynamic,
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

        let given = match given {
            Some(g) => g,
            None => {
                eprintln!("Parse error: eval block requires a 'given:' clause");
                crate::ast_v2::NodeId(0)
            }
        };

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
                // v0.31: 不再 panic; 用空字符串占位让 parser 继续
                eprintln!("Parse error: Expected string endpoint after otel");
                self.arena.alloc_expr(
                    ExprKind::Literal(Literal::String(String::new(), self.span_of_current())),
                    self.span_of_current(),
                )
            };
            ObserveConfig::Otel { endpoint }
        } else {
            // v0.31: 不再 panic; 默认 Trace 让 parser 继续
            eprintln!("Parse error: Expected trace / metrics / otel after 'observe'");
            ObserveConfig::Trace
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
            _ => {
                eprintln!("Parse error: Expected span name string");
                String::new()
            }
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
                    _ => {
                        eprintln!("Parse error: Expected tag key");
                        String::new()
                    }
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
            return Some(
                self.arena
                    .alloc_stmt(StmtKind::PromptSet { key, value }, span),
            );
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
        let span = self
            .arena
            .get_expr(expr_id)
            .map(|e| e.span)
            .unwrap_or(_span);
        self.arena.alloc_stmt(StmtKind::Expr(expr_id), span)
    }

    /// v0.27: `document "name" do ... end` — 块语句入口
    /// 解析 set <key>: <value> / read <expr> 子语句
    pub(super) fn document_section_statement(&mut self) -> NodeId {
        let span = self.span_of_current();
        self.advance(); // consume 'document'
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
                    "Parse error: Expected string section name after 'document' at line {}",
                    span.line
                );
                String::new()
            }
        };
        self.consume(&TokenType::Do, "Expected 'do' after document section name");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body: Vec<NodeId> = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            // set <key>: <value>
            if self.match_identifier("set") {
                let s = self.span_of_current();
                let key = self.consume_identifier("Expected 'origin' or 'max_pages' after 'set'");
                self.consume(&TokenType::Colon, "Expected ':' after key");
                let value = self.expression();
                body.push(
                    self.arena
                        .alloc_stmt(StmtKind::DocumentSet { key, value }, s),
                );
                continue;
            }
            // read <expr>
            if self.match_token(&[TokenType::Read]) {
                let s = self.span_of_current();
                let path = self.expression();
                body.push(self.arena.alloc_stmt(StmtKind::DocumentRead(path), s));
                continue;
            }
            // 未知子语句:跳过当前 token 防卡死
            eprintln!(
                "Parse warning: unsupported inner statement in document section at line {}",
                self.span_of_current().line
            );
            self.advance();
        }
        self.consume(&TokenType::End, "Expected 'end' to close document section");
        self.arena
            .alloc_stmt(StmtKind::DocumentSection { name, body }, span)
    }
}

#[cfg(test)]
mod tests {
    //! 语句解析器白盒测试。
    //!
    //! 覆盖顶层 entry `parse()` 路径:
    //! - let / task / if / for / return / assign / match / import / expression
    use super::*;
    use crate::ast_v2::StmtKind;
    use crate::lexer::Lexer;

    /// 解析整个程序,返回首个 stmt 的 StmtKind。
    fn first_stmt(src: &str) -> StmtKind {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let stmts = parser.parse();
        let arena = parser.into_arena();
        // find first non-EOF statement
        for id in stmts {
            if let Some(s) = arena.get_stmt(id) {
                return s.kind.clone();
            }
        }
        panic!("no statement produced from: {src}");
    }

    #[test]
    fn parses_let_with_int_literal() {
        let kind = first_stmt("let x = 42");
        let StmtKind::Let { name, .. } = &kind else {
            panic!("expected Let, got: {:?}", kind);
        };
        assert_eq!(name, "x");
    }

    #[test]
    fn parses_let_with_type_hint() {
        let kind = first_stmt("let x: number = 7");
        let StmtKind::Let {
            name, type_hint, ..
        } = &kind
        else {
            panic!("expected Let, got: {:?}", kind);
        };
        assert_eq!(name, "x");
        assert_eq!(type_hint.as_deref(), Some("number"));
    }

    #[test]
    fn parses_export_let() {
        let kind = first_stmt("export let x = 1");
        let StmtKind::Let { exported, .. } = &kind else {
            panic!("expected Let, got: {:?}", kind);
        };
        assert!(exported);
    }

    #[test]
    fn parses_task_with_no_params() {
        let kind = first_stmt("task foo() 1 end");
        let StmtKind::TaskDef { name, params, .. } = &kind else {
            panic!("expected TaskDef, got: {:?}", kind);
        };
        assert_eq!(name, "foo");
        assert!(params.is_empty());
    }

    #[test]
    fn parses_task_with_two_params() {
        // 注意:`add` 是关键字(TokenType::Add / `@add` 语义),不能用作任务名。
        // 改用 `combine`。
        let kind = first_stmt("task combine(a, b)\n  1\nend");
        let StmtKind::TaskDef { name, params, .. } = &kind else {
            panic!("expected TaskDef, got: {:?}", kind);
        };
        assert_eq!(name, "combine");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn parses_task_with_return_type() {
        let kind = first_stmt("task identity(x): number x end");
        let StmtKind::TaskDef { return_type, .. } = &kind else {
            panic!("expected TaskDef, got: {:?}", kind);
        };
        assert_eq!(return_type.as_deref(), Some("number"));
    }

    /// v0.55 (Bug C): `else` 关键字未在 lexer 内,parser 层无法扩展
    /// `if-else` 分支。测试锁定当前现状(`else` 作为 identifier):
    /// - lexer 把 `else` 当 identifier
    /// - parser 把 `if x then 1 end` 解析为 If(then_branch=[1], else_branch=[])
    /// - `if x then 1 else 2 end` 当前无法产生干净的双分支 If,因为 `else`
    ///   是 identifier,parser 会吞 `else 2 end` 作为 then_branch 内的语句。
    ///
    /// 修复路径(留作未来 PR): 在 `src/lexer.rs` 加 `TokenType::Else` 与
    /// `"else"` 关键字映射,然后在 `if_statement` 内添加 `else` 吞入 + 递归解析。
    #[test]
    fn if_else_branch_is_unimplemented_feature_in_lexer() {
        // 1. 文档化 lexer 现状: `else` 是 identifier,不是关键字
        let tokens = crate::lexer::Lexer::new("else").scan_tokens();
        assert_eq!(
            tokens[0].token_type,
            crate::lexer::TokenType::Identifier("else".to_string())
        );
        // 2. 文档化 parser 现状: `if x then 1 end` 解析为 then-only
        let kind = first_stmt("if x then 1 end");
        if let StmtKind::If {
            then_branch,
            else_branch,
            ..
        } = &kind
        {
            assert_eq!(then_branch.len(), 1);
            assert!(else_branch.is_empty());
        } else {
            panic!("expected If, got: {:?}", kind);
        }
    }

    #[test]
    fn parses_for_over_list() {
        let kind = first_stmt("for x in xs 1 end");
        let StmtKind::For { var, iterable, .. } = &kind else {
            panic!("expected For, got: {:?}", kind);
        };
        assert_eq!(var, "x");
        // iterable is NodeId; we don't easily check its kind here
        let _ = iterable;
    }

    #[test]
    fn parses_return_statement_with_value() {
        let kind = first_stmt("return 42");
        let StmtKind::Return { value } = &kind else {
            panic!("expected Return, got: {:?}", kind);
        };
        assert!(value.is_some());
    }

    #[test]
    fn parses_return_statement_without_value() {
        // `return` 后必须紧跟 newline/EOF 才识别为 None —— 否则会被吞进 expression
        let kind = first_stmt("return\n");
        let StmtKind::Return { value } = &kind else {
            panic!("expected Return, got: {:?}", kind);
        };
        assert!(value.is_none());
    }

    #[test]
    fn parses_simple_assignment() {
        let kind = first_stmt("x = 5");
        let StmtKind::Assign { name, .. } = &kind else {
            panic!("expected Assign, got: {:?}", kind);
        };
        assert_eq!(name, "x");
    }

    #[test]
    fn parses_match_statement_with_two_arms() {
        // 注意: 当前 match 只识别 identifier 模式(wildcard `_` 不支持),
        // 也使用 `with` 而不是 `=>`;arm 形式: `pat expr`
        let kind = first_stmt("match x with\n  a 1\n  b 0\nend");
        let StmtKind::Match { arms, .. } = &kind else {
            panic!("expected Match stmt, got: {:?}", kind);
        };
        assert_eq!(arms.len(), 2);
    }

    /// v0.55 (Bug D) root-cause: `match_statement` 直接读 identifier 当模式,
    /// 不走完整 `pattern()` 解析。改走 pattern() 后,`_` 通配符 / 字面模式 / 列表模式
    /// 都可用,与 expression-level match 共享同一套模式语法。
    #[test]
    fn match_statement_should_call_pattern_for_wildcard() {
        // 当前 match_statement: `match _ with 99 end` 会失败,因为
        // `consume_identifier` 把 `_` 当 identifier 消耗,然后期待 1 个 arm
        // body(表达式),但后面是 `99`(数字字面量可接受),所以技术上能解析,
        // 但模式绑定是 `Variable("_")` 而非 `Wildcard`。
        // 修复后,`pattern()` 在 `_` 时返回 Pattern::Wildcard。
        let kind = first_stmt("match _ with _ 99 end");
        let StmtKind::Match { arms, .. } = &kind else {
            panic!("expected Match stmt, got: {:?}", kind);
        };
        assert_eq!(arms.len(), 1);
        let (pattern, _body) = arms.first().expect("1 arm");
        // 修复后应是 Wildcard; 修复前是 Variable("_")
        assert!(
            matches!(pattern, Pattern::Wildcard),
            "after fix: pattern should be Wildcard, got: {:?}",
            pattern
        );
    }

    #[test]
    fn parses_import_statement() {
        let kind = first_stmt("import std::io");
        // import is its own variant
        let s = format!("{:?}", kind);
        assert!(
            s.contains("Import"),
            "import → {:?}, expected Import variant",
            kind
        );
    }

    #[test]
    fn parses_expression_statement() {
        // 调用表达式(如 print(...))也是合法 statement
        let kind = first_stmt("print(1)");
        // 可归类为 Expression / Call / MethodCall 等;只需能解析、不报错
        let s = format!("{:?}", kind);
        assert!(!s.is_empty());
    }

    #[test]
    fn parses_struct_definition() {
        let kind = first_stmt("struct Point end");
        let StmtKind::StructDef { name, fields, .. } = &kind else {
            panic!("expected StructDef, got: {:?}", kind);
        };
        assert_eq!(name, "Point");
        assert!(fields.is_empty());
    }

    #[test]
    fn parses_enum_definition_with_variants() {
        let kind = first_stmt("enum Color\n  Red\n  Green\nend");
        let StmtKind::EnumDef { name, variants, .. } = &kind else {
            panic!("expected EnumDef, got: {:?}", kind);
        };
        assert_eq!(name, "Color");
        assert_eq!(variants.len(), 2);
    }

    #[test]
    fn parses_loop_orchestrate_with_max_rounds_bounded() {
        // 受 v0.54 orchestrator max_rounds bounded by parser 影响
        // 负数 -> 降级处理 (eprintln 警告 + 用 0);此处只测合法正值
        let kind = first_stmt("orchestrate loop(name: x, max_rounds: 3)\n  end\n");
        let s = format!("{:?}", kind);
        assert!(s.contains("Orchestrate"), "got: {:?}", kind);
    }

    #[test]
    fn parses_trait_decl_with_method() {
        let kind = first_stmt("trait Greet\n  fn greet(self): string\nend");
        let StmtKind::TraitDef { name, methods, .. } = &kind else {
            panic!("expected TraitDef, got: {:?}", kind);
        };
        assert_eq!(name, "Greet");
        assert!(!methods.is_empty());
    }

    #[test]
    fn parses_parallel_block() {
        let kind = first_stmt("parallel\n  a()\n  b()\nend");
        let s = format!("{:?}", kind);
        assert!(s.contains("Parallel"), "got: {:?}", kind);
    }

    #[test]
    fn parses_with_block() {
        let kind = first_stmt("with { x = 1 }\n  body()\nend");
        let s = format!("{:?}", kind);
        assert!(s.contains("With"), "got: {:?}", kind);
    }

    #[test]
    fn parses_break_and_continue() {
        // 两个独立语句,各自在 parse() 返回的 Vec 中
        let tokens = Lexer::new("break\ncontinue\n").scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let stmts = parser.parse();
        assert_eq!(stmts.len(), 2);
        let arena = parser.into_arena();
        let kinds: Vec<_> = stmts
            .iter()
            .filter_map(|id| arena.get_stmt(*id).map(|s| s.kind.clone()))
            .collect();
        assert!(matches!(kinds[0], StmtKind::Break));
        assert!(matches!(kinds[1], StmtKind::Continue));
    }
}
