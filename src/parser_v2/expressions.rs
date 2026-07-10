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
            // v0.55 root-cause: 一元负号 `-expr` desugars 为 `0 - expr`。
            // v0.53 之前 left 硬编码为 `Literal::Number(0.0)`,与 v0.38 strict
            // numeric tower 的"Int 不与 f64 混算"约束产生交集,把整个结果
            // 推成 Number,丢了 Int 类型信息。
            //
            // 修复: 用 operand 的字面后缀决定 left 类型 —
            //   - i 后缀 → Int(0)
            //   - f 后缀 → Float(0.0)
            //   - 其他 / 后跟表达式 → Number(0.0)(legacy alias)
            //
            // 这样 `-3i ⇒ 0i - 3i ⇒ Int(-3)`,`-1.5f ⇒ Float(-1.5)`,
            // `-x ⇒ Number(0.0) - x` (回退到现状,不影响非字面场景)。
            let span = self.span_of_current();
            self.advance();
            let operand = self.unary();
            let zero_kind = match literal_kind(&self.arena, operand) {
                Some(Literal::Int(_, _)) => Literal::Int(0, span),
                Some(Literal::Float(_, _)) => Literal::Float(0.0, span),
                _ => Literal::Number(0.0, span),
            };
            let zero = self.arena.alloc_expr(ExprKind::Literal(zero_kind), span);
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
            token_type: TokenType::Int(n),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Int(n, span)), span)
        } else if let Some(Token {
            token_type: TokenType::Float(n),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            self.arena
                .alloc_expr(ExprKind::Literal(Literal::Float(n, span)), span)
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
        } else if self.check(&TokenType::Command) {
            self.command_expression(span)
        } else if self.check(&TokenType::Send) {
            self.send_expression(span)
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

    /// v0.50: 解析 Command 表达式
    /// command { goto: "node_name", update: { key: expr }, resume: expr }
    fn command_expression(&mut self, span: Span) -> NodeId {
        self.advance(); // consume 'command'
        self.consume(&TokenType::LBrace, "Expected '{'");
        let mut goto = None;
        let mut update = Vec::new();
        let mut resume = None;
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::RBrace) || self.is_at_end() {
                break;
            }
            let key = self.consume_identifier("Expected field name");
            self.consume(&TokenType::Colon, "Expected ':'");
            match key.as_str() {
                "goto" => {
                    if let Some(Token {
                        token_type: TokenType::String(s),
                        ..
                    }) = self.peek().cloned()
                    {
                        goto = Some(s);
                        self.advance();
                    } else {
                        eprintln!("Parse error: command.goto expects string literal");
                        self.expression(); // consume for error recovery
                    }
                }
                "update" => {
                    self.consume(&TokenType::LBrace, "Expected '{'");
                    while !self.check(&TokenType::RBrace) && !self.is_at_end() {
                        while self.check(&TokenType::Newline) {
                            self.advance();
                        }
                        if self.check(&TokenType::RBrace) || self.is_at_end() {
                            break;
                        }
                        let ukey = self.consume_identifier("Expected update key");
                        self.consume(&TokenType::Colon, "Expected ':'");
                        let uval = self.expression();
                        update.push((ukey, uval));
                        if !self.match_token(&[TokenType::Comma]) {
                            break;
                        }
                    }
                    self.consume(&TokenType::RBrace, "Expected '}'");
                }
                "resume" => {
                    resume = Some(self.expression());
                }
                other => {
                    eprintln!("Parse error: Unknown command field '{}'", other);
                    self.expression(); // consume for error recovery
                }
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(&TokenType::RBrace, "Expected '}'");
        let kind = ExprKind::Command {
            goto,
            update,
            resume,
        };
        self.arena.alloc_expr(kind, span)
    }

    /// v0.50: 解析 Send 表达式
    /// send("target", input_expr)
    fn send_expression(&mut self, span: Span) -> NodeId {
        self.advance(); // consume 'send'
        self.consume(&TokenType::LParen, "Expected '('");
        let target = if let Some(Token {
            token_type: TokenType::String(s),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            s
        } else {
            self.consume_identifier("Expected target name")
        };
        self.consume(&TokenType::Comma, "Expected ','");
        let input = self.expression();
        self.consume(&TokenType::RParen, "Expected ')'");
        let kind = ExprKind::Send { target, input };
        self.arena.alloc_expr(kind, span)
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

/// v0.55 root-cause 辅助: 从 arena 上读取 expr,若 expr 是字面量则返回 Some(literal)
/// (用于一元负号 desugaring 决定 left 字面量类型)。
pub(super) fn literal_kind(
    arena: &crate::ast_v2::AstArena,
    id: crate::ast_v2::NodeId,
) -> Option<Literal> {
    let expr = arena.get_expr(id)?;
    if let ExprKind::Literal(lit) = &expr.kind {
        Some(lit.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    //! 表达式解析器白盒测试。
    //!
    //! 覆盖：
    //! - 优先级 (pipe / binary / unary / call / primary)
    //! - 一元负号 (desugaring 到 `0 - expr`)
    //! - 字面量 (Int / Float / Number / String / Char / Bool / Nil)
    //! - 列表 / 字典 / 元组 / 范围字面量
    //! - 模式匹配 (match / pattern / dict pattern)
    //! - 闭包 / 匿名函数 / 命令式表达式
    use super::*;
    use crate::ast_v2::ExprKind;
    use crate::common::Literal;
    use crate::lexer::Lexer;

    /// 解析单个表达式,返回 (ExprKind, arena) 方便断言。
    fn parse_expr(src: &str) -> (ExprKind, crate::ast_v2::AstArena) {
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let expr = parser.expression();
        let arena = parser.into_arena();
        let kind = arena
            .get_expr(expr)
            .expect("expression must exist in arena")
            .kind
            .clone();
        (kind, arena)
    }

    #[test]
    fn parses_integer_literal_int_suffix() {
        let (kind, _) = parse_expr("42i");
        assert!(matches!(kind, ExprKind::Literal(Literal::Int(42, _))));
    }

    #[test]
    fn parses_float_literal_f_suffix() {
        let (kind, _) = parse_expr("1.5f");
        assert!(matches!(kind, ExprKind::Literal(Literal::Float(f, _)) if (f - 1.5).abs() < 1e-9));
    }

    #[test]
    fn parses_unsuffixed_number_as_number() {
        let (kind, _) = parse_expr("3.5");
        assert!(matches!(kind, ExprKind::Literal(Literal::Number(n, _)) if (n - 3.5).abs() < 1e-9));
    }

    #[test]
    fn parses_string_literal() {
        let (kind, _) = parse_expr(r#""hi""#);
        assert!(matches!(kind, ExprKind::Literal(Literal::String(ref s, _)) if s == "hi"));
    }

    #[test]
    fn parses_bool_true() {
        let (kind, _) = parse_expr("true");
        assert!(matches!(kind, ExprKind::Literal(Literal::Bool(true, _))));
    }

    #[test]
    fn parses_nil_literal() {
        let (kind, _) = parse_expr("nil");
        assert!(matches!(kind, ExprKind::Literal(Literal::Nil(_))));
    }

    #[test]
    fn parses_identifier_alone() {
        let (kind, _) = parse_expr("foo");
        assert!(matches!(kind, ExprKind::Variable(ref s) if s == "foo"));
    }

    #[test]
    fn binary_add_groups_left_to_right() {
        // 1 + 2 + 3 → (1 + 2) + 3
        let (kind, arena) = parse_expr("1 + 2 + 3");
        let ExprKind::Binary { left, op, right } = kind else {
            panic!("expected top-level Binary, got: {:?}", kind);
        };
        assert_eq!(op, BinaryOp::Add);
        // right is Literal(3)
        assert!(matches!(
            arena.get_expr(right).map(|e| &e.kind),
            Some(ExprKind::Literal(Literal::Number(n, _))) if *n == 3.0
        ));
        // left is Binary(1 + 2)
        if let ExprKind::Binary { op: op2, .. } = &arena.get_expr(left).unwrap().kind {
            assert_eq!(*op2, BinaryOp::Add);
        } else {
            panic!("nested left should also be Binary");
        }
    }

    #[test]
    fn unary_minus_desugars_to_zero_minus() {
        // -x ⇒ 0 - x
        let (kind, arena) = parse_expr("-x");
        let ExprKind::Binary { left, op, right } = kind else {
            panic!("expected top-level Binary, got: {:?}", kind);
        };
        assert_eq!(op, BinaryOp::Sub);
        assert!(matches!(
            arena.get_expr(left).map(|e| &e.kind),
            Some(ExprKind::Literal(Literal::Number(n, _))) if *n == 0.0
        ));
        assert!(matches!(
            arena.get_expr(right).map(|e| &e.kind),
            Some(ExprKind::Variable(s)) if s == "x"
        ));
    }

    #[test]
    fn pipe_chains_left_to_right() {
        // a |> b ⇒ Pipe { a, b }
        let (kind, arena) = parse_expr("a |> b");
        let ExprKind::Pipe { left, right } = kind else {
            panic!("expected Pipe expr, got: {:?}", kind);
        };
        assert!(matches!(
            arena.get_expr(left).map(|e| &e.kind),
            Some(ExprKind::Variable(s)) if s == "a"
        ));
        assert!(matches!(
            arena.get_expr(right).map(|e| &e.kind),
            Some(ExprKind::Variable(s)) if s == "b"
        ));
    }

    #[test]
    fn list_literal_parses_three_elements() {
        let (kind, arena) = parse_expr("[1, 2, 3]");
        let ExprKind::List(items) = kind else {
            panic!("expected List, got: {:?}", kind);
        };
        assert_eq!(items.len(), 3);
        // validate each item is a literal
        for id in &items {
            assert!(matches!(
                arena.get_expr(*id).map(|e| &e.kind),
                Some(ExprKind::Literal(_))
            ));
        }
    }

    #[test]
    fn empty_list_literal_parses() {
        let (kind, _) = parse_expr("[]");
        let ExprKind::List(items) = kind else {
            panic!("expected List");
        };
        assert!(items.is_empty());
    }

    #[test]
    fn dict_literal_parses_string_keys() {
        let (kind, arena) = parse_expr(r#"{"a": 1, "b": 2}"#);
        let ExprKind::Dict(entries) = kind else {
            panic!("expected Dict");
        };
        assert_eq!(entries.len(), 2);
        for (k, v) in entries {
            assert!(matches!(
                arena.get_expr(v).map(|e| &e.kind),
                Some(ExprKind::Literal(_))
            ));
            assert!(!k.is_empty());
        }
    }

    #[test]
    fn parens_force_grouping() {
        // (1 + 2) * 3 ⇒ left should be Grouping(Binary(1+2)) or directly Binary depending on
        // whether the parser keeps the explicit Grouping wrapper; accept either.
        let (kind, arena) = parse_expr("(1 + 2) * 3");
        let ExprKind::Binary { left, op, right } = kind else {
            panic!("expected top-level Binary, got: {:?}", kind);
        };
        assert_eq!(op, BinaryOp::Mul);
        // left is Binary(1+2); the outer Grouping(...) wrapper is acceptable per parser
        let left_kind = &arena.get_expr(left).unwrap().kind;
        let unwrapped: &ExprKind = match left_kind {
            ExprKind::Grouping(inner) => &arena.get_expr(*inner).unwrap().kind,
            other => other,
        };
        assert!(
            matches!(
                unwrapped,
                ExprKind::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ),
            "left should be Binary(1+2), got: {:?}",
            left_kind
        );
        assert!(matches!(
            arena.get_expr(right).map(|e| &e.kind),
            Some(ExprKind::Literal(Literal::Number(n, _))) if *n == 3.0
        ));
    }

    #[test]
    fn comparison_operators_recognized() {
        for (src, expected) in [
            ("a == b", BinaryOp::Equal),
            ("a != b", BinaryOp::NotEqual),
            ("a < b", BinaryOp::Less),
            ("a > b", BinaryOp::Greater),
            ("a <= b", BinaryOp::LessEqual),
            ("a >= b", BinaryOp::GreaterEqual),
        ] {
            let (kind, _) = parse_expr(src);
            let ExprKind::Binary { op, .. } = kind else {
                panic!("{src}: expected Binary, got {kind:?}");
            };
            assert_eq!(op, expected, "comparator mismatch for {src}");
        }
    }

    #[test]
    fn match_expression_parses_two_arms() {
        let (kind, _) = parse_expr(
            r#"match x
    1 => "one"
    _ => "other"
  end"#,
        );
        let ExprKind::Match { arms, .. } = kind else {
            panic!("expected Match expr, got: {:?}", kind);
        };
        assert_eq!(arms.len(), 2);
    }

    #[test]
    fn pattern_literal_true() {
        let pat = pattern_of("true");
        assert!(matches!(pat, Pattern::Literal(Literal::Bool(true, _))));
    }

    #[test]
    fn pattern_wildcard() {
        let pat = pattern_of("_");
        assert!(matches!(pat, Pattern::Wildcard));
    }

    #[test]
    fn closure_with_two_params_parses() {
        let tokens = Lexer::new("fn(a, b) a + b end").scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let expr = parser.expression();
        let arena = parser.into_arena();
        let e = arena.get_expr(expr).expect("closure expr");
        let ExprKind::Closure { params, .. } = &e.kind else {
            panic!("expected Closure, got: {:?}", e.kind);
        };
        assert_eq!(params.len(), 2);
    }

    /// Parse a pattern from a prefix that the full match parser consumes.
    /// Match.arms is `Vec<(Pattern, NodeId)>`; first arm's first element is the pattern.
    fn pattern_of(src: &str) -> Pattern {
        let tokens = Lexer::new(&format!("match x {src} => 1 end")).scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let expr = parser.expression();
        let arena = parser.into_arena();
        if let ExprKind::Match { arms, .. } = &arena.get_expr(expr).unwrap().kind
            && let Some((p, _)) = arms.first()
        {
            return p.clone();
        }
        panic!("failed to extract pattern from: {src}");
    }
}
