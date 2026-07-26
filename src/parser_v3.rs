//! v0.54: Parser V3 - Pure MIR expression parser (Phase γ Complete)
//!
//! **Zero AST v2 dependencies** - Direct tokens → MirExpr conversion
//! This is the final parser implementation that completely replaces Parser v2.

use crate::common::{BinaryOp, Literal, Span};
use crate::lexer::{Token, TokenType};
use crate::mir::expr::*;

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
            while !self.check(&TokenType::RParen) && !self.is_at_end() {
                self.advance();
            }
            self.consume(TokenType::RParen, "Expected ')' after task params")?;
            let _ = self.match_token(&[TokenType::Newline]);
            let body = if let Some(expr) = self.parse_orchestrate_statement() {
                expr
            } else if let Some(expr) = self.parse_assignment() {
                expr
            } else {
                return None;
            };
            return Some(MirExpr {
                kind: MirExprKind::FnDef {
                    name,
                    params: Vec::new(),
                    return_type: None,
                    body: Box::new(body),
                },
                span: start_span,
                ty: None,
            });
        }

        if self.match_token_exact(TokenType::Let) {
            let name = self.consume_identifier("Expected variable name after 'let'")?;
            let type_hint = if self.match_token_exact(TokenType::Colon) {
                Some(self.parse_type_annotation()?)
            } else {
                None
            };
            self.consume(TokenType::Assign, "Expected '=' after variable name")?;
            let value = self.parse_assignment()?;
            let nil = MirExpr::lit(Literal::Nil(start_span), start_span);
            let _ = self.match_token(&[TokenType::Newline]);
            return Some(MirExpr {
                kind: MirExprKind::LetBinding {
                    name,
                    type_hint,
                    value: Box::new(value),
                    init_body: Box::new(nil),
                },
                span: start_span,
                ty: None,
            });
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
            ty: None,
        })
    }

    /// Parse a single match arm.
    /// Syntax: `pattern => expression`
    /// This creates a proper MatchArm node with pattern, guard, and body.
    fn parse_match_arm(&mut self) -> Option<crate::mir::expr::MatchArm> {
        // Parse pattern on left side of =>
        let pattern = self.parse_pattern()?;

        // Must have => arrow
        if !self.match_token_exact(TokenType::Arrow) {
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
    /// Reserved for v0.55+:
    /// - Tuple patterns: `(a, b)` → Pattern::Tuple(...)
    /// - List patterns: `[head, ..tail]` → Pattern::List { ... }
    /// - Dict patterns: `{key: value}` → Pattern::Dict { ... }
    /// - Guard clauses: `pattern if condition` → Pattern::Guard { ... }
    /// - Or patterns: `A | B` → Need new Pattern variant
    fn parse_pattern(&mut self) -> Option<crate::mir::expr::Pattern> {
        // Check if current token is an identifier (variable name)
        let is_identifier = matches!(self.peek()?.token_type, TokenType::Identifier(_));

        if is_identifier {
            let name = self.consume_identifier("Expected pattern")?;
            return Some(crate::mir::expr::Pattern::Variable(name));
        }

        // Support wildcard pattern _
        if self.match_token_exact(TokenType::Wildcard) {
            return Some(crate::mir::expr::Pattern::Wildcard);
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

        // Parse kind: sequential | loop | graph
        let kind_str = if self.check(&TokenType::Loop) {
            self.advance();
            "loop".to_string()
        } else {
            let name = self.consume_identifier("Expected orchestrate kind (sequential/loop/graph)")?;
            if name != "sequential" && name != "graph" {
                eprintln!(
                    "Parse error: Expected orchestrate kind (sequential/loop/graph) at line {}",
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
            if self.peek_is_identifier("agent") {
                if let Some(agent) = self.parse_agent_def() {
                    agents.push(agent);
                    continue;
                }
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
                let agent = agents.into_iter().next().unwrap_or_else(|| {
                    MirOrchestrateAgent {
                        name: "default".to_string(),
                        with_config: None,
                    task_expr: MirExpr::lit(
                        crate::common::Literal::Nil(start_span),
                        start_span,
                    ),
                        verify_expr: None,
                    }
                });
                MirOrchestrateKind::Loop {
                    agents: vec![agent],
                    rounds: Some(1000),
                    exit_when,
                }
            }
            "graph" => MirOrchestrateKind::Graph { agents, edges },
            _ => MirOrchestrateKind::Sequential { agents },
        };

        Some(MirExpr {
            kind: MirExprKind::Orchestrate {
                input_var,
                result_var,
                kind: Box::new(kind),
            },
            span: start_span,
            ty: None,
        })
    }

    /// Peek at current token and check if it's an Identifier with the given name.
    fn peek_is_identifier(&self, name: &str) -> bool {
        self.peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == name))
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
            if self.consume(TokenType::RParen, "Expected ')' after parameters").is_none() {
                self.current = saved;
                return None;
            }
            Some(params)
        } else {
            None
        };

        // => body expression
        if !self.match_token_exact(TokenType::Arrow) {
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

        Some(MirOrchestrateAgent {
            name,
            with_config: None,
            task_expr: body,
            verify_expr: None,
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
            condition,
            transform: None,
            dynamic: None,
        })
    }

    /// Parse if/else expressions.
    /// Supports two syntax styles:
    /// 1. Expression style: `if cond then expr else else_expr`
    /// 2. Block style: `if cond { then_expr } else { else_expr }`
    /// The `then` keyword is optional when using block syntax `{`.
    /// The `else` branch is optional in both styles.
    fn parse_if_expression(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::If) {
            return None;
        }

        let expr_span = self.span_of_current();

        // Parse condition expression
        let cond = self.parse_expression()?;

        // Check for 'then' keyword or opening brace (block syntax)
        let has_then =
            self.match_token_exact(TokenType::Then) || self.match_token_exact(TokenType::LBrace);

        if !has_then {
            return None;
        }

        // Parse then-branch based on syntax style
        let then_branch = if self.match_token_exact(TokenType::LBrace) {
            // Block syntax: if cond { ... }
            let then_expr = self.parse_expression()?;
            self.consume(TokenType::RBrace, "Expected closing brace '}}'")?;
            then_expr
        } else {
            // Expression syntax: if cond then expr
            self.parse_assignment()?
        };

        // Optional else branch
        let else_branch = if self.match_token_exact(TokenType::Else) {
            Some(self.parse_assignment()?)
        } else {
            None
        };

        // Construct if/else expression node
        Some(MirExpr::if_else(cond, then_branch, else_branch, expr_span))
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

        // Expect block
        if !self.match_token_exact(TokenType::LBrace) {
            eprintln!(
                "Parse error: Expected '{{' after 'for' iterable at line {}",
                self.current_line()
            );
            return None;
        }

        // Parse body expression (skip leading newlines)
        let _ = self.match_token(&[TokenType::Newline]);
        let body = self.parse_assignment()?;

        // Skip newlines and expect closing brace
        let _ = self.match_token(&[TokenType::Newline]);
        self.consume(TokenType::RBrace, "Expected '}' after for loop body")?;

        Some(MirExpr {
            kind: MirExprKind::Loop {
                var,
                iterable: Box::new(iterable),
                body: Box::new(body),
            },
            span: expr_span,
            ty: None,
        })
    }

    /// Parse while loop expressions.
    /// Syntax: `while cond { body }`
    fn parse_while_loop(&mut self) -> Option<MirExpr> {
        if !self.match_token_exact(TokenType::While) {
            return None;
        }

        let expr_span = self.span_of_current();

        // Parse condition expression
        let cond = self.parse_assignment()?;

        // Expect block
        if !self.match_token_exact(TokenType::LBrace) {
            eprintln!(
                "Parse error: Expected '{{' after 'while' condition at line {}",
                self.current_line()
            );
            return None;
        }

        // Parse body expression (skip leading newlines)
        let _ = self.match_token(&[TokenType::Newline]);
        let body = self.parse_assignment()?;

        // Skip newlines and expect closing brace
        let _ = self.match_token(&[TokenType::Newline]);
        self.consume(TokenType::RBrace, "Expected '}' after while loop body")?;

        Some(MirExpr {
            kind: MirExprKind::While {
                cond: Box::new(cond),
                body: Box::new(body),
            },
            span: expr_span,
            ty: None,
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
                    ty: None,
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
                    ty: None,
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
                    ty: None,
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
                    ty: None,
                });
            }

            // Not an assignment, rewind
            self.current = ident_start;
        }

        self.parse_or()
    }

    fn parse_or(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_and()?;

        while let Some(_op) = self.consume_binary_op(&[TokenType::Or]) {
            let right = self.parse_and()?;
            left = MirExpr {
                kind: MirExprKind::Or {
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span: self.span_of_current(),
                ty: None,
            };
        }

        Some(left)
    }

    fn parse_and(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_equality()?;

        while let Some(_op) = self.consume_binary_op(&[TokenType::And]) {
            let right = self.parse_equality()?;
            left = MirExpr {
                kind: MirExprKind::And {
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span: self.span_of_current(),
                ty: None,
            };
        }

        Some(left)
    }

    fn parse_equality(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_comparison()?;

        while let Some(op) = self.consume_binary_op(&[TokenType::Equal, TokenType::NotEqual]) {
            let right = self.parse_comparison()?;
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
        // Check for unary minus
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

        self.parse_call()
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

                // Build a method call as: method(object, arg1, arg2...)
                let old_callee = callee.clone();
                callee = MirExpr::call(
                    MirCallee::Name(format!("{}_{}", match_to_string(&old_callee), method_name)),
                    std::iter::once(old_callee).chain(args).collect(),
                    self.span_of_current(),
                );
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
    /// `a |> b |> c` means `c(b(a))`.
    fn parse_pipe(&mut self) -> Option<MirExpr> {
        let mut left = self.parse_call()?;

        while self.match_token_exact(TokenType::Pipe) {
            let right = self.parse_call()?;
            // Pipe: left |> right becomes right(left)
            // Build as a call where left is the first argument to right
            let right_name = match_to_string(&right);
            let right_span = right.span;
            let args = match right.kind {
                MirExprKind::Call { callee: MirCallee::Name(_name), mut args } => {
                    // If right is already a call, prepend left as first arg
                    args.insert(0, left);
                    args
                }
                _ => {
                    // If right is not a call, treat it as a function with left as arg
                    vec![left]
                }
            };
            left = MirExpr::call(MirCallee::Name(right_name), args, right_span);
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
                if paren_parsed.is_none() {
                    return None; // parse_primary returns Option, not Result
                }
                Some(mir_group(inner))
            }

            // Keywords and other constructs
            TokenType::Fn => {
                self.advance();
                // Closure: fn(params) => body
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
                if !self.match_token_exact(TokenType::Arrow) {
                    eprintln!(
                        "Parse error: Expected '=>' after closure params at line {}",
                        self.current_line()
                    );
                    return None;
                }
                let body = self.parse_assignment()?;
                Some(MirExpr::closure(params, body, span))
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

        // Extract string content from TokenType::String variant
        let content = match &tok.token_type {
            TokenType::String(s) => s.clone(),
            TokenType::PromptString(s) => s.clone(),
            _ => return None,
        };

        Some(MirExpr::lit(Literal::String(content, span), span))
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

// Helper function to convert MirExpr to string (for method name generation)
fn match_to_string(expr: &MirExpr) -> String {
    match &expr.kind {
        MirExprKind::Variable(n) => n.clone(),
        MirExprKind::Literal(lit) => format!("{:?}", lit),
        _ => "expr".to_string(),
    }
}

// Add grouping helper - build grouped expression as identity
fn mir_group(inner: MirExpr) -> MirExpr {
    inner // Currently just return as-is
}
