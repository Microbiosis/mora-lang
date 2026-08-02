//! v0.54: Parser V3 - Pure MIR expression parser (Phase γ Complete)
//!
//! **Zero AST v2 dependencies** - Direct tokens → MirExpr conversion
//! This is the final parser implementation that completely replaces Parser v2.

use crate::common::{BinaryOp, Literal, Span};
use crate::lexer::{Lexer, Token, TokenType};
use crate::mir::MirFunction;
use crate::mir::expr::*;
use std::collections::HashMap;

///  ParserV3 - Clean-room MIR parser with no AST legacy baggage
pub struct ParserV3 {
    tokens: Vec<Token>,
    current: usize,
}

impl ParserV3 {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, current: 0 }
    }

    /// Parse complete program into Vec<MirExpr>
    pub fn parse(mut self) -> Result<Vec<MirExpr>, ParseError> {
        let mut exprs = Vec::new();
        let mut guard = 0usize;

        while !self.is_at_end() {
            // Skip blank lines / multiple newlines
            while self.match_token(&[TokenType::Newline]) {}

            if self.is_at_end() {
                break;
            }

            // Guard against regressions where a successful parse does not
            // advance the token cursor.
            guard += 1;
            if guard > 10_000 {
                return Err(ParseError(
                    "parser_v3: aborted after 10k iterations".to_string(),
                ));
            }

            // Parse expression statement
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

    // ===================================================================
    // Top-level entry point
    // ===================================================================

    fn parse_expression_statement(&mut self) -> Option<MirExpr> {
        let start_span = self.span_of_current();

        // Try task declaration first: task name(params) { body }
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
            // Consume the trailing `end` keyword when the body was parsed by
            // parse_block_body (which stops at End/RBrace/EOF).
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

        // v0.75.11: `let` 即必须成功（helper 已消费 let；中途失败不能
        // fallback 到其它语句，否则 token 错位会静默返回 Ok）。
        if self.check(&TokenType::Let) {
            return self.parse_let_binding();
        }

        // Handle simple expressions as statements
        // Priority order: Match > IfElse > Assignment

        // Try match first (highest precedence at statement level)
        if let Some(expr) = self.parse_match_expression() {
            return Some(expr);
        }

        // Then try if/else
        if let Some(expr) = self.parse_if_expression() {
            return Some(expr);
        }

        // Try for loop next (top-level control flow construct)
        if let Some(expr) = self.parse_for_loop() {
            return Some(expr);
        }

        // Try while loop next
        if let Some(expr) = self.parse_while_loop() {
            return Some(expr);
        }

        // Try return/break/continue (control flow statements)
        if let Some(expr) = self.parse_return_break_continue() {
            return Some(expr);
        }

        // Try type/enum/struct/import/macro definitions
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

        // Try orchestrate next (before assignment, it's a top-level construct)
        if let Some(expr) = self.parse_orchestrate_statement() {
            return Some(expr);
        }

        // Finally try assignment and other constructs
        let expr = self.parse_assignment()?;

        // Consume an optional line terminator. The top-level loop handles
        // additional blank lines.
        let _ = self.match_token(&[TokenType::Newline]);

        Some(expr)
    }

    /// Parse match expressions (pattern matching).
    /// Syntax: `match expr { arm1 => body1, arm2 => body2, ... }`
    /// This is a v0.54 MVP implementation with basic support for:
    /// - Variable patterns (identifiers)
    /// - Literal patterns (true, false, numbers, strings, nil)
    /// - Guard conditions on arms are reserved for v0.55+
    ///
    /// Future enhancements (v0.55+):
    /// - Tuple/list/dict destructuring patterns
    /// - Guard clauses after =>
    /// - Pattern matching on traits and enums
    fn parse_match_expression(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Match) {
            return None;
        }

        let expr_span = self.span_of_current();

        // Parse match subject expression
        let subject = self.parse_expression()?;

        // Expect opening brace for arms
        if !self.match_token_exact(TokenType::LBrace) {
            return None;
        }

        // Parse all match arms
        let mut arms = Vec::new();
        let mut arm_guard = 0usize;
        loop {
            if self.match_token_exact(TokenType::RBrace) || self.is_at_end() {
                break;
            }

            if let Some(arm) = self.parse_match_arm() {
                arms.push(arm);
                // Tolerate trailing comma between arms; required between
                // most arms so we always advance after a successful parse.
                let _ = self.match_token(&[TokenType::Comma]);
            } else {
                // Recover by skipping one token so the loop makes progress
                // even when a malformed arm is encountered. This is the
                // minimum safety net required to avoid an infinite loop
                // when parser_v3 cannot yet represent a particular arm.
                self.advance();
            }

            arm_guard += 1;
            if arm_guard > 10_000 {
                eprintln!("parser_v3: match arms aborted after 10k iterations");
                break;
            }
        }

        // Construct the Match variant with parsed arms
        Some(MirExpr {
            kind: MirExprKind::Match {
                scrutinee: Box::new(subject),
                arms,
            },
            span: expr_span,
        })
    }

    /// Parse a single match arm.
    /// Syntax: `pattern => expression`
    /// This creates a proper MatchArm node with pattern, guard, and body.
    fn parse_match_arm(&mut self) -> Option<crate::mir::expr::MatchArm> {
        // Parse pattern on left side of =>
        let pattern = self.parse_pattern()?;

        // Must have => fat arrow (v0.55: now a proper token)
        if !self.match_token_exact(TokenType::FatArrow) {
            return None;
        }

        // Parse body expression on right side of =>
        let body = self.parse_assignment()?;

        // Construct proper MatchArm with pattern and body
        Some(crate::mir::expr::MatchArm {
            pattern,
            guard: None,
            body,
        })
    }

    /// Parse pattern for destructuring.
    /// Returns a Pattern enum variant (not MirExpr).
    /// Supported patterns in v0.54:
    /// - Variable names: `x` → Pattern::Variable("x".to_string())
    /// - Literal values: `true`, `false`, `123`, `"string"`, `nil` → Pattern::Literal(_)
    /// - Wildcard: `_` → Pattern::Wildcard
    /// - Reserved for v0.55+:
    ///   - Tuple patterns: `(a, b)` → Pattern::Tuple(...)
    ///   - List patterns: `[head, ..tail]` → Pattern::List { ... }
    ///   - Dict patterns: `{key: value}` → Pattern::Dict { ... }
    ///   - Guard clauses: `pattern if condition` → Pattern::Guard { ... }
    ///   - Or patterns: `A | B` → Need new Pattern variant
    fn parse_pattern(&mut self) -> Option<crate::mir::expr::Pattern> {
        // Check for wildcard pattern `_` first (before generic identifier check)
        if let TokenType::Identifier(ref name) = self.peek()?.token_type
            && name == "_"
        {
            self.advance();
            return Some(crate::mir::expr::Pattern::Wildcard);
        }

        // Check if current token is an identifier (variable name)
        let is_identifier = matches!(self.peek()?.token_type, TokenType::Identifier(_));

        if is_identifier {
            let name = self.consume_identifier("Expected pattern")?;
            return Some(crate::mir::expr::Pattern::Variable(name));
        }

        // Support literal patterns by converting them to Pattern::Literal
        if let Some(literal) = self.try_parse_literal_pattern() {
            return Some(crate::mir::expr::Pattern::Literal(literal));
        }

        // Unsupported pattern type
        None
    }

    /// Try to parse a literal value as a pattern.
    /// Returns the Literal directly without constructing MirExpr.
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

    /// Parse orchestrate statements (v0.x top-level multi-agent construct).
    /// Supported syntax:
    ///  orchestrate sequential x -> y
    ///  agent a(x) => "a:" + x
    ///  agent b(x) => "b:" + x
    ///  orchestrate loop x -> y, max_rounds: 5
    ///  on: x == "done"
    ///  agent a(x) => x
    ///  orchestrate graph x -> y
    ///  @start -> a
    ///  @start -> b on: x == "research"
    ///  a -> @exit
    ///  b -> @exit
    fn parse_orchestrate_statement(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Orchestrate) {
            return None;
        }

        let start_span = self.span_of_current();

        // Parse kind: sequential | loop | graph | pregel
        let kind_str = if self.check(&TokenType::Loop) {
            self.advance();
            "loop".to_string()
        } else {
            let name = self
                .consume_identifier("Expected orchestrate kind (sequential/loop/graph/pregel)")?;
            if name != "sequential" && name != "graph" && name != "pregel" {
                eprintln!(
                    "Parse error: Expected orchestrate kind (sequential/loop/graph/pregel) at line {}",
                    self.current_line()
                );
                return None;
            }
            name
        };

        // Parse input_var -> result_var
        let input_var = self.consume_identifier("Expected input variable")?;
        self.consume(TokenType::Arrow, "Expected '->' after input variable")?;
        let result_var = self.consume_identifier("Expected result variable")?;

        // Skip optional comma
        let _ = self.match_token(&[TokenType::Comma]);

        // Parse body: agents, edges, on: predicate
        let mut agents: Vec<MirOrchestrateAgent> = Vec::new();
        let mut edges: Vec<MirOrchestrateEdge> = Vec::new();
        let mut exit_when: Option<MirExpr> = None;

        loop {
            // Skip blank lines
            while self.match_token(&[TokenType::Newline]) {}

            if self.is_at_end() {
                break;
            }

            // Check for 'end' keyword (explicit block terminator)
            if self.check(&TokenType::End) {
                self.advance();
                break;
            }

            // Skip max_rounds: N (loop header property)
            if self.peek_is_identifier("max_rounds") || self.check(&TokenType::MaxRounds) {
                if self.check(&TokenType::MaxRounds) {
                    self.advance();
                } else {
                    self.advance(); // consume identifier
                }
                self.consume(TokenType::Colon, "Expected ':' after 'max_rounds'")?;
                // Skip value (number or expression) until newline/end
                while !self.check(&TokenType::Newline) && !self.is_at_end() {
                    self.advance();
                }
                continue;
            }

            // Try 'on:' predicate (loop)
            if self.peek_is_identifier("on") {
                self.advance(); // consume 'on'
                self.consume(TokenType::Colon, "Expected ':' after 'on'")?;
                if let Some(cond) = self.parse_assignment() {
                    exit_when = Some(cond);
                }
                continue;
            }

            // Try agent definition: agent name(params) => expr
            if self.peek_is_identifier("agent")
                && let Some(agent) = self.parse_agent_def()
            {
                agents.push(agent);
                continue;
            }

            // Try edge definition: @start -> a, a -> @exit, @start -> b on: cond
            if let Some(edge) = self.try_parse_edge_def() {
                edges.push(edge);
                continue;
            }

            // Nothing recognizable — stop body parsing
            break;
        }

        // Build orchestrate kind
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
                        },
                        task_mir_expr: None,
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

    /// Peek at current token and check if it matches the given name.
    /// Works for both regular Identifier tokens and reserved-word tokens.
    fn peek_is_identifier(&self, name: &str) -> bool {
        self.peek()
            .map(|t| {
                matches!(&t.token_type, TokenType::Identifier(s) if s == name)
                    || token_to_identifier_name(&t.token_type) == Some(name)
            })
            .unwrap_or(false)
    }

    /// Parse an agent definition: `agent name(params) => expr`
    fn parse_agent_def(&mut self) -> Option<MirOrchestrateAgent> {
        let saved = self.current;

        // consume 'agent'
        self.advance();
        let name = match self.consume_identifier("Expected agent name") {
            Some(n) => n,
            None => {
                self.current = saved;
                return None;
            }
        };

        // Optional params: (x, y)
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

        // => body expression
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

        let task_mir = Some(body.clone());
        // v0.75.32: 修复 task_expr → task_body 降级缺失 — 此前 task_body 恒空
        // （pregel 执行报 "lowering missing"）。产出时立即 lower 填入；
        // 失败兜底为空（保持旧行为，pregel 端仍会报真降级错误）。
        let lowered_body = crate::mir::lower::lower_mir_exprs(std::slice::from_ref(&body))
            .unwrap_or_else(|_| MirFunction {
                params: vec![],
                body: vec![],
                n_regs: 0,
            });
        Some(MirOrchestrateAgent {
            name,
            with_config: None,
            task_expr: body,
            verify_expr: None,
            task_body: lowered_body,
            task_mir_expr: task_mir,
            combiner_body: None,
        })
    }

    /// Try to parse an edge definition: `@start -> a`, `a -> @exit`, `@start -> b on: cond`
    fn try_parse_edge_def(&mut self) -> Option<MirOrchestrateEdge> {
        let saved = self.current;

        // Parse from node
        let from = if self.check(&TokenType::At) {
            self.advance();
            let node_name = self.consume_identifier("Expected node name after @")?;
            format!("@{}", node_name)
        } else {
            // Clone the identifier string before advancing to avoid borrow conflict
            let name = match self.peek()?.token_type {
                TokenType::Identifier(ref s) => s.clone(),
                _ => return None,
            };
            self.advance();
            name
        };

        // Expect ->
        if !self.match_token_exact(TokenType::Arrow) {
            self.current = saved;
            return None;
        }

        // Parse to node
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

        // Optional on: condition
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

    /// v0.75.11: `let name[: type] = value` 绑定（复用型 helper）。
    ///
    /// 此前块体内（task/if/for 的 parse_block_body）只试 parse_assignment +
    /// if/for/while — `let` 关键字不匹配任何分支，被 advance() 跳过 → 余下的
    /// `n = 5` 被解析成 `Assign` → `env.assign` 对未定义变量静默返回 false
    /// （n 变 Nil），导致 task 内 let 变量后续比较/builtin 全错。顶层路径
    /// （parse_expression_statement）有 let 分支；抽出本 helper 让块体同样
    /// 正确生成 `MirExprKind::LetBinding`（lower 发 `MirInst::Define`）。
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

    /// Parse a block body: multiple newline-separated expressions until RBrace/End.
    /// Returns a Sequence if multiple expressions, or the single expression if just one.
    fn parse_block_body(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        let mut exprs = Vec::new();

        loop {
            // Skip leading newlines
            while self.match_token(&[TokenType::Newline]) {}

            // Check for end of block
            if self.is_at_end() || self.check(&TokenType::RBrace) || self.check(&TokenType::End) {
                break;
            }

            // v0.75.11: `let` 优先且必须成功（helper 已消费 let，失败不
            // fallback — 避免未知注解等错误被错位解析成 Assign）。
            let stmt = if self.check(&TokenType::Let) {
                self.parse_let_binding()
            } else {
                // Try assign; if not, try one of the statement-level constructs
                // (if/for/while/match/return).
                self.parse_assignment().or_else(|| {
                    // Identify a leading construct keyword and dispatch
                    let tok = self.peek()?.token_type.clone();

                    match tok {
                        TokenType::If => self.parse_if_expression(),
                        TokenType::For => self.parse_for_loop(),
                        _ => {
                            // 'while' is identifier-based (no TokenType::While), but
                            // try the loop parser if the next token is "while"
                            if let TokenType::Identifier(ref n) = tok
                                && n == "while"
                            {
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
                // Can't parse — skip token to make progress
                self.advance();
            }

            // Consume trailing newline or comma
            let _ = self.match_token(&[TokenType::Newline, TokenType::Comma]);
        }

        if exprs.is_empty() {
            return None;
        }
        if exprs.len() == 1 {
            return Some(exprs.into_iter().next().expect("len==1 verified above"));
        }
        Some(MirExpr {
            kind: MirExprKind::Sequence(exprs),
            span,
        })
    }

    /// Parse if/else expressions.
    /// Supports three syntax styles:
    /// 1. Expression style: `if cond then expr else else_expr`
    /// 2. Block style: `if cond { then_expr } else { else_expr }`
    ///    The `then` keyword is optional when using block syntax `{`.
    ///    The `else` branch is optional in both styles.
    fn parse_if_expression(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::If) {
            return None;
        }

        let expr_span = self.span_of_current();

        // Parse condition expression
        let cond = self.parse_expression()?;

        // Three syntax styles:
        // 1. `if cond then expr else expr`  (then keyword, expression syntax)
        // 2. `if cond { expr } else { expr }`  (brace block syntax)
        // 3. `if cond then expr else expr end`  (then keyword with end terminator)

        if self.match_token_exact(TokenType::Then) {
            // Expression syntax: if cond then expr [else expr] [end]
            let then_branch = self.parse_assignment()?;

            let else_branch = if self
                .peek()
                .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "else"))
                .unwrap_or(false)
            {
                self.advance(); // consume 'else'
                Some(self.parse_assignment()?)
            } else {
                None
            };

            // Optional 'end' terminator (skip any intermediate newlines)
            while self.match_token(&[TokenType::Newline]) {}
            let _ = self.match_token(&[TokenType::End]);

            return Some(MirExpr::if_else(cond, then_branch, else_branch, expr_span));
        }

        if self.match_token_exact(TokenType::LBrace) {
            // Brace block syntax: if cond { then } [else { else }]
            let then_branch = self.parse_block_body()?;
            self.consume(TokenType::RBrace, "Expected closing brace '}}'")?;

            let else_branch = if self
                .peek()
                .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "else"))
                .unwrap_or(false)
            {
                self.advance(); // consume 'else'
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

    /// Parse for loop expressions.
    /// Syntax: `for var in iterable { body }`
    fn parse_for_loop(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::For) {
            return None;
        }

        let expr_span = self.span_of_current();

        // Parse variable name
        let var = self.consume_identifier("Expected variable name after 'for'")?;

        // Expect 'in' keyword
        if !self.match_token_exact(TokenType::In) {
            eprintln!(
                "Parse error: Expected 'in' after 'for' variable at line {}",
                self.current_line()
            );
            return None;
        }

        // Parse iterable expression
        let iterable = self.parse_assignment()?;

        // Accept both brace block { ... } and end-terminated block:
        //   for i in items { body }
        //   for i in items\n body end
        let body = if self.match_token_exact(TokenType::LBrace) {
            let b = self.parse_block_body()?;
            self.consume(TokenType::RBrace, "Expected '}' after for loop body")?;
            b
        } else {
            // end-terminated block syntax
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

    /// Parse while loop expressions.
    /// Syntax: `while cond { body }`
    fn parse_while_loop(&mut self) -> Option<MirExpr> {
        // Check for Identifier("while") since 'while' is not a keyword token
        if !self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "while"))
            .unwrap_or(false)
        {
            return None;
        }
        self.advance(); // consume 'while'

        let expr_span = self.span_of_current();

        // Parse condition expression
        let cond = self.parse_assignment()?;

        // Accept both brace block { ... } and end-terminated block:
        //   while cond { body }
        //   while cond\n body end
        let body = if self.match_token_exact(TokenType::LBrace) {
            let b = self.parse_block_body()?;
            self.consume(TokenType::RBrace, "Expected '}' after while loop body")?;
            b
        } else {
            // end-terminated block syntax
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

    /// Parse return/break/continue statements.
    /// Syntax:
    /// - `return [value]`
    /// - `break [label]`
    /// - `continue [label]`
    fn parse_return_break_continue(&mut self) -> Option<MirExpr> {
        let token = self.peek()?.token_type.clone();
        let span = self.span_of_current();

        match token {
            TokenType::Return => {
                self.advance();
                // Optional return value
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
                // Optional loop label
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
                // Optional loop label
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

    // ===================================================================
    // Expression Parsing Hierarchy (by precedence)
    // ===================================================================

    fn parse_assignment(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();

        // Check for assignment pattern: identifier = value
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

            // Not an assignment, rewind
            self.current = ident_start;
        }

        self.parse_or()
    }

    fn parse_or(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        let mut left = self.parse_and()?;

        // Check for 'or' keyword (identifier "or")
        while self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "or"))
            .unwrap_or(false)
        {
            self.advance(); // consume 'or'
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

        // Check for 'and' keyword (identifier "and")
        while self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "and"))
            .unwrap_or(false)
        {
            self.advance(); // consume 'and'
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
        // v0.75.21: pipe 优先级低于 equality — `a == b |> f` = `a == f(b)`
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

        // Check for 'not' keyword (identifier "not")
        if self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "not"))
            .unwrap_or(false)
        {
            self.advance(); // consume 'not'
            let operand = self.parse_unary()?;
            // not x = 0 == x → false if x is truthy
            let zero = MirExpr::lit(Literal::Int(0, span), span);
            let negated = MirExpr::binop(BinaryOp::Equal, zero, operand, span);
            return Some(negated);
        }

        // Check for unary minus or bang (!)
        if self.check(&TokenType::Minus) || self.check(&TokenType::Bang) {
            let _op = self.advance()?;
            let operand = self.parse_unary()?;

            // For now, convert to subtraction from 0
            let zero = MirExpr::lit(
                Literal::Int(0, self.span_of_current()),
                self.span_of_current(),
            );
            let negated = MirExpr::binop(BinaryOp::Sub, zero, operand, self.span_of_current());
            return Some(negated);
        }

        let expr = self.parse_call()?;

        // Check for 'as dyn Trait' suffix
        if self.check(&TokenType::As) {
            self.advance(); // consume 'as'
            if self.check(&TokenType::Dyn) {
                self.advance(); // consume 'dyn'
                let trait_name = self.consume_identifier("Expected trait name after 'dyn'")?;
                // Optional generics: <Type, ...>
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

        // Handle function calls and method calls
        loop {
            // Function call: func(args...)
            if self.match_token_exact(TokenType::LParen) {
                let args = self.parse_argument_list().ok()?;
                callee = MirExpr::call(
                    MirCallee::Name(match_to_string(&callee).to_string()),
                    args,
                    self.span_of_current(),
                );
            }
            // Method call: object.method(args)
            else if self.match_token_exact(TokenType::Dot) {
                let method_name = self.consume_identifier("Expected method name")?;
                let mut args = Vec::new();

                if self.match_token_exact(TokenType::LParen) {
                    args = self.parse_argument_list().ok()?;
                }

                // v0.75.16: 产出 MirCallee::Method（此前降成 Name("obj_method")
                // 让 typeck 无法走 method_signature 推断；lower 层仍拼
                // "obj_method" 字符串，runtime 分发不变）。
                let old_callee = callee.clone();
                callee = MirExpr::call(
                    MirCallee::Method(match_to_string(&old_callee), method_name),
                    std::iter::once(old_callee).chain(args).collect(),
                    self.span_of_current(),
                );
            }
            // v0.58: static dispatch / namespace qualification — `Type::method(args)`
            else if self.match_token_exact(TokenType::ColonColon) {
                let method_name = self.consume_identifier("Expected method name after '::'")?;
                // Form qualified name "McpServer::new" — the interpreter dispatches
                // on the full qualified name as a single callable.
                let old_name = match_to_string(&callee);
                let qualified = format!("{}::{}", old_name, method_name);
                callee = MirExpr::var(qualified, self.span_of_current());
                // Continue loop: next token `(` will be handled as a function call.
            }
            // Indexing: array[index] or dict[key]
            else if self.check(&TokenType::LBracket) {
                self.advance(); // consume '['
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

    /// Parse pipe expressions: `expr |> func` or `expr |> func(args)`
    /// Syntax: `lhs |> rhs`
    /// The rhs is typically a function call. This is left-associative:
    /// v0.75.21: `|>` 管道语法接入 — 挂在 `parse_equality` 之下、
    /// `parse_comparison` 之上（优先级低于 equality：`a == b |> f` =
    /// `a == f(b)`）。操作数是 comparison 级（`1 + 2 |> f` = `f(1 + 2)`，
    /// 算术先于管道）。`a |> b |> c` 链式 → `c(b(a))`。
    fn parse_pipe(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_comparison()?;

        while self.match_token_exact(TokenType::Pipe) {
            let right = self.parse_comparison()?;
            // Pipe: left |> right becomes right(left)
            // Build as a call where left is the first argument to right
            let right_span = right.span;
            // v0.75.21: 修复 Call 分支的 callee 名丢失 — 此前 `right_name =
            // match_to_string(&right)` 对 Call 变体返回 "expr"，`x |> f(a)`
            // 会产出 `Call(Name("expr"), [x, a])`。改为保留分支内的真名。
            let (callee, args) = match right.kind {
                MirExprKind::Call {
                    callee: MirCallee::Name(name),
                    mut args,
                } => {
                    // If right is already a call, prepend left as first arg
                    args.insert(0, left);
                    (MirCallee::Name(name), args)
                }
                _ => {
                    // If right is not a call, treat it as a function with left as arg
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
        // Clone the current token to release the immutable borrow on
        // `self.tokens`. We need to call `self.advance()` inside some
        // branches, which requires `&mut self`.
        let token = self.peek().cloned()?;
        let span = crate::common::Span::new(token.line, token.column);

        match token.token_type {
            // Literals
            TokenType::Int(val) => {
                self.advance();
                Some(MirExpr::lit(Literal::Int(val, span), span))
            }
            TokenType::Float(val) => {
                self.advance();
                Some(MirExpr::lit(Literal::Float(val, span), span))
            }
            TokenType::String(_) => {
                // parse_string_literal advances internally
                self.parse_string_literal()
            }
            TokenType::PromptString(_) => {
                // p"..." template: same handler as String
                self.parse_string_literal()
            }
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

            // Identifiers and variables
            TokenType::Identifier(name) => {
                self.advance();
                Some(MirExpr::var(name, span))
            }

            // Collections
            TokenType::LBracket => self.parse_list(),
            TokenType::LBrace => self.parse_dict(),

            // Grouping
            TokenType::LParen => {
                self.advance(); // consume '('
                let inner = self.parse_expression()?;
                let paren_parsed = self.consume(TokenType::RParen, "Expected ')'");
                paren_parsed?;
                Some(mir_group(inner))
            }

            // Keywords and other constructs
            TokenType::Fn => {
                self.advance();
                // Closure: fn(params) => body       (expression body)
                //          fn(params) body end      (block body)
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

                // v0.58: support both `=> expr` (FatArrow) and block body
                if self.match_token_exact(TokenType::FatArrow) {
                    // Expression body: fn(params) => body
                    let body = self.parse_assignment()?;
                    Some(MirExpr::closure(params, body, span))
                } else {
                    // Block body: fn(params) body end
                    let body = self.parse_block_body()?;
                    self.consume(TokenType::End, "Expected 'end' after closure body")?;
                    Some(MirExpr::closure(params, body, span))
                }
            }
            _ => None,
        }
    }

    // Alias for parse_assignment to satisfy grammar calls
    fn parse_expression(&mut self) -> Option<MirExpr> {
        self.parse_assignment()
    }

    fn parse_string_literal(&mut self) -> Option<MirExpr> {
        let span = self.span_of_current();
        let tok = self.advance()?;

        match tok.token_type {
            TokenType::String(ref s) => Some(MirExpr::lit(Literal::String(s.clone(), span), span)),
            TokenType::PromptString(ref s) => {
                // Parse prompt string: p"text {expr} more {expr}"
                // Split into parts: literal strings and interpolated expressions
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

    fn parse_type_annotation(&mut self) -> Option<crate::typeck::Type> {
        use crate::typeck::Type;
        let tok = self.peek().cloned()?;
        match &tok.token_type {
            TokenType::Identifier(name) => {
                let lower = name.to_lowercase();
                // v0.75.17: 泛型注解 `<...>` — `List<int>` / `dict<string, any>`
                // 递归解析参数后构造带元素/键值类型的类型（此前只接受单标识符，
                // `List<int>` 报 "unsupported type annotation"）。
                // 双 token lookahead：identifier 后紧跟 '<'（如 `List<int>`）。
                if matches!(
                    self.tokens.get(self.current + 1).map(|t| &t.token_type),
                    Some(TokenType::Less)
                ) {
                    // 消费 identifier 与 '<'（advance 越过 identifier 后
                    // current 停在 '<'，需再 match 掉 '<' 才到首个参数）。
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
                            args.into_iter().next().unwrap_or(Type::Any),
                        ))),
                        "dict" => {
                            let mut it = args.into_iter();
                            let k = it.next().unwrap_or(Type::Any);
                            let v = it.next().unwrap_or(Type::Any);
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

    /// Parse type alias: `type Name = TargetType`
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

    /// Parse enum definition: `enum Name\n  Variant1\n  Variant2\nend`
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

    /// Parse struct definition: `struct Name\n  field1: Type\n  field2: Type\nend`
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

    /// Parse import statement: `import "path/to/module"`
    fn parse_import_statement(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Import) {
            return None;
        }
        let span = self.span_of_current();
        // Expect a string literal
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

    /// Parse macro definition: `macro name(param1, param2)\n  body\nend`
    fn parse_macro_def(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::Macro) {
            return None;
        }
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected macro name after 'macro'")?;

        // Optional params
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

        // Skip body until 'end'
        loop {
            while self.match_token(&[TokenType::Newline]) {}
            if self.is_at_end() || self.check(&TokenType::End) {
                if self.check(&TokenType::End) {
                    self.advance();
                }
                break;
            }
            self.advance(); // skip body tokens
        }

        Some(MirExpr {
            kind: MirExprKind::MacroDef { name, params },
            span,
        })
    }

    // ===================================================================
    // Token Utilities
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

    fn previous(&self) -> Option<&Token> {
        if self.current > 0 {
            self.tokens.get(self.current - 1)
        } else {
            None
        }
    }

    fn is_at_end(&self) -> bool {
        if self.current >= self.tokens.len() {
            return true;
        }
        match self.tokens.get(self.current) {
            Some(t) => t.token_type == TokenType::EOF,
            None => true,
        }
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

    fn match_token_exact(&mut self, token_type: TokenType) -> bool {
        if self.check(&token_type) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn consume(&mut self, token_type: TokenType, message: &str) -> Option<()> {
        if self.check(&token_type) {
            self.advance();
            Some(())
        } else {
            eprintln!("Parse error: {} at line {}", message, self.current_line());
            None
        }
    }

    fn consume_identifier(&mut self, message: &str) -> Option<String> {
        match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                Some(name)
            }
            // v0.58: accept reserved-word tokens that are valid in identifier
            // positions (method names, variable names after `.` or `::`).
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

    fn current_line(&self) -> u32 {
        self.peek()
            .map(|t| t.line)
            .unwrap_or(0)
            .try_into()
            .unwrap_or(0)
    }

    fn span_of_current(&self) -> Span {
        self.peek()
            .map(|t| Span {
                line: t.line,
                column: t.column,
            })
            .unwrap_or(Span { line: 0, column: 0 })
    }

    fn consume_binary_op(&mut self, accepted: &[TokenType]) -> Option<BinaryOp> {
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

#[derive(Debug)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

/// v0.58: map reserved-word tokens back to their identifier strings.
/// This is needed because the lexer tokenizes `tool`, `task`, etc. as
/// dedicated token types, but they can appear in identifier positions
/// (method names, variable references after `.` or `::`).
///
/// v0.75.19: 与 lexer 关键字表同步收敛 — 移除已删除 token 的 arm
/// （词面是语法集，MirInst 原语由手工构造驱动，运行时原语集不变）。
fn token_to_identifier_name(tt: &TokenType) -> Option<&'static str> {
    match tt {
        TokenType::Task => Some("task"),
        TokenType::Fn => Some("fn"),
        TokenType::Let => Some("let"),
        TokenType::If => Some("if"),
        TokenType::Then => Some("then"),
        TokenType::Match => Some("match"),
        TokenType::Return => Some("return"),
        TokenType::For => Some("for"),
        TokenType::Break => Some("break"),
        TokenType::Continue => Some("continue"),
        TokenType::End => Some("end"),
        TokenType::In => Some("in"),
        TokenType::Import => Some("import"),
        TokenType::Type => Some("type"),
        TokenType::Enum => Some("enum"),
        TokenType::Struct => Some("struct"),
        TokenType::Macro => Some("macro"),
        TokenType::Loop => Some("loop"),
        TokenType::Orchestrate => Some("orchestrate"),
        TokenType::Prompt => Some("prompt"),
        TokenType::Document => Some("document"),
        TokenType::Dyn => Some("dyn"),
        TokenType::As => Some("as"),
        TokenType::Do => Some("do"),
        TokenType::MaxRounds => Some("max_rounds"),
        _ => None,
    }
}

// Helper function to convert MirExpr to string (for method name generation)
fn match_to_string(expr: &MirExpr) -> String {
    match &expr.kind {
        MirExprKind::Variable(n) => n.clone(),
        MirExprKind::Literal(lit) => format!("{:?}", lit),
        _ => "expr".to_string(),
    }
}

/// Parse prompt string content into MirExpr parts (standalone, no &self borrow).
/// `p"hello {name}"` → [Literal("hello "), Variable("name")]
fn parse_prompt_parts(content: &str, span: Span) -> Vec<MirExpr> {
    let mut parts = Vec::new();
    let mut current_text = String::new();
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '{' {
            // Flush accumulated text as a literal part
            if !current_text.is_empty() {
                parts.push(MirExpr::lit(
                    Literal::String(current_text.clone(), span),
                    span,
                ));
                current_text.clear();
            }
            // Collect expression text until matching '}'
            let mut expr_text = String::new();
            let mut depth = 1;
            while let Some(&ec) = chars.peek() {
                chars.next();
                if ec == '{' {
                    depth += 1;
                    expr_text.push(ec);
                } else if ec == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    expr_text.push(ec);
                } else {
                    expr_text.push(ec);
                }
            }
            // Parse the expression text via sub-lexer+parser
            if !expr_text.is_empty() {
                let mut lexer = Lexer::new(&expr_text);
                let tokens = lexer.scan_tokens();
                let mut parser = ParserV3::new(tokens);
                if let Some(expr) = parser.parse_assignment() {
                    parts.push(expr);
                }
            }
        } else {
            current_text.push(c);
        }
    }

    // Flush remaining text
    if !current_text.is_empty() || parts.is_empty() {
        parts.push(MirExpr::lit(Literal::String(current_text, span), span));
    }

    parts
}

// Add grouping helper - build grouped expression as identity
fn mir_group(inner: MirExpr) -> MirExpr {
    inner // Currently just return as-is
}
