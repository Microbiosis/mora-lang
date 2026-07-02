use super::*;

impl ParserV2 {
    pub(super) fn expression(&mut self) -> NodeId {
        self.pipe()
    }

    fn pipe(&mut self) -> NodeId {
        let mut left = self.binary();
        while self.match_token(&[TokenType::Pipe]) {
            let right = self.binary();
            let kind = ExprKind::Pipe { left, right };
            left = self.arena.alloc_expr(kind, self.span_of_current());
        }
        left
    }

    fn binary(&mut self) -> NodeId {
        let mut left = self.unary();
        while self.match_binary_op() {
            let op = self.previous_binary_op();
            let right = self.unary();
            let kind = ExprKind::Binary { left, op, right };
            left = self.arena.alloc_expr(kind, self.span_of_current());
        }
        left
    }

    fn unary(&mut self) -> NodeId {
        if self.check(&TokenType::Match) {
            self.match_expression()
        } else if self.check(&TokenType::Minus) {
            // 一元负号: -expr → 0 - expr
            let span = self.span_of_current();
            self.advance();
            let operand = self.unary();
            let zero = self
                .arena
                .alloc_expr(ExprKind::Literal(Literal::Number(0.0, span)), span);
            let kind = ExprKind::Binary {
                left: zero,
                op: BinaryOp::Sub,
                right: operand,
            };
            self.arena.alloc_expr(kind, span)
        } else {
            self.call()
        }
    }

    fn call(&mut self) -> NodeId {
        let mut expr = self.primary();
        loop {
            if self.check(&TokenType::LParen) {
                // 函数调用
                let span = self.span_of_current();
                self.advance();
                // 检查是否是 ai_model 调用
                if let Some(e) = self.arena.get_expr(expr)
                    && let ExprKind::Variable(name) = &e.kind
                    && name == "ai_model"
                {
                    expr = self.parse_ai_model_call(span);
                    continue;
                }
                let mut args = Vec::new();
                if !self.check(&TokenType::RParen) {
                    // 跳过参数前的换行
                    while self.check(&TokenType::Newline) {
                        self.advance();
                    }
                    args.push(self.expression());
                    while self.match_token(&[TokenType::Comma]) {
                        // 跨行调用的支持:v0.26 修复 call() 跨行不跳 newline 的问题
                        while self.check(&TokenType::Newline) {
                            self.advance();
                        }
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
                            let kind = ExprKind::Call { callee, args };
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
                let kind = ExprKind::Index {
                    object: expr,
                    index,
                };
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

    pub(super) fn closure_expression(&mut self, span: Span) -> NodeId {
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

        let kind = ExprKind::Closure {
            params,
            return_type,
            body,
        };
        self.arena.alloc_expr(kind, span)
    }

    fn match_expression(&mut self) -> NodeId {
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
            while self.check(&TokenType::Newline) {
                self.advance();
            }
        }
        self.consume(&TokenType::End, "Expected 'end'");

        let kind = ExprKind::Match { expr, arms };
        self.arena.alloc_expr(kind, span)
    }

    pub(super) fn pattern(&mut self) -> Pattern {
        if let Some(Token {
            token_type: TokenType::Identifier(name),
            ..
        }) = self.peek().cloned()
            && name == "_"
        {
            self.advance();
            return Pattern::Wildcard;
        }

        if self.match_token(&[TokenType::True]) {
            Pattern::Literal(Literal::Bool(true, Span::default()))
        } else if self.match_token(&[TokenType::False]) {
            Pattern::Literal(Literal::Bool(false, Span::default()))
        } else if self.match_token(&[TokenType::Nil]) {
            Pattern::Literal(Literal::Nil(Span::default()))
        } else if let Some(Token {
            token_type: TokenType::Number(n),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            Pattern::Literal(Literal::Number(n, Span::default()))
        } else if let Some(Token {
            token_type: TokenType::String(s),
            ..
        }) = self.peek().cloned()
        {
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
                            rest =
                                Some(self.consume_identifier("Expected variable name after '...'"));
                            break;
                        }
                        items.push(self.pattern());
                    }
                }
            }
            self.consume(&TokenType::RBracket, "Expected ']'");
            Pattern::List {
                prefix: items,
                rest,
            }
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
        } else if let Some(Token {
            token_type: TokenType::Identifier(name),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            Pattern::Variable(name)
        } else {
            Pattern::Wildcard
        }
    }

    fn dict_pattern_entry(&mut self) -> (String, Pattern) {
        let key = match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                name
            }
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => {
                self.advance();
                s
            }
            _ => {
                eprintln!("Parse error: Expected pattern key");
                String::new()
            }
        };
        self.consume(&TokenType::Colon, "Expected ':'");
        let pattern = self.pattern();
        (key, pattern)
    }

    fn primary(&mut self) -> NodeId {
        let span = self.span_of_current();

        if self.match_token(&[TokenType::True]) {
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Bool(true, span)), span)
        } else if self.match_token(&[TokenType::False]) {
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Bool(false, span)), span)
        } else if self.match_token(&[TokenType::Nil]) {
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Nil(span)), span)
        } else if let Some(Token {
            token_type: TokenType::Number(n),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Number(n, span)), span)
        } else if let Some(Token {
            token_type: TokenType::Char(ch),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Char(ch, span)), span)
        } else if let Some(Token {
            token_type: TokenType::String(s),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            if has_format_interpolation(&s) {
                self.parse_format_string(&s, span)
            } else {
                self.arena
                    .alloc_expr(ExprKind::Literal(Literal::String(s, span)), span)
            }
        } else if let Some(Token {
            token_type: TokenType::Identifier(name),
            ..
        }) = self.peek().cloned()
        {
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
                let kind = ExprKind::NamespaceRef {
                    namespace: ns_or_name,
                    name: method,
                };
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
        } else if let Some(Token {
            token_type: TokenType::PromptString(s),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            let inner = if has_format_interpolation(&s) {
                self.parse_format_string(&s, span)
            } else {
                self.arena
                    .alloc_expr(ExprKind::Literal(Literal::String(s, span)), span)
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
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Nil(span)), span)
        }
    }

    fn list_literal(&mut self, span: Span) -> NodeId {
        self.advance(); // consume '['
        let mut items = Vec::new();
        if !self.check(&TokenType::RBracket) {
            items.push(self.expression());
            while self.match_token(&[TokenType::Comma]) {
                if self.check(&TokenType::RBracket) {
                    break;
                }
                items.push(self.expression());
            }
        }
        self.consume(&TokenType::RBracket, "Expected ']'");
        self.arena.alloc_expr(ExprKind::List(items), span)
    }

    fn dict_literal(&mut self, span: Span) -> NodeId {
        self.advance(); // consume '{'
        let mut entries = Vec::new();
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        if !self.check(&TokenType::RBrace) {
            let (key, val) = self.dict_entry();
            entries.push((key, val));
            while self.match_token(&[TokenType::Comma]) {
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                if self.check(&TokenType::RBrace) {
                    break;
                }
                let (key, val) = self.dict_entry();
                entries.push((key, val));
            }
        }
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        self.consume(&TokenType::RBrace, "Expected '}'");
        self.arena.alloc_expr(ExprKind::Dict(entries), span)
    }

    fn dict_entry(&mut self) -> (String, NodeId) {
        let key = match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                name
            }
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => {
                self.advance();
                s
            }
            _ => {
                eprintln!("Parse error: Expected dict key");
                String::new()
            }
        };
        self.consume(&TokenType::Colon, "Expected ':'");
        let val = self.expression();
        (key, val)
    }
}
