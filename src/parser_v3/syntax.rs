//! v0.92: ParserV3 语法子系统（自 parse.rs 迁出，P1.4 拆分）。
//!
//! 这些方法直接产出 `MirWitness`（witness-native），不再经 MirExpr 桥接：
//! - `parse_pattern` / `try_parse_literal_pattern` — 模式匹配
//! - `parse_orchestrate_statement` / `parse_agent_def` / `try_parse_edge_def` — orchestrate
//! - `parse_type_annotation` / `parse_single_type_annotation` — 类型注解（含 union / 泛型）

use super::*;

/// v0.92: MoA/MoE 默认 prompt/router —— `input` 变量引用 witness。
fn input_var_witness(span: Span) -> crate::mir::witness::MirWitness {
    crate::mir::witness::MirWitness {
        kind: crate::mir::witness::WitnessKind::Variable("input".to_string()),
        span,
    }
}

impl ParserV3 {
    // ════════════════════════════════════════════════════════════════
    // Pattern 解析（witness-native）
    // ════════════════════════════════════════════════════════════════

    pub(super) fn parse_pattern(&mut self) -> Option<crate::mir::witness::WitnessPattern> {
        if let Some(tok) = self.peek()
            && let TokenType::Identifier(ref name) = tok.token_type
            && name == "_"
        {
            self.advance();
            return Some(crate::mir::witness::WitnessPattern::Wildcard);
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
                return Some(crate::mir::witness::WitnessPattern::Variable(name));
            }
            return Some(crate::mir::witness::WitnessPattern::Wildcard);
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
                    return Some(crate::mir::witness::WitnessPattern::TypeAscription {
                        name,
                        pattern: Box::new(inner),
                    });
                }
                return None;
            }
            let name = self.consume_identifier("Expected pattern")?;
            return Some(crate::mir::witness::WitnessPattern::Variable(name));
        }

        if let Some(literal) = self.try_parse_literal_pattern() {
            return Some(crate::mir::witness::WitnessPattern::Literal(literal));
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
                        rest = Some(Box::new(crate::mir::witness::WitnessPattern::Variable(
                            name,
                        )));
                    } else {
                        rest = Some(Box::new(crate::mir::witness::WitnessPattern::Wildcard));
                    }
                    self.consume(TokenType::RBracket, "Expected ']' after rest pattern")?;
                    break;
                }
                // v0.104.4: `?` 取代 `if let … else return None`
                //（clippy 1.98 的 question_mark；本函数返回 `Option`，
                // Option 的 `?` 在 `None` 时提前返回 `None`）。
                let elem = self.parse_pattern()?;
                elements.push(elem);
                if !self.match_token_exact(TokenType::Comma) {
                    self.consume(TokenType::RBracket, "Expected ']' after list pattern")?;
                    break;
                }
            }
            return Some(crate::mir::witness::WitnessPattern::ListVec { elements, rest });
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
                // v0.104.4: `?` 取代 `if let … else return None`
                //（clippy 1.98 的 question_mark）。
                let value_pat = self.parse_pattern()?;
                required.push((key, value_pat));
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
            return Some(crate::mir::witness::WitnessPattern::Dict { required, rest });
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
            // v0.91: BigInt 字面量
            TokenType::BigInt(val) => {
                self.advance();
                Some(crate::common::Literal::BigInt(val, self.span_of_current()))
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

    // ════════════════════════════════════════════════════════════════
    // Orchestrate 解析（witness-native）
    // ════════════════════════════════════════════════════════════════

    /// v0.92: 返回 MirWitness（Orchestrate 变体）——不再经 MirExpr 桥接。
    pub(super) fn parse_orchestrate_statement(
        &mut self,
    ) -> Option<crate::mir::witness::MirWitness> {
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
        // v0.92: 子表达式以 MirWitness 承载（emit_expr_w 产出）。
        let mut exit_when: Option<crate::mir::witness::MirWitness> = None;
        let mut moa_layers: Option<usize> = None;
        let mut moa_proposers: Vec<String> = Vec::new();
        let mut moa_aggregator: Option<String> = None;
        let mut moa_prompt: Option<crate::mir::witness::MirWitness> = None;
        let mut moe_experts: Vec<crate::mir::orchestrate::MirMoeExpert> = Vec::new();
        let mut moe_router: Option<crate::mir::witness::MirWitness> = None;
        let mut moe_top_k: Option<usize> = None;
        let mut moe_prompt: Option<crate::mir::witness::MirWitness> = None;

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
                            moa_prompt = self.emit_expr_w().map(|(_, w)| w);
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
                                if let Some((_reg, def_witness)) = self.emit_expr_w() {
                                    let def_fn = match crate::mir::lower::lower_mir_witnesses(
                                        std::slice::from_ref(&def_witness),
                                    ) {
                                        Ok(f) => f,
                                        Err(_) => return None,
                                    };
                                    moe_experts.push(crate::mir::orchestrate::MirMoeExpert {
                                        name,
                                        def: def_witness,
                                        def_fn,
                                    });
                                }
                                while self.match_token(&[TokenType::Newline]) {}
                                if !self.match_token(&[TokenType::Comma]) {
                                    break;
                                }
                            }
                            self.consume(TokenType::RBrace, "Expected '}' after experts")?;
                        }
                        "router" => {
                            moe_router = self.emit_expr_w().map(|(_, w)| w);
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
                            moe_prompt = self.emit_expr_w().map(|(_, w)| w);
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
                if let Some((_reg, cond)) = self.emit_expr_w() {
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
                        // v0.92: task_expr 现为 MirWitness。
                        task_expr: crate::mir::witness::MirWitness {
                            kind: crate::mir::witness::WitnessKind::Literal(
                                crate::common::Literal::Nil(start_span),
                            ),
                            span: start_span,
                        },
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
                let prompt_witness = moa_prompt.unwrap_or_else(|| input_var_witness(start_span));
                let prompt_fn = match crate::mir::lower::lower_mir_witnesses(std::slice::from_ref(
                    &prompt_witness,
                )) {
                    Ok(f) => f,
                    Err(_) => return None,
                };
                MirOrchestrateKind::Moa {
                    layers,
                    proposers,
                    aggregator,
                    prompt: prompt_witness,
                    prompt_fn,
                }
            }
            "moe" => {
                let router_witness = moe_router.unwrap_or_else(|| input_var_witness(start_span));
                let router_fn = match crate::mir::lower::lower_mir_witnesses(std::slice::from_ref(
                    &router_witness,
                )) {
                    Ok(f) => f,
                    Err(_) => return None,
                };
                let top_k = moe_top_k.unwrap_or(2);
                let prompt_witness = moe_prompt.unwrap_or_else(|| input_var_witness(start_span));
                let prompt_fn = match crate::mir::lower::lower_mir_witnesses(std::slice::from_ref(
                    &prompt_witness,
                )) {
                    Ok(f) => f,
                    Err(_) => return None,
                };
                MirOrchestrateKind::Moe {
                    experts: moe_experts,
                    router: router_witness,
                    top_k,
                    prompt: prompt_witness,
                    router_fn,
                    prompt_fn,
                }
            }
            _ => MirOrchestrateKind::Sequential { agents },
        };

        Some(crate::mir::witness::MirWitness {
            kind: crate::mir::witness::WitnessKind::Orchestrate {
                input_var,
                result_var,
                kind: Box::new(crate::mir::witness::WitnessOrchestrateKind::from_kind(
                    &kind,
                )),
            },
            span: start_span,
        })
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
        let body_witness = match self.emit_expr_w() {
            Some((_reg, w)) => w,
            None => {
                self.current = saved;
                return None;
            }
        };
        let lowered_body =
            match crate::mir::lower::lower_mir_witnesses(std::slice::from_ref(&body_witness)) {
                Ok(f) => f,
                Err(_) => {
                    self.current = saved;
                    return None;
                }
            };
        Some(MirOrchestrateAgent {
            name,
            with_config: None,
            // v0.92: task_expr 现为 MirWitness。
            task_expr: body_witness,
            verify_expr: None,
            task_body: lowered_body,
            combiner_body: None,
        })
    }

    fn try_parse_edge_def(&mut self) -> Option<MirOrchestrateEdge> {
        let saved = self.current;

        // v0.92: 消费可选的 'edge' 前缀关键字。
        // 语法形式：
        //   `edge a -> b`       — 显式 edge 前缀
        //   `a -> b`            — 裸边（向后兼容）
        //   `a -> b on: cond`   — 带条件
        if self.peek_is_identifier("edge") {
            self.advance();
        }

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
            if let Some((_reg, cond)) = self.emit_expr_w() {
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

    // ════════════════════════════════════════════════════════════════
    // 类型注解解析（含 union / 泛型）
    // ════════════════════════════════════════════════════════════════

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
            Some(
                members
                    .into_iter()
                    .next()
                    .expect("members.len() == 1 verified"),
            )
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
}
