use super::*;

impl ParserV3 {
    /// Parse the full source into a Vec<MirExpr> (legacy/LSP path).
    pub fn parse(mut self) -> Result<Vec<MirExpr>, ParseError> {
        let mut exprs = Vec::new();
        let mut guard = 0usize;

        while !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}

            if self.is_at_end() {
                break;
            }

            guard += 1;
            if guard > 10_000 {
                return Err(ParseError(
                    "parser_v3: aborted after 10k iterations".to_string(),
                ));
            }

            match self.parse_expression_statement() {
                Some(expr) => exprs.push(expr),
                None => {
                    return Err(ParseError(format!(
                        "Failed to parse at line {}",
                        self.current_line()
                    )));
                }
            }
        }

        Ok(exprs)
    }

    fn parse_expression_statement(&mut self) -> Option<MirExpr> {
        let start_span = self.span_of_current();

        if self.match_token_exact(TokenType::Task) {
            let name = self.consume_identifier("Expected task name")?;
            self.consume(TokenType::LParen, "Expected '(' after task name")?;
            let mut params = Vec::new();
            while !self.check(&TokenType::RParen) && !self.is_at_end() {
                if let Some(pname) = self.consume_identifier("Expected parameter name") {
                    params.push(Param {
                        name: pname,
                        type_hint: None,
                        default: None,
                    });
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(TokenType::RParen, "Expected ')' after task params")?;
            let _ = self.match_token(&[TokenType::Newline]);
            let body = if let Some(expr) = self.parse_orchestrate_statement() {
                expr
            } else if let Some(expr) = self.parse_block_body() {
                expr
            } else {
                self.parse_assignment()?
            };
            if self.check(&TokenType::End) {
                self.advance();
            }
            return Some(MirExpr {
                kind: MirExprKind::FnDef {
                    name,
                    params,
                    return_type: None,
                    body: Box::new(body),
                },
                span: start_span,
            });
        }

        if self.check(&TokenType::Let) {
            return self.parse_let_binding();
        }

        if let Some(expr) = self.parse_match_expression() {
            return Some(expr);
        }

        if let Some(expr) = self.parse_if_expression() {
            return Some(expr);
        }

        if let Some(expr) = self.parse_for_loop() {
            return Some(expr);
        }

        if let Some(expr) = self.parse_while_loop() {
            return Some(expr);
        }

        if let Some(expr) = self.parse_return_break_continue() {
            return Some(expr);
        }

        if let Some(expr) = self.parse_type_alias() {
            return Some(expr);
        }
        if let Some(expr) = self.parse_enum_def() {
            return Some(expr);
        }
        if let Some(expr) = self.parse_struct_def() {
            return Some(expr);
        }
        if let Some(expr) = self.parse_import_statement() {
            return Some(expr);
        }
        if let Some(expr) = self.parse_macro_def() {
            return Some(expr);
        }

        if let Some(expr) = self.parse_orchestrate_statement() {
            return Some(expr);
        }

        // v0.88: quasiquote `expr — must come before parse_assignment
        // since backtick is not consumed by any other path.
        if self.check(&TokenType::Backtick) {
            return self.parse_quasiquote();
        }

        let expr = self.parse_assignment()?;
        let _ = self.match_token(&[TokenType::Newline]);
        Some(expr)
    }

    fn parse_match_expression(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Match) {
            return None;
        }

        let expr_span = self.span_of_current();
        let subject = self.parse_expression()?;

        if !self.match_token_exact(TokenType::LBrace) {
            return None;
        }

        let mut arms = Vec::new();
        let mut arm_guard = 0usize;
        loop {
            if self.match_token_exact(TokenType::RBrace) || self.is_at_end() {
                break;
            }

            if let Some(arm) = self.parse_match_arm() {
                arms.push(arm);
                let _ = self.match_token(&[TokenType::Comma]);
            } else {
                self.advance();
            }

            arm_guard += 1;
            if arm_guard > 10_000 {
                break;
            }
        }

        Some(MirExpr {
            kind: MirExprKind::Match {
                scrutinee: Box::new(subject),
                arms,
            },
            span: expr_span,
        })
    }

    fn parse_match_arm(&mut self) -> Option<crate::mir::expr::MatchArm> {
        let pattern = self.parse_pattern()?;

        let guard = if self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "when"))
            .unwrap_or(false)
        {
            self.advance();
            Some(self.parse_assignment()?)
        } else {
            None
        };

        if !self.match_token_exact(TokenType::FatArrow) {
            return None;
        }

        let body = self.parse_assignment()?;

        Some(crate::mir::expr::MatchArm {
            pattern,
            guard,
            body,
        })
    }

    pub(super) fn parse_pattern(&mut self) -> Option<crate::mir::expr::Pattern> {
        if let Some(tok) = self.peek()
            && let TokenType::Identifier(ref name) = tok.token_type
            && name == "_"
        {
            self.advance();
            return Some(crate::mir::expr::Pattern::Wildcard);
        }

        if self.check(&TokenType::DotDot) || self.check(&TokenType::DotDotDot) {
            self.advance();
            let name_opt = if let Some(tok) = self.peek() {
                if let TokenType::Identifier(ref name) = tok.token_type {
                    Some(name.clone())
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(name) = name_opt {
                self.advance();
                return Some(crate::mir::expr::Pattern::Variable(name));
            }
            return Some(crate::mir::expr::Pattern::Wildcard);
        }

        if let Some(tok) = self.peek()
            && let TokenType::Identifier(ref name) = tok.token_type
        {
            if let Some(next) = self.tokens.get(self.current + 1)
                && next.token_type == TokenType::Colon
            {
                let name = name.clone();
                self.advance();
                self.advance();
                if let Some(inner) = self.parse_pattern() {
                    return Some(crate::mir::expr::Pattern::TypeAscription {
                        name,
                        pattern: Box::new(inner),
                    });
                }
                return None;
            }
            let name = self.consume_identifier("Expected pattern")?;
            return Some(crate::mir::expr::Pattern::Variable(name));
        }

        if let Some(literal) = self.try_parse_literal_pattern() {
            return Some(crate::mir::expr::Pattern::Literal(literal));
        }

        if self.match_token_exact(TokenType::LBracket) {
            let mut elements = Vec::new();
            let mut rest = None;
            loop {
                if self.match_token_exact(TokenType::RBracket) {
                    break;
                }
                if self.check(&TokenType::DotDot) || self.check(&TokenType::DotDotDot) {
                    self.advance();
                    let name_opt = if let Some(tok) = self.peek() {
                        if let TokenType::Identifier(ref name) = tok.token_type {
                            Some(name.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(name) = name_opt {
                        self.advance();
                        rest = Some(Box::new(
                            crate::mir::expr::Pattern::Variable(name),
                        ));
                    } else {
                        rest = Some(Box::new(crate::mir::expr::Pattern::Wildcard));
                    }
                    self.consume(TokenType::RBracket, "Expected ']' after rest pattern")?;
                    break;
                }
                if let Some(elem) = self.parse_pattern() {
                    elements.push(elem);
                } else {
                    return None;
                }
                if !self.match_token_exact(TokenType::Comma) {
                    self.consume(TokenType::RBracket, "Expected ']' after list pattern")?;
                    break;
                }
            }
            return Some(crate::mir::expr::Pattern::ListVec { elements, rest });
        }

        if self.match_token_exact(TokenType::LBrace) {
            let mut required = Vec::new();
            let mut rest = false;
            loop {
                if self.match_token_exact(TokenType::RBrace) {
                    break;
                }
                let key = self.consume_identifier("Expected dict key")?;
                self.consume(TokenType::Colon, "Expected ':' after dict key")?;
                if let Some(value_pat) = self.parse_pattern() {
                    required.push((key, value_pat));
                } else {
                    return None;
                }
                if !self.match_token_exact(TokenType::Comma) {
                    self.consume(TokenType::RBrace, "Expected '}' after dict pattern")?;
                    break;
                }
                if self.check(&TokenType::DotDot) || self.check(&TokenType::DotDotDot) {
                    self.advance();
                    rest = true;
                    self.consume(TokenType::RBrace, "Expected '}' after dict rest")?;
                    break;
                }
            }
            return Some(crate::mir::expr::Pattern::Dict { required, rest });
        }

        None
    }

    fn try_parse_literal_pattern(&mut self) -> Option<crate::common::Literal> {
        let token = self.peek()?.token_type.clone();

        match token {
            TokenType::True => {
                self.advance();
                Some(crate::common::Literal::Bool(true, self.span_of_current()))
            }
            TokenType::False => {
                self.advance();
                Some(crate::common::Literal::Bool(false, self.span_of_current()))
            }
            TokenType::Int(val) => {
                self.advance();
                Some(crate::common::Literal::Int(val, self.span_of_current()))
            }
            TokenType::Float(val) => {
                self.advance();
                Some(crate::common::Literal::Float(val, self.span_of_current()))
            }
            TokenType::String(s) => {
                self.advance();
                Some(crate::common::Literal::String(s, self.span_of_current()))
            }
            TokenType::Nil => {
                self.advance();
                Some(crate::common::Literal::Nil(self.span_of_current()))
            }
            _ => None,
        }
    }

    pub(super) fn parse_orchestrate_statement(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Orchestrate) {
            return None;
        }

        let start_span = self.span_of_current();

        let kind_str = if self.check(&TokenType::Loop) {
            self.advance();
            "loop".to_string()
        } else {
            let name = self.consume_identifier(
                "Expected orchestrate kind (sequential/loop/graph/pregel/moa/moe)",
            )?;
            if name != "sequential"
                && name != "graph"
                && name != "pregel"
                && name != "moa"
                && name != "moe"
            {
                eprintln!(
                    "Parse error: Expected orchestrate kind (sequential/loop/graph/pregel/moa/moe) at line {}",
                    self.current_line()
                );
                return None;
            }
            name
        };

        let input_var = self.consume_identifier("Expected input variable")?;
        self.consume(TokenType::Arrow, "Expected '->' after input variable")?;
        let result_var = self.consume_identifier("Expected result variable")?;
        let _ = self.match_token(&[TokenType::Comma]);

        let mut agents: Vec<MirOrchestrateAgent> = Vec::new();
        let mut edges: Vec<MirOrchestrateEdge> = Vec::new();
        let mut exit_when: Option<MirExpr> = None;
        let mut moa_layers: Option<usize> = None;
        let mut moa_proposers: Vec<String> = Vec::new();
        let mut moa_aggregator: Option<String> = None;
        let mut moa_prompt: Option<MirExpr> = None;
        let mut moe_experts: Vec<crate::mir::expr::MirMoeExpert> = Vec::new();
        let mut moe_router: Option<MirExpr> = None;
        let mut moe_top_k: Option<usize> = None;
        let mut moe_prompt: Option<MirExpr> = None;

        loop {
            while self.match_token(&[TokenType::Newline]) {}

            if self.is_at_end() {
                break;
            }

            if self.check(&TokenType::End) {
                self.advance();
                break;
            }

            if kind_str == "moa" {
                let is_moa_field = self.peek_is_identifier("layers")
                    || self.peek_is_identifier("proposers")
                    || self.peek_is_identifier("aggregator")
                    || self.peek_is_identifier("prompt");
                if is_moa_field {
                    let field = self.consume_identifier("Expected moa field")?;
                    self.consume(TokenType::Colon, "Expected ':' after moa field")?;
                    match field.as_str() {
                        "layers" => {
                            let tok = self.advance()?;
                            let n = match tok.token_type {
                                TokenType::Float(f) => f.max(1.0) as usize,
                                TokenType::Int(i) => i.max(1) as usize,
                                _ => return None,
                            };
                            moa_layers = Some(n);
                        }
                        "proposers" => {
                            if !self.match_token_exact(TokenType::LBracket) {
                                return None;
                            }
                            while !self.check(&TokenType::RBracket) && !self.is_at_end() {
                                if let TokenType::String(s) = self.peek().cloned()?.token_type {
                                    self.advance();
                                    moa_proposers.push(s);
                                }
                                if !self.match_token(&[TokenType::Comma]) {
                                    break;
                                }
                            }
                            self.consume(TokenType::RBracket, "Expected ']' after proposers")?;
                        }
                        "aggregator" => {
                            if let TokenType::String(s) = self.peek().cloned()?.token_type {
                                self.advance();
                                moa_aggregator = Some(s);
                            }
                        }
                        "prompt" => {
                            moa_prompt = self.parse_assignment();
                        }
                        _ => return None,
                    }
                    continue;
                }
            }

            if kind_str == "moe" {
                let is_moe_field = self.peek_is_identifier("experts")
                    || self.peek_is_identifier("router")
                    || self.peek_is_identifier("top_k")
                    || self.peek_is_identifier("prompt");
                if is_moe_field {
                    let field = self.consume_identifier("Expected moe field")?;
                    self.consume(TokenType::Colon, "Expected ':' after moe field")?;
                    match field.as_str() {
                        "experts" => {
                            if !self.match_token_exact(TokenType::LBrace) {
                                return None;
                            }
                            while !self.check(&TokenType::RBrace) && !self.is_at_end() {
                                while self.match_token(&[TokenType::Newline]) {}
                                let name = match self.peek().cloned()?.token_type {
                                    TokenType::String(s) => {
                                        self.advance();
                                        s
                                    }
                                    _ => return None,
                                };
                                self.consume(TokenType::Colon, "Expected ':' after expert name")?;
                                if let Some(def) = self.parse_assignment() {
                                    moe_experts.push(crate::mir::expr::MirMoeExpert { name, def });
                                }
                                while self.match_token(&[TokenType::Newline]) {}
                                if !self.match_token(&[TokenType::Comma]) {
                                    break;
                                }
                            }
                            self.consume(TokenType::RBrace, "Expected '}' after experts")?;
                        }
                        "router" => {
                            moe_router = self.parse_assignment();
                        }
                        "top_k" => {
                            let tok = self.advance()?;
                            let n = match tok.token_type {
                                TokenType::Float(f) => f.max(1.0) as usize,
                                TokenType::Int(i) => i.max(1) as usize,
                                _ => return None,
                            };
                            moe_top_k = Some(n);
                        }
                        "prompt" => {
                            moe_prompt = self.parse_assignment();
                        }
                        _ => return None,
                    }
                    continue;
                }
            }

            if self.peek_is_identifier("max_rounds") || self.check(&TokenType::MaxRounds) {
                self.advance();
                self.consume(TokenType::Colon, "Expected ':' after 'max_rounds'")?;
                while !self.check(&TokenType::Newline) && !self.is_at_end() {
                    self.advance();
                }
                continue;
            }

            if self.peek_is_identifier("on") {
                self.advance();
                self.consume(TokenType::Colon, "Expected ':' after 'on'")?;
                if let Some(cond) = self.parse_assignment() {
                    exit_when = Some(cond);
                }
                continue;
            }

            if self.peek_is_identifier("agent") {
                if let Some(agent) = self.parse_agent_def() {
                    agents.push(agent);
                    continue;
                }
                return None;
            }

            if let Some(edge) = self.try_parse_edge_def() {
                edges.push(edge);
                continue;
            }

            break;
        }

        let kind = match kind_str.as_str() {
            "sequential" => MirOrchestrateKind::Sequential { agents },
            "loop" => {
                let agent = agents
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| MirOrchestrateAgent {
                        name: "default".to_string(),
                        with_config: None,
                        task_expr: MirExpr::lit(
                            crate::common::Literal::Nil(start_span),
                            start_span,
                        ),
                        verify_expr: None,
                        task_body: MirFunction {
                            params: vec![],
                            body: vec![],
                            n_regs: 0,
                            ..Default::default()
                        },
                        combiner_body: None,
                    });
                MirOrchestrateKind::Loop {
                    agents: vec![agent],
                    rounds: Some(1000),
                    exit_when,
                }
            }
            "graph" => MirOrchestrateKind::Graph { agents, edges },
            "pregel" => MirOrchestrateKind::Pregel {
                agents,
                edges,
                state_schema: vec![],
                checkpoint: None,
                interrupt_points: vec![],
                adjacency: HashMap::new(),
            },
            "moa" => {
                let layers = moa_layers.unwrap_or(2);
                let proposers = if moa_proposers.is_empty() {
                    vec!["gpt-4o".to_string()]
                } else {
                    moa_proposers
                };
                let aggregator = moa_aggregator.unwrap_or_else(|| proposers[0].clone());
                let prompt =
                    moa_prompt.unwrap_or_else(|| MirExpr::var("input".to_string(), start_span));
                MirOrchestrateKind::Moa {
                    layers,
                    proposers,
                    aggregator,
                    prompt,
                }
            }
            "moe" => {
                let router =
                    moe_router.unwrap_or_else(|| MirExpr::var("input".to_string(), start_span));
                let top_k = moe_top_k.unwrap_or(2);
                let prompt =
                    moe_prompt.unwrap_or_else(|| MirExpr::var("input".to_string(), start_span));
                MirOrchestrateKind::Moe {
                    experts: moe_experts,
                    router,
                    top_k,
                    prompt,
                }
            }
            _ => MirOrchestrateKind::Sequential { agents },
        };

        Some(MirExpr {
            kind: MirExprKind::Orchestrate {
                input_var,
                result_var,
                kind: Box::new(kind),
            },
            span: start_span,
        })
    }

    pub(super) fn peek_is_identifier(&self, name: &str) -> bool {
        self.peek()
            .map(|t| {
                matches!(&t.token_type, TokenType::Identifier(s) if s == name)
                    || token_to_identifier_name(&t.token_type) == Some(name)
            })
            .unwrap_or(false)
    }

    fn parse_agent_def(&mut self) -> Option<MirOrchestrateAgent> {
        let saved = self.current;

        self.advance();
        let name = match self.consume_identifier("Expected agent name") {
            Some(n) => n,
            None => {
                self.current = saved;
                return None;
            }
        };

        let _params = if self.match_token_exact(TokenType::LParen) {
            let mut params = Vec::new();
            while !self.check(&TokenType::RParen) && !self.is_at_end() {
                if let Some(p) = self.consume_identifier("Expected parameter name") {
                    params.push(p);
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            if self
                .consume(TokenType::RParen, "Expected ')' after parameters")
                .is_none()
            {
                self.current = saved;
                return None;
            }
            Some(params)
        } else {
            None
        };

        if !self.match_token_exact(TokenType::FatArrow) {
            self.current = saved;
            return None;
        }
        let body = match self.parse_assignment() {
            Some(b) => b,
            None => {
                self.current = saved;
                return None;
            }
        };

        let lowered_body = match crate::mir::lower::lower_mir_exprs(std::slice::from_ref(&body)) {
            Ok(f) => f,
            Err(_) => {
                self.current = saved;
                return None;
            }
        };
        Some(MirOrchestrateAgent {
            name,
            with_config: None,
            task_expr: body,
            verify_expr: None,
            task_body: lowered_body,
            combiner_body: None,
        })
    }

    fn try_parse_edge_def(&mut self) -> Option<MirOrchestrateEdge> {
        let saved = self.current;

        let from = if self.check(&TokenType::At) {
            self.advance();
            let node_name = self.consume_identifier("Expected node name after @")?;
            format!("@{}", node_name)
        } else {
            let name = match self.peek()?.token_type {
                TokenType::Identifier(ref s) => s.clone(),
                _ => return None,
            };
            self.advance();
            name
        };

        if !self.match_token_exact(TokenType::Arrow) {
            self.current = saved;
            return None;
        }

        let to = if self.check(&TokenType::At) {
            self.advance();
            let node_name = self.consume_identifier("Expected node name after @")?;
            format!("@{}", node_name)
        } else {
            let name = match self.peek()?.token_type {
                TokenType::Identifier(ref s) => s.clone(),
                _ => {
                    self.current = saved;
                    return None;
                }
            };
            self.advance();
            name
        };

        let mut condition = None;
        if self.peek_is_identifier("on") {
            self.advance();
            self.consume(TokenType::Colon, "Expected ':' after 'on'")?;
            if let Some(cond) = self.parse_assignment() {
                condition = Some(cond);
            }
        }

        Some(MirOrchestrateEdge {
            from,
            to,
            condition_expr: condition,
            condition_body: None,
        })
    }

    fn parse_let_binding(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        if !self.match_token_exact(TokenType::Let) {
            return None;
        }
        let name = self.consume_identifier("Expected variable name after 'let'")?;
        let type_hint = if self.match_token_exact(TokenType::Colon) {
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        self.consume(TokenType::Assign, "Expected '=' after variable name")?;
        let value = self.parse_assignment()?;
        let nil = MirExpr::lit(Literal::Nil(span), span);
        let _ = self.match_token(&[TokenType::Newline]);
        Some(MirExpr {
            kind: MirExprKind::LetBinding {
                name,
                type_hint,
                value: Box::new(value),
                init_body: Box::new(nil),
            },
            span,
        })
    }

    fn parse_block_body(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        let mut exprs = Vec::new();

        loop {
            while self.match_token(&[TokenType::Newline]) {}

            if self.is_at_end() || self.check(&TokenType::RBrace) || self.check(&TokenType::End) {
                break;
            }

            let stmt = if self.check(&TokenType::Let) {
                self.parse_let_binding()
            } else {
                self.parse_assignment().or_else(|| {
                    let tok = self.peek()?.token_type.clone();
                    match tok {
                        TokenType::If => self.parse_if_expression(),
                        TokenType::For => self.parse_for_loop(),
                        _ => {
                            if let TokenType::Identifier(ref n) = tok && n == "while" {
                                return self.parse_while_loop();
                            }
                            None
                        }
                    }
                })
            };
            if let Some(e) = stmt {
                exprs.push(e);
            } else {
                self.advance();
            }

            let _ = self.match_token(&[TokenType::Newline, TokenType::Comma]);
        }

    if exprs.is_empty() {
        return Some(MirExpr::lit(Literal::Nil(span), span));
    }
        if exprs.len() == 1 {
            return Some(exprs.into_iter().next().expect("len==1"));
        }
        Some(MirExpr {
            kind: MirExprKind::Sequence(exprs),
            span,
        })
    }

    fn parse_if_expression(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::If) {
            return None;
        }

        let expr_span = self.span_of_current();
        let cond = self.parse_expression()?;

        if self.match_token_exact(TokenType::Then) {
            let then_branch = self.parse_assignment()?;

            let else_branch = if self
                .peek()
                .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "else"))
                .unwrap_or(false)
            {
                self.advance();
                Some(self.parse_assignment()?)
            } else {
                None
            };

            while self.match_token(&[TokenType::Newline]) {}
            let _ = self.match_token(&[TokenType::End]);

            return Some(MirExpr::if_else(cond, then_branch, else_branch, expr_span));
        }

        if self.match_token_exact(TokenType::LBrace) {
            let then_branch = self.parse_block_body()?;
            self.consume(TokenType::RBrace, "Expected closing brace '}}'")?;

            let else_branch = if self
                .peek()
                .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "else"))
                .unwrap_or(false)
            {
                self.advance();
                self.consume(TokenType::LBrace, "Expected '{' after 'else'")?;
                let else_expr = self.parse_block_body()?;
                self.consume(TokenType::RBrace, "Expected closing brace '}}' after else")?;
                Some(else_expr)
            } else {
                None
            };

            return Some(MirExpr::if_else(cond, then_branch, else_branch, expr_span));
        }

        None
    }

    fn parse_for_loop(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::For) {
            return None;
        }

        let expr_span = self.span_of_current();
        let var = self.consume_identifier("Expected variable name after 'for'")?;

        if !self.match_token_exact(TokenType::In) {
            eprintln!(
                "Parse error: Expected 'in' after 'for' variable at line {}",
                self.current_line()
            );
            return None;
        }

        let iterable = self.parse_assignment()?;

        let body = if self.match_token_exact(TokenType::LBrace) {
            let b = self.parse_block_body()?;
            self.consume(TokenType::RBrace, "Expected '}' after for loop body")?;
            b
        } else {
            let _ = self.match_token(&[TokenType::Newline]);
            let b = self.parse_block_body()?;
            self.consume(TokenType::End, "Expected 'end' after for loop body")?;
            b
        };

        Some(MirExpr {
            kind: MirExprKind::Loop {
                var,
                iterable: Box::new(iterable),
                body: Box::new(body),
            },
            span: expr_span,
        })
    }

    fn parse_while_loop(&mut self) -> Option<MirExpr> {
        if !self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "while"))
            .unwrap_or(false)
        {
            return None;
        }
        self.advance();

        let expr_span = self.span_of_current();
        let cond = self.parse_assignment()?;

        let body = if self.match_token_exact(TokenType::LBrace) {
            let b = self.parse_block_body()?;
            self.consume(TokenType::RBrace, "Expected '}' after while loop body")?;
            b
        } else {
            let _ = self.match_token(&[TokenType::Newline]);
            let b = self.parse_block_body()?;
            self.consume(TokenType::End, "Expected 'end' after while loop body")?;
            b
        };

        Some(MirExpr {
            kind: MirExprKind::While {
                cond: Box::new(cond),
                body: Box::new(body),
            },
            span: expr_span,
        })
    }

    fn parse_return_break_continue(&mut self) -> Option<MirExpr> {
        let token = self.peek()?.token_type.clone();
        let span = self.span_of_current();

        match token {
            TokenType::Return => {
                self.advance();
                let value = if self.check(&TokenType::Newline)
                    || self.check(&TokenType::RBrace)
                    || self.is_at_end()
                {
                    None
                } else {
                    self.parse_assignment()
                };
                Some(MirExpr {
                    kind: MirExprKind::Return(value.map(Box::new)),
                    span,
                })
            }
            TokenType::Break => {
                self.advance();
                let label = if matches!(self.peek()?.token_type, TokenType::Identifier(_)) {
                    Some(self.consume_identifier("Expected label after 'break'")?)
                } else {
                    None
                };
                Some(MirExpr {
                    kind: MirExprKind::Break(label.unwrap_or_default()),
                    span,
                })
            }
            TokenType::Continue => {
                self.advance();
                let label = if matches!(self.peek()?.token_type, TokenType::Identifier(_)) {
                    Some(self.consume_identifier("Expected label after 'continue'")?)
                } else {
                    None
                };
                Some(MirExpr {
                    kind: MirExprKind::Continue(label.unwrap_or_default()),
                    span,
                })
            }
            _ => None,
        }
    }

    pub(super) fn parse_assignment(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();

        if matches!(self.peek()?.token_type, TokenType::Identifier(_)) {
            let ident_start = self.current;
            let name = self.consume_identifier("Expected variable name")?;

            if self.match_token(&[TokenType::Assign]) {
                let value = self.parse_assignment()?;

                return Some(MirExpr {
                    kind: MirExprKind::Assign {
                        target: name,
                        value: Box::new(value),
                    },
                    span,
                });
            }

            self.current = ident_start;
        }

        self.parse_or()
    }

    fn parse_or(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        let mut left = self.parse_and()?;

        while self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "or"))
            .unwrap_or(false)
        {
            self.advance();
            let right = self.parse_and()?;
            left = MirExpr {
                kind: MirExprKind::Or {
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }

        Some(left)
    }

    fn parse_and(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        let mut left = self.parse_equality()?;

        while self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "and"))
            .unwrap_or(false)
        {
            self.advance();
            let right = self.parse_equality()?;
            left = MirExpr {
                kind: MirExprKind::And {
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }

        Some(left)
    }

    fn parse_equality(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_pipe()?;

        while let Some(op) = self.consume_binary_op(&[TokenType::Equal, TokenType::NotEqual]) {
            let right = self.parse_pipe()?;
            left = MirExpr::binop(op, left, right, self.span_of_current());
        }

        Some(left)
    }

    fn parse_comparison(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_term()?;

        while let Some(op) = self.consume_binary_op(&[
            TokenType::Less,
            TokenType::Greater,
            TokenType::LessEqual,
            TokenType::GreaterEqual,
        ]) {
            let right = self.parse_term()?;
            left = MirExpr::binop(op, left, right, self.span_of_current());
        }

        Some(left)
    }

    fn parse_term(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_factor()?;

        while let Some(op) = self.consume_binary_op(&[TokenType::Minus, TokenType::Plus]) {
            let right = self.parse_factor()?;
            left = MirExpr::binop(op, left, right, self.span_of_current());
        }

        Some(left)
    }

    fn parse_factor(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_unary()?;

        while let Some(op) =
            self.consume_binary_op(&[TokenType::Star, TokenType::Slash, TokenType::Percent])
        {
            let right = self.parse_unary()?;
            left = MirExpr::binop(op, left, right, self.span_of_current());
        }

        Some(left)
    }

    fn parse_unary(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();

        if self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "not"))
            .unwrap_or(false)
        {
            self.advance();
            let operand = self.parse_unary()?;
            let zero = MirExpr::lit(Literal::Int(0, span), span);
            let negated = MirExpr::binop(BinaryOp::Equal, zero, operand, span);
            return Some(negated);
        }

        if self.check(&TokenType::Minus) || self.check(&TokenType::Bang) {
            let _op = self.advance()?;
            let operand = self.parse_unary()?;

            let zero = MirExpr::lit(
                Literal::Int(0, self.span_of_current()),
                self.span_of_current(),
            );
            let negated = MirExpr::binop(BinaryOp::Sub, zero, operand, self.span_of_current());
            return Some(negated);
        }

        let expr = self.parse_call()?;

        if self.check(&TokenType::As) {
            self.advance();
            if self.check(&TokenType::Dyn) {
                self.advance();
                let trait_name = self.consume_identifier("Expected trait name after 'dyn'")?;
                let generics = if self.check(&TokenType::Less) {
                    self.advance();
                    let mut gens = Vec::new();
                    loop {
                        if let Some(ty) = self.parse_type_annotation() {
                            gens.push(ty);
                        }
                        if !self.match_token(&[TokenType::Comma]) {
                            break;
                        }
                    }
                    self.consume(TokenType::Greater, "Expected '>' after generics")?;
                    gens
                } else {
                    Vec::new()
                };
                return Some(MirExpr {
                    kind: MirExprKind::DynTrait {
                        expr: Box::new(expr),
                        trait_name,
                        generics,
                    },
                    span,
                });
            }
        }

        Some(expr)
    }

    fn parse_call(&mut self) -> Option<MirExpr> {
        let mut callee = self.parse_primary()?;

        loop {
            if self.match_token_exact(TokenType::LParen) {
                let args = self.parse_argument_list().ok()?;
                callee = MirExpr::call(
                    MirCallee::Name(match_to_string(&callee).to_string()),
                    args,
                    self.span_of_current(),
                );
            } else if self.match_token_exact(TokenType::Dot) {
                let method_name = self.consume_identifier("Expected method name")?;
                let mut args = Vec::new();

                if self.match_token_exact(TokenType::LParen) {
                    args = self.parse_argument_list().ok()?;
                }

                let old_callee = callee.clone();
                callee = MirExpr::call(
                    MirCallee::Method(match_to_string(&old_callee), method_name),
                    std::iter::once(old_callee).chain(args).collect(),
                    self.span_of_current(),
                );
            } else if self.match_token_exact(TokenType::ColonColon) {
                let method_name = self.consume_identifier("Expected method name after '::'")?;
                let old_name = match_to_string(&callee);
                let qualified = format!("{}::{}", old_name, method_name);
                callee = MirExpr::var(qualified, self.span_of_current());
            } else if self.check(&TokenType::LBracket) {
                self.advance();
                let index = self.parse_assignment()?;
                self.consume(TokenType::RBracket, "Expected ']'")?;
                let old_callee = callee.clone();
                callee = MirExpr::call(
                    MirCallee::Name(format!("{}_index", match_to_string(&old_callee))),
                    vec![old_callee, index],
                    self.span_of_current(),
                );
            } else {
                break;
            }
        }

        Some(callee)
    }

    fn parse_pipe(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_comparison()?;

        while self.match_token_exact(TokenType::Pipe) {
            let right = self.parse_comparison()?;
            let right_span = right.span;

            let (callee, args) = match right.kind {
                MirExprKind::Call {
                    callee: MirCallee::Name(name),
                    mut args,
                } => {
                    args.insert(0, left);
                    (MirCallee::Name(name), args)
                }
                _ => {
                    let name = match_to_string(&right);
                    (MirCallee::Name(name), vec![left])
                }
            };
            left = MirExpr::call(callee, args, right_span);
        }

        Some(left)
    }

    fn parse_argument_list(&mut self) -> Result<Vec<MirExpr>, ParseError> {
        let mut args = Vec::new();

        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            if let Some(arg) = self.parse_assignment() {
                args.push(arg);
            }

            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }

        if self.consume(TokenType::RParen, "Expected ')'").is_none() {
            return Err(ParseError("Expected ')'".to_string()));
        }
        Ok(args)
    }

    fn parse_primary(&mut self) -> Option<MirExpr> {
        let token = self.peek().cloned()?;
        let span = crate::common::Span::new(token.line, token.column);

        match token.token_type {
            TokenType::Int(val) => {
                self.advance();
                Some(MirExpr::lit(Literal::Int(val, span), span))
            }
            TokenType::Float(val) => {
                self.advance();
                Some(MirExpr::lit(Literal::Float(val, span), span))
            }
            TokenType::String(_) => self.parse_string_literal(),
            TokenType::PromptString(_) => self.parse_string_literal(),
            TokenType::True => {
                self.advance();
                Some(MirExpr::lit(Literal::Bool(true, span), span))
            }
            TokenType::False => {
                self.advance();
                Some(MirExpr::lit(Literal::Bool(false, span), span))
            }
            TokenType::Nil => {
                self.advance();
                Some(MirExpr::lit(Literal::Nil(span), span))
            }
            TokenType::Identifier(name) => {
                self.advance();
                Some(MirExpr::var(name, span))
            }
            TokenType::LBracket => self.parse_list(),
            TokenType::LBrace => self.parse_dict(),
            TokenType::LParen => {
                self.advance();
                let inner = self.parse_expression()?;
                let paren_parsed = self.consume(TokenType::RParen, "Expected ')'");
                paren_parsed?;
                Some(mir_group(inner))
            }
            TokenType::Fn => {
                self.advance();
                if !self.match_token_exact(TokenType::LParen) {
                    eprintln!(
                        "Parse error: Expected '(' after 'fn' at line {}",
                        self.current_line()
                    );
                    return None;
                }
                let mut params = Vec::new();
                while !self.check(&TokenType::RParen) && !self.is_at_end() {
                    if let Some(p) = self.consume_identifier("Expected parameter name") {
                        params.push(Param {
                            name: p,
                            type_hint: None,
                            default: None,
                        });
                    }
                    if !self.match_token(&[TokenType::Comma]) {
                        break;
                    }
                }
                self.consume(TokenType::RParen, "Expected ')' after parameters")?;

                if self.match_token_exact(TokenType::FatArrow) {
                    let body = self.parse_assignment()?;
                    Some(MirExpr::closure(params, body, span))
                } else {
                    let body = self.parse_block_body()?;
                    self.consume(TokenType::End, "Expected 'end' after closure body")?;
                    Some(MirExpr::closure(params, body, span))
                }
            }
            _ => None,
        }
    }

    pub(super) fn parse_expression(&mut self) -> Option<MirExpr> {
        self.parse_assignment()
    }

    fn parse_string_literal(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        let tok = self.advance()?;

        match tok.token_type {
            TokenType::String(ref s) => {
                Some(MirExpr::lit(Literal::String(s.clone(), span), span))
            }
            TokenType::PromptString(ref s) => {
                let parts = parse_prompt_parts(s, span);
                Some(MirExpr {
                    kind: MirExprKind::Prompt { parts },
                    span,
                })
            }
            _ => None,
        }
    }

    fn parse_list(&mut self) -> Option<MirExpr> {
        self.consume(TokenType::LBracket, "Expected '['")?;

        let mut items = Vec::new();
        while !self.check(&TokenType::RBracket) && !self.is_at_end() {
            if let Some(item) = self.parse_assignment() {
                items.push(item);
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }

        self.consume(TokenType::RBracket, "Expected ']'")?;
        Some(MirExpr::list(items, self.span_of_current()))
    }

    fn parse_dict(&mut self) -> Option<MirExpr> {
        self.consume(TokenType::LBrace, "Expected '{'")?;

        let mut entries = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            if let Some(key) = self.parse_assignment() {
                self.consume(TokenType::Colon, "Expected ':' after dict key")?;
                if let Some(value) = self.parse_assignment() {
                    let key_str = match key.kind {
                        MirExprKind::Variable(n) => n,
                        MirExprKind::Literal(Literal::String(s, _)) => s,
                        _ => format!("{:?}", key),
                    };
                    entries.push((key_str, value));
                }
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }

        self.consume(TokenType::RBrace, "Expected '}'")?;
        Some(MirExpr::dict(entries, self.span_of_current()))
    }

    pub(super) fn parse_type_annotation(&mut self) -> Option<crate::typeck::Type> {
        use crate::typeck::Type;
        // v0.85: 先解析一个基础类型，再收集 `|` 分隔的 union 成员
        let ty = self.parse_single_type_annotation()?;

        // v0.85: 收集 union 成员 (string | number | bool)
        let mut members: Vec<Type> = vec![ty];
        while self.match_token_exact(TokenType::Or) {
            // 跳过 `|` 后的可选空格/换行
            let next = self.peek();
            if next.is_none() {
                eprintln!(
                    "Parse error: expected type after '|' at line {}",
                    self.current_line()
                );
                return None;
            }
            let member = self.parse_single_type_annotation()?;
            members.push(member);
        }

        if members.len() == 1 {
            Some(members.into_iter().next().unwrap())
        } else {
            Some(Type::Union(members))
        }
    }

    /// 解析单个类型注解（不含 `|` 联合），内部由 `parse_type_annotation` 调用。
    fn parse_single_type_annotation(&mut self) -> Option<crate::typeck::Type> {
        use crate::typeck::Type;
        let tok = self.peek().cloned()?;
        match &tok.token_type {
            TokenType::Dyn => {
                self.advance();
                let name = self.consume_identifier("Expected trait name after 'dyn'")?;
                let generics = if self.match_token(&[TokenType::Less]) {
                    let mut g: Vec<Type> = Vec::new();
                    loop {
                        g.push(self.parse_type_annotation()?);
                        if !self.match_token(&[TokenType::Comma]) {
                            break;
                        }
                    }
                    if !self.match_token(&[TokenType::Greater]) {
                        eprintln!(
                            "Parse error: expected '>' in dyn trait generics at line {}",
                            self.current_line()
                        );
                        return None;
                    }
                    g
                } else {
                    Vec::new()
                };
                Some(Type::TraitObject {
                    trait_name: name,
                    generics,
                })
            }
            TokenType::Identifier(name) => {
                let lower = name.to_lowercase();
                if matches!(
                    self.tokens.get(self.current + 1).map(|t| &t.token_type),
                    Some(TokenType::Less)
                ) {
                    self.advance();
                    self.advance();
                    let mut args: Vec<Type> = Vec::new();
                    loop {
                        args.push(self.parse_type_annotation()?);
                        if self.match_token(&[TokenType::Comma]) {
                            continue;
                        }
                        break;
                    }
                    if !self.match_token(&[TokenType::Greater]) {
                        eprintln!(
                            "Parse error: expected '>' after generic type arguments at line {}",
                            self.current_line()
                        );
                        return None;
                    }
                    return match lower.as_str() {
                        "list" => Some(Type::List(Box::new(
                            args.into_iter().next().unwrap_or(Type::Unknown),
                        ))),
                        "dict" => {
                            let mut it = args.into_iter();
                            let k = it.next().unwrap_or(Type::Unknown);
                            let v = it.next().unwrap_or(Type::Unknown);
                            Some(Type::Dict(Box::new(k), Box::new(v)))
                        }
                        other => {
                            eprintln!(
                                "Parse error: unsupported generic type annotation '{}' at line {}",
                                other,
                                self.current_line()
                            );
                            None
                        }
                    };
                }
                let ty = match lower.as_str() {
                    "int" | "number" => Type::Int,
                    "float" => Type::Float,
                    "string" => Type::String,
                    "char" => Type::Char,
                    "bool" => Type::Bool,
                    "nil" => Type::Nil,
                    "any" => Type::Any,
                    "unknown" => Type::Unknown,
                    other => {
                        eprintln!(
                            "Parse error: unsupported type annotation '{}' at line {}",
                            other,
                            self.current_line()
                        );
                        return None;
                    }
                };
                self.advance();
                Some(ty)
            }
            _ => {
                eprintln!(
                    "Parse error: expected type annotation at line {}",
                    self.current_line()
                );
                None
            }
        }
    }

    fn parse_type_alias(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Type) {
            return None;
        }
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected type name after 'type'")?;
        self.consume(TokenType::Assign, "Expected '=' after type alias name")?;
        let target = self.parse_type_annotation()?;
        let _ = self.match_token(&[TokenType::Newline]);
        Some(MirExpr {
            kind: MirExprKind::TypeAlias { name, target },
            span,
        })
    }

    fn parse_enum_def(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Enum) {
            return None;
        }
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected enum name after 'enum'")?;
        let _ = self.match_token(&[TokenType::Newline]);

        let mut variants = Vec::new();
        loop {
            while self.match_token(&[TokenType::Newline]) {}
            if self.is_at_end() || self.check(&TokenType::End) {
                if self.check(&TokenType::End) {
                    self.advance();
                }
                break;
            }
            if let Some(v) = self.consume_identifier("Expected variant name") {
                variants.push(v);
            }
        }
        Some(MirExpr {
            kind: MirExprKind::EnumDef { name, variants },
            span,
        })
    }

    fn parse_struct_def(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Struct) {
            return None;
        }
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected struct name after 'struct'")?;
        let _ = self.match_token(&[TokenType::Newline]);

        let mut fields = Vec::new();
        loop {
            while self.match_token(&[TokenType::Newline]) {}
            if self.is_at_end() || self.check(&TokenType::End) {
                if self.check(&TokenType::End) {
                    self.advance();
                }
                break;
            }
            if let Some(field_name) = self.consume_identifier("Expected field name") {
                self.consume(TokenType::Colon, "Expected ':' after field name")?;
                if let Some(field_type) = self.parse_type_annotation() {
                    fields.push((field_name, field_type));
                }
            }
        }
        Some(MirExpr {
            kind: MirExprKind::StructDef { name, fields },
            span,
        })
    }

    fn parse_import_statement(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Import) {
            return None;
        }
        let span = self.span_of_current();
        let path = match self.peek()?.token_type {
            TokenType::String(ref s) => {
                let p = s.clone();
                self.advance();
                p
            }
            _ => {
                eprintln!(
                    "Parse error: Expected string after 'import' at line {}",
                    self.current_line()
                );
                return None;
            }
        };
        let _ = self.match_token(&[TokenType::Newline]);
        Some(MirExpr {
            kind: MirExprKind::Import(path),
            span,
        })
    }

    fn parse_macro_def(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Macro) {
            return None;
        }
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected macro name after 'macro'")?;

        let mut params = Vec::new();
        if self.match_token_exact(TokenType::LParen) {
            while !self.check(&TokenType::RParen) && !self.is_at_end() {
                if let Some(p) = self.consume_identifier("Expected parameter name") {
                    params.push(p);
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(TokenType::RParen, "Expected ')' after macro params")?;
        }

        let _ = self.match_token(&[TokenType::Newline]);

        loop {
            while self.match_token(&[TokenType::Newline]) {}
            if self.is_at_end() || self.check(&TokenType::End) {
                if self.check(&TokenType::End) {
                    self.advance();
                }
                break;
            }
            self.advance();
        }

        Some(MirExpr {
            kind: MirExprKind::MacroDef { name, params },
            span,
        })
    }

    /// v0.88: quasiquote `` `expr `` — MirExpr path equivalent of emit_quasiquote_w.
    ///
    /// Mirrors emit_quasiquote_w's logic: scan tokens, track depth, collect
    /// segments as MirExprs. Lowering in lower.rs resolves each MirExpr to
    /// the corresponding QuasiquoteSegment (Quote/Unquote/UnquoteSplice).
    fn parse_quasiquote(&mut self) -> Option<MirExpr> {
        let start_span = self.span_of_current();
        self.consume(TokenType::Backtick, "Expected '`' for quasiquote")?;

        let mut capture_start = match self.peek() {
            Some(t) => self.source_byte_at(t.line, t.column),
            None => self.source.len(),
        };

        let mut segments: Vec<MirExpr> = Vec::new();
        let mut depth = 0usize;

        'scan: while !self.is_at_end() {
            let tok = match self.peek() {
                Some(t) => t,
                None => break,
            };

            match &tok.token_type {
                TokenType::LParen
                | TokenType::LBracket
                | TokenType::LBrace
                | TokenType::Less => {
                    depth += 1;
                    self.advance();
                }
                TokenType::RParen
                | TokenType::RBracket
                | TokenType::RBrace
                | TokenType::Greater => {
                    depth = depth.saturating_sub(1);
                    self.advance();
                }
                TokenType::Newline if depth == 0 => {
                    break;
                }
                _ if depth == 0 => {
                    match &tok.token_type {
                        TokenType::Comma | TokenType::CommaComma => {
                            let is_splice =
                                matches!(&tok.token_type, TokenType::CommaComma);

                            // Flush static text since last capture
                            let end = self.source_byte_at(tok.line, tok.column);
                            if end > capture_start {
                                let text = &self.source[capture_start..end];
                                if !text.is_empty() {
                                    segments.push(MirExpr::lit(
                                        Literal::String(text.to_string(), start_span),
                                        start_span,
                                    ));
                                }
                            }

                            self.advance();
                            // Parse the unquote/splice sub-expression
                            if let Some(sub_expr) = self.parse_expression() {
                                if is_splice {
                                    // splice(expr) → Call{Name("splice"), [expr]}
                                    segments.push(MirExpr {
                                        kind: MirExprKind::Call {
                                            callee: MirCallee::Name("splice".to_string()),
                                            args: vec![sub_expr],
                                        },
                                        span: start_span,
                                    });
                                } else {
                                    segments.push(sub_expr);
                                }
                            } else {
                                return None;
                            }
                            if let Some(next) = self.peek() {
                                capture_start =
                                    self.source_byte_at(next.line, next.column);
                            }
                            continue 'scan;
                        }
                        _ => {
                            self.advance();
                        }
                    }
                }
                _ => {
                    self.advance();
                }
            }
        }

        // Capture trailing static text
        if let Some(tok) = self.peek() {
            let end = self.source_byte_at(tok.line, tok.column);
            if end > capture_start {
                let text = &self.source[capture_start..end];
                if !text.is_empty() {
                    segments.push(MirExpr::lit(
                        Literal::String(text.to_string(), start_span),
                        start_span,
                    ));
                }
            }
        }

        Some(MirExpr {
            kind: MirExprKind::QuasiquoteExpr(segments),
            span: start_span,
        })
    }

    pub(super) fn advance(&mut self) -> Option<&Token> {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    pub(super) fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.current)
    }

    pub(super) fn previous(&self) -> Option<&Token> {
        if self.current > 0 {
            self.tokens.get(self.current - 1)
        } else {
            None
        }
    }

    pub(super) fn is_at_end(&self) -> bool {
        if self.current >= self.tokens.len() {
            return true;
        }
        match self.tokens.get(self.current) {
            Some(t) => t.token_type == TokenType::EOF,
            None => true,
        }
    }

    pub(super) fn check(&self, token_type: &TokenType) -> bool {
        self.peek()
            .map(|t| &t.token_type == token_type)
            .unwrap_or(false)
    }

    pub(super) fn match_token(&mut self, types: &[TokenType]) -> bool {
        for tt in types {
            if self.check(tt) {
                self.advance();
                return true;
            }
        }
        false
    }

    pub(super) fn match_token_exact(&mut self, token_type: TokenType) -> bool {
        if self.check(&token_type) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn consume(&mut self, token_type: TokenType, message: &str) -> Option<()> {
        if self.check(&token_type) {
            self.advance();
            Some(())
        } else {
            eprintln!("Parse error: {} at line {}", message, self.current_line());
            None
        }
    }

    pub(super) fn consume_identifier(&mut self, message: &str) -> Option<String> {
        match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                Some(name)
            }
            Some(ref tok) => {
                if let Some(name) = token_to_identifier_name(&tok.token_type) {
                    self.advance();
                    return Some(name.to_string());
                }
                eprintln!("Parse error: {} at line {}", message, self.current_line());
                None
            }
            _ => {
                eprintln!("Parse error: {} at line {}", message, self.current_line());
                None
            }
        }
    }

    pub(super) fn current_line(&self) -> u32 {
        self.peek()
            .map(|t| t.line)
            .unwrap_or(0)
            .try_into()
            .unwrap_or(0)
    }

    pub(super) fn span_of_current(&self) -> Span {
        self.peek()
            .map(|t| Span {
                line: t.line,
                column: t.column,
            })
            .unwrap_or(Span { line: 0, column: 0 })
    }

    pub(super) fn consume_binary_op(&mut self, accepted: &[TokenType]) -> Option<BinaryOp> {
        if !accepted.iter().any(|token_type| self.check(token_type)) {
            return None;
        }

        let token = self.advance()?.token_type.clone();
        match token {
            TokenType::Plus => Some(BinaryOp::Add),
            TokenType::Minus => Some(BinaryOp::Sub),
            TokenType::Star => Some(BinaryOp::Mul),
            TokenType::Slash => Some(BinaryOp::Div),
            TokenType::Percent => Some(BinaryOp::Mod),
            TokenType::Equal => Some(BinaryOp::Equal),
            TokenType::NotEqual => Some(BinaryOp::NotEqual),
            TokenType::Greater => Some(BinaryOp::Greater),
            TokenType::Less => Some(BinaryOp::Less),
            TokenType::GreaterEqual => Some(BinaryOp::GreaterEqual),
            TokenType::LessEqual => Some(BinaryOp::LessEqual),
            _ => None,
        }
    }
}