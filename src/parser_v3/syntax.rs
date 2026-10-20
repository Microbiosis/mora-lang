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
        // v0.104.6 D269：`max_rounds` 的实值。此前该分支**只吞掉整行**
        // （见下方 `max_rounds` 处理），值从未被读取，`Loop.rounds` 又在
        // kind 构造处硬编码为 `Some(1000)` ⇒ 用户写的轮数被静默丢弃。
        let mut loop_rounds: Option<u64> = None;

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

            // v0.104.6 D269：真正读出 `max_rounds` 的值。
            //
            // 修前这里是「识别关键字 → 吃掉冒号 → 把行尾 token 全部 advance
            // 掉」，值从不落地；而 `Loop.rounds` 又在 kind 构造处写死
            // `Some(1000)`。两处叠加的结果是：`max_rounds: 5` 被**完全接受**
            // 却**完全无效**，循环照跑 1000 轮，exit 0、零诊断。
            //
            // lexer 早为它准备了专属 token（`TokenType::MaxRounds`）、
            // handler 侧也有 `rounds.unwrap_or(1000)` 的消费点 ——
            // **意图明确是「支持」**，只是中间这一段没接上。
            //
            // 解析风格与同函数内 `top_k`（见上）保持一致：非数字字面量一律
            // `return None` 走解析错误，而不是像修前那样照单全收后丢弃 ——
            // 「写错值」必须比「值被忽略」更容易被发现。
            if self.peek_is_identifier("max_rounds") || self.check(&TokenType::MaxRounds) {
                self.advance();
                self.consume(TokenType::Colon, "Expected ':' after 'max_rounds'")?;
                let tok = self.advance()?;
                loop_rounds = Some(match tok.token_type {
                    TokenType::Float(f) => f.max(1.0) as u64,
                    TokenType::Int(i) => i.max(1) as u64,
                    _ => return None,
                });
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
                // v0.104.6 D269：保留**全部** agent。
                //
                // 修前是 `agents.into_iter().next()` —— 只取第一个，其余
                // `agent` 行被**静默丢弃**。而 `MirOrchestrateKind::Loop.agents`
                // 本身是 `Vec`，handler 侧（`runtime.rs` 的 Loop 分支）也是
                // `for agent in agents` 逐个执行：`sequential`/`graph`/`pregel`
                // 三个兄弟 kind 都原样保留整个 vec。**只有 loop 截断**，
                // 截断只发生在解析器这一处，是孤立的漏写而非设计。
                if agents.is_empty() {
                    agents.push(MirOrchestrateAgent {
                        name: "default".to_string(),
                        // v0.104.6 D413：占位 agent 无形参。
                        params: Vec::new(),
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
                }
                MirOrchestrateKind::Loop {
                    agents,
                    // 缺省仍是 1000 —— 与 handler 侧 `rounds.unwrap_or(1000)`
                    // 一致（`runtime.rs` 的注释也这么写）。写了 `max_rounds`
                    // 就用用户给的值；不写则行为与修前完全相同。
                    rounds: loop_rounds.or(Some(1000)),
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

        let params = if self.match_token_exact(TokenType::LParen) {
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

        // v0.104.6 D413：形参真正被绑上（此前绑到 `_params` **整个丢弃**
        // ⇒ 体内引用恒为 `nil` 且 **exit 0 零诊断**）。
        //
        // **多参在解析期拒绝**：agent 只有一个输入值（`input`，见
        // `pregel/mod.rs` 的 `env.define("input", …)`），没有第二个值可绑；
        // 形参语法在 `docs/mora-spec.md` 里**零出现**、仓内**全 usages 都是单参**
        // ⇒ 拒绝的破坏面为零，且优于「让多参也静默给 nil」。
        if let Some(ps) = &params
            && ps.len() > 1
        {
            self.diag = Some(format!(
                "agent '{name}' declares {} parameters ({}), but an agent receives a \
                 single `input` value — at most 1 parameter is supported. \
                 Write `agent {name} => …` and read `input`, or keep 1 parameter \
                 and treat it as an alias of `input`.",
                ps.len(),
                ps.join(", ")
            ));
            self.current = saved;
            return None;
        }
        let params = params.unwrap_or_default();

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
            params,
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

        // v0.104.6 D271：把边条件**预 lowering** 成 `condition_body`。
        //
        // 修前 `condition_body` 恒为 `None`，而引擎（`pregel/mod.rs` 的两处
        // 条件求值）**只读 `condition_body`**、从不读 `condition_expr`
        // —— 后者全仓唯一的读者是 LSP 的 witness walk。于是
        // `edge a -> b on: <cond>` 解析成功、条件被完整保存、却**从不生效**：
        // 边永远无条件激活，exit 0、零诊断。
        //
        // 在此预 lowering，与同函数内 agent 的 `task_body`、moe expert 的
        // `def_fn` 同一风格：lowering 失败一律 `return None` 走解析错误，
        // 而不是让一个「写了但永远不执行」的条件蒙混过关。
        let condition_body = match &condition {
            Some(w) => match crate::mir::lower::lower_mir_witnesses(std::slice::from_ref(w)) {
                Ok(f) => Some(f),
                Err(_) => return None,
            },
            None => None,
        };

        Some(MirOrchestrateEdge {
            from,
            to,
            condition_expr: condition,
            condition_body,
        })
    }

    // ════════════════════════════════════════════════════════════════
    // 类型注解解析（含 union / 泛型）
    // ════════════════════════════════════════════════════════════════

    /// v0.104.6：解析泛型声明子句 `f<T, U>`（紧跟 `task` / `fn` 名字、形参之前）。
    ///
    /// 无 `<` ⇒ 非泛型：返回空 `Vec`（与本刀之前逐字等价）。
    /// 有 `<` ⇒ 解析逗号分隔的**类型参数名**（标识符，不是类型标注），存入
    /// [`ParserV3::generic_params`] 供形参/返回标注把裸名认作 `Type::TypeVar`，
    /// 并返回各参数的**首字母**列表（`Type::TypeVar` 是单 `char`）。
    ///
    /// 首字母重复时**报错**（`TT` 与 `UU` 都映射到 `'T'` 会静默合并两个类型参数）。
    /// `fresh_type_var_id` 只发控制字符（`' '`…），故用户声明的类型变量不会与之
    /// 撞名，无需另做规避。
    pub(super) fn parse_generic_clause(&mut self) -> Option<Vec<char>> {
        if !self.match_token(&[TokenType::Less]) {
            return Some(Vec::new());
        }
        let mut names: Vec<String> = Vec::new();
        loop {
            let n = self.consume_identifier("Expected type parameter name")?;
            names.push(n);
            if self.match_token(&[TokenType::Comma]) {
                continue;
            }
            break;
        }
        if !self.match_token(&[TokenType::Greater]) {
            eprintln!(
                "Parse error: expected '>' after type parameters at line {}",
                self.current_line()
            );
            return None;
        }
        // 首字母去重校验（见文档注释）。
        let mut seen: Vec<char> = Vec::new();
        let mut chars = Vec::with_capacity(names.len());
        for n in &names {
            let c = n.chars().next().expect("非空标识符必有首字符");
            if seen.contains(&c) {
                eprintln!(
                    "Parse error: generic type parameter names must start with distinct \
                 characters ('{c}' repeated) at line {}",
                    self.current_line()
                );
                return None;
            }
            seen.push(c);
            chars.push(c);
        }
        self.generic_params = names;
        Some(chars)
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
            // v0.104.6 D64：`nil` 是**关键字 token**（lexer.rs:13 `TokenType::Nil`），
            // 走不到下面的 `Identifier` 分支 —— 白名单里的 `"nil" => Type::Nil`
            // 因此是一条**不可达的死 arm**，`let v: nil = nil` 直接报
            // "expected type annotation"。spec §3.1 :115 把 `nil` 列为正式类型，
            // 且 typeck 三处（`subtype_of` mod.rs:762、`compatible_with` mod.rs:532、
            // `unify` unify.rs:307）都已成对支持 `Nil`，补上这一条即可跑通。
            TokenType::Nil => {
                self.advance();
                Some(Type::Nil)
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
                    "int" => Type::Int,
                    // v0.104.6 D62: `number` 是**数值塔**，不是 `Int` 的别名。
                    //
                    // 依据（spec §13.1 类型表 + 形式规则）：
                    //   · :110 把 `42` 与 `3.14` **同列**为 `number`
                    //   · :1154-1156 `Γ ⊢ e₁ : number Γ ⊢ e₂ : number ⊢
                    //     e₁ + e₂ : number`
                    //   · :928 `len(x) -> number`，而 `len` 运行期返 `Int`
                    // 三条合起来要求 `number` 同时容纳 Int 与 Float。
                    //
                    // 此前映射到 `Type::Int`，方向正好相反：只收 Int、拒掉
                    // Float，而 **Float 才是本语言的无后缀数值字面量类型**
                    // （`1` / `1.5` / `-1` / `1 + 2` 全是 Float，实测见
                    // tests/number_tower.rs）。于是「通用数值标注」恰好在最
                    // 常见的用法上失败，且与 :1156 自相矛盾。
                    //
                    // 选 `Union[Int, Float]` 而非新造 `Type::Number` 变体：
                    // subtype_of / compatible_with / unify 三处**早已**在
                    // Union 两侧实现了成员语义（`mod.rs:642-656`、
                    // `mod.rs:473-487`、`unify.rs:376-392`），`let` 标注的
                    // 三道关卡（infer_let_typed 的 compatible_with 即时报错
                    // + 压入的 Constraint::Eq + bidirectional 的 subtype_of）
                    // 因此全部自动放行，**且完全不触碰 promotion 塔本身**。
                    // BigInt 故意排除：v0.91 明确「BigInt 不参与 Int <: Float
                    // 提升（避免隐式精度损失）」，spec :110 也未列入 `number`。
                    "number" => Type::Union(vec![Type::Int, Type::Float]),
                    "float" => Type::Float,
                    // v0.104.6 D63：`bigint` 是 spec §3.1 :113 正式列出的类型
                    // （`999n`），`Type::BigInt` 存在、字面量产出它、`from_hint`
                    // 也认它 —— 但白名单漏了，导致 BigInt 值**永远无法被标注**
                    // （实测 `let x: bigint = 999n` 报 unsupported type annotation）。
                    // typeck 侧无需改动：`subtype_of` mod.rs:824-825、
                    // `compatible_with`、`unify` unify.rs:304 三处都已自反。
                    // 不放进 `number`：v0.91 明确 BigInt 不参与 `Int <: Float`。
                    "bigint" => Type::BigInt,
                    // v0.104.6 D72：`document` 此前是**幽灵标注** ——
                    // `Type::Document` 存在、`document.parse(path)` 运行期
                    // 产出 `Value::Document`，但白名单没有它，
                    // `let d: document = document.parse("a.md")` 报
                    // "unsupported type annotation 'document'"。
                    // 与 D63（bigint）同型：值能造出来、标编写不出。
                    // ⚠ 与 `document` **模块**同名但不是一回事 ——
                    // 模块侧的方法签名见 `module_method_signature`；
                    // `Type::Document` 是 `Value::Document` 对应的值类型。
                    "document" => Type::Document,
                    // v0.104.6 D85：`agent` 同属**幽灵标注** ——
                    // `Type::Agent` 存在、`agent.create(name, cfg)` 运行期产出
                    // `Value::Agent`（D85 刚给它补上签名），但白名单没有它，
                    // `let v: agent = agent.create("a", {})` 报
                    // "unsupported type annotation 'agent'"。与 D63（bigint）、
                    // D64（nil）、D72（document）同型。
                    "agent" => Type::Agent,
                    "string" => Type::String,
                    "char" => Type::Char,
                    "bool" => Type::Bool,
                    "nil" => Type::Nil,
                    "any" => Type::Any,
                    "unknown" => Type::Unknown,
                    other => {
                        // v0.104.6：**泛型声明的类型参数**在作用域内是类型变量
                        // （`task f<T>(x: T)`）。不加「全局大写即类型变量」规则 ——
                        // 那会让 `let x: Foo` 到处静默变成类型变量，过松。
                        // 仅当该名字是**当前泛型声明**显式声明的参数时才认。
                        if let Some(stored) = self
                            .generic_params
                            .iter()
                            .find(|p| p.to_lowercase() == other)
                            .cloned()
                        {
                            self.advance();
                            // `Type::TypeVar` 是单 `char`，取**存储的原始名**首字母
                            //（不是 lowercased 的 `other`）—— 必须与 `type_params`
                            // 里存的字符一致，否则 `ForAll` 量化不到它，两次调用会被
                            // 当同一类型统一（实测 `id(1); id("s")` 报 float/string）。
                            let c = stored.chars().next().expect("非空标识符必有首字符");
                            return Some(Type::TypeVar(c));
                        }
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

#[cfg(test)]
mod tests {
    use crate::lexer::Lexer;
    use crate::parser_v3::ParserV3;
    use crate::typeck::Type;

    fn parser(src: &str) -> ParserV3 {
        let tokens = Lexer::new(src).scan_tokens();
        ParserV3::new(tokens, src)
    }

    use crate::mir::witness::WitnessPattern as Pat;

    /// `parse_pattern` 的四种基本形态。
    ///
    /// 模式是 `match` arm 与 `let` 解构的共同基础，而 `emit_match_arm_w`
    /// 首个动作就是调它 —— 它返回 `None` 时 arm 被判为畸形。
    #[test]
    fn parse_pattern_covers_wildcard_variable_and_literals() {
        assert!(matches!(parser("_").parse_pattern(), Some(Pat::Wildcard)));
        assert!(matches!(
            parser("x").parse_pattern(),
            Some(Pat::Variable(ref n)) if n == "x"
        ));
        assert!(matches!(
            parser("42").parse_pattern(),
            Some(Pat::Literal(_))
        ));
        assert!(matches!(
            parser("\"s\"").parse_pattern(),
            Some(Pat::Literal(_))
        ));
    }

    /// rest 模式 `..name` 绑定新变量，裸 `..` 不绑定。
    ///
    /// 两者的区别直接决定 list 模式是「收集剩余元素」还是「丢弃剩余元素」。
    #[test]
    fn rest_pattern_binds_or_discards() {
        assert!(
            matches!(parser("..rest").parse_pattern(), Some(Pat::Variable(ref n)) if n == "rest"),
            "`..name` 应绑定为具名变量"
        );
        assert!(
            matches!(parser("..").parse_pattern(), Some(Pat::Wildcard)),
            "裸 `..` 不绑定，应退化为通配"
        );
    }

    /// 列表模式的元素数与 rest 绑定。
    ///
    /// `[a, b]` 无 rest、`[a, ..rest]` 捕获剩余元素 —— 这个区别直接决定
    /// 匹配时是否收集尾部，配错会静默丢元素。
    #[test]
    fn list_patterns_carry_elements_and_optional_rest() {
        match parser("[a, b]").parse_pattern() {
            Some(Pat::ListVec {
                ref elements,
                ref rest,
            }) => {
                assert_eq!(elements.len(), 2);
                assert!(rest.is_none(), "`[a, b]` 不应有 rest 绑定");
            }
            other => panic!("应为 ListVec，实得 {other:?}"),
        }
        match parser("[a, ..rest]").parse_pattern() {
            Some(Pat::ListVec {
                ref elements,
                ref rest,
            }) => {
                assert_eq!(elements.len(), 1);
                assert!(rest.is_some(), "`[a, ..rest]` 必须捕获 rest");
            }
            other => panic!("应为 ListVec，实得 {other:?}"),
        }
    }

    /// 字典模式：条目是 `key: pattern`，`{..}` 表示忽略其余字段。
    ///
    /// `rest` 标志区分「必须恰好匹配这些键」与「允许多余键」，配错会让
    /// 多余字段从静默通过变成误拒（或反之）。
    #[test]
    fn dict_patterns_distinguish_strict_from_rest() {
        match parser("{a: 1, b: x}").parse_pattern() {
            Some(Pat::Dict { ref required, rest }) => {
                assert_eq!(required.len(), 2, "两个必填键");
                assert!(!rest, "无 `..` ⇒ rest 为 false");
            }
            other => panic!("应为 Dict，实得 {other:?}"),
        }
        match parser("{a: 1, ..}").parse_pattern() {
            Some(Pat::Dict { ref required, rest }) => {
                assert_eq!(required.len(), 1);
                assert!(rest, "`{{a: 1, ..}}` 必须置 rest");
            }
            other => panic!("应为 Dict，实得 {other:?}"),
        }
    }

    /// 单一类型注解**不得**被包成 `Union(vec![T])` —— 单成员 union 会
    /// 让 `subtype_of` / `unify` 多走一条匹配分支，且打印诊断时呈现为
    /// 只有一个成员的花括号。
    #[test]
    fn single_type_annotation_is_not_wrapped_in_a_one_member_union() {
        let ty = parser("int").parse_type_annotation().expect("int 应可解析");
        assert_eq!(ty, Type::Int);
        assert!(
            !matches!(ty, Type::Union(_)),
            "单类型不得包装成 Union：{ty:?}"
        );
    }

    /// `|` 连接的多个类型必须合成真正的 `Union`。
    #[test]
    fn pipe_joined_annotations_form_a_union() {
        let ty = parser("string | number")
            .parse_type_annotation()
            .expect("联合类型应可解析");
        match &ty {
            Type::Union(members) => assert_eq!(members.len(), 2, "两个成员：{ty:?}"),
            other => panic!("应为 Union，实得 {other:?}"),
        }
    }

    /// D64：`nil` 是**关键字 token**，走不到 `Identifier` 白名单分支。
    ///
    /// 修前白名单里的 `"nil" => Type::Nil` 是**不可达的死 arm**，
    /// `let v: nil = nil` 直接报 "expected type annotation"，而 spec §3.1
    /// 明确把 `nil` 列为正式类型，typeck 三处也都已成对支持 `Nil`。
    #[test]
    fn d64_nil_keyword_parses_as_a_type() {
        assert_eq!(
            parser("nil").parse_type_annotation(),
            Some(Type::Nil),
            "`nil` 关键字必须解析为 Type::Nil"
        );
        assert!(
            ParserV3::compile("let v: nil = nil\nprint(v)\n").is_ok(),
            "spec §3.1 把 `nil` 列为正式类型，端到端必须能编译"
        );
    }

    /// `dyn Trait<...>` 泛型实参必须成对解析，缺 `>` 报错。
    #[test]
    fn dyn_trait_objects_parse_with_and_without_generics() {
        let ty = parser("dyn Show")
            .parse_type_annotation()
            .expect("dyn Show");
        assert!(matches!(ty, Type::TraitObject { .. }));

        let ty = parser("dyn Show<number>").parse_type_annotation();
        assert!(
            matches!(ty, Some(Type::TraitObject { ref generics, .. }) if generics.len() == 1),
            "泛型实参应被收进 TraitObject：{ty:?}"
        );

        assert!(
            parser("dyn Show<number").parse_type_annotation().is_none(),
            "缺 `>` 必须报错，不得当成合法注解放行"
        );
    }
}
