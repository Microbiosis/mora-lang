use super::*;

type ParsedArgList = (Vec<MirWitness>, Vec<(String, MirWitness)>);
type SliceStopStep = (Option<Box<MirWitness>>, Option<Box<MirWitness>>);

impl ParserV3 {
    /// v0.75.40: 顶层语句循环 — emit 每条语句 + 顶层 witness。
    pub(super) fn emit_program(&mut self) -> crate::error::MoraResult<()> {
        let mut guard = 0usize;
        while !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.is_at_end() {
                break;
            }
            guard += 1;
            if guard > 10_000 {
                return Err(crate::error::MoraError::Parse(
                    "parser_v3: aborted after 10k iterations".to_string(),
                ));
            }
            match self.emit_statement_w() {
                Some(w) => self.witnesses.push(w),
                None => {
                    // v0.104.6 D30：词法错误 token 带着精确病因（超范围字面量、
                    // 非法 bigint、未闭合字符串…）。解析器只把它当陌生物 token，
                    // 于是这些信息全被丢弃、只报一句 "Failed to parse at line N"。
                    // 先回头找词法层的真实诊断再决定报什么。
                    return Err(crate::error::MoraError::Parse(
                        match self.lexical_error_ahead() {
                            Some((msg, line, col)) => {
                                format!("{} at line {}, column {}", msg, line, col)
                            }
                            None => format!("Failed to parse at line {}", self.current_line()),
                        },
                    ));
                }
            }
        }
        Ok(())
    }

    // ===================================================================
    // emit 家族 — 单遍解析（token → MirWitness）
    // ===================================================================
    // 按语法优先级结构直接构造见证树；本层**不发射任何指令** ——
    // 指令序列由 `mir::control::lower` 从见证树 lowering 而来。

    /// 顶层语句分发。
    /// 顶层语句 → witness（嵌套树，emit 时直接构建）。
    fn emit_statement_w(&mut self) -> Option<MirWitness> {
        match self.peek()?.token_type {
            TokenType::Task => self.emit_fn_def_w(),
            TokenType::Let => self.emit_let_w(),
            TokenType::Match => self.emit_match_w(),
            TokenType::If => self.emit_if_w(),
            TokenType::For => self.emit_loop_w(),
            TokenType::Identifier(ref s) if s == "while" => self.emit_while_w(),
            // v0.103: export（标识符分派，spec §10.2）
            TokenType::Identifier(ref s) if s == "export" => self.emit_export_w(),
            // v0.103: 并行块 / worker
            TokenType::Identifier(ref s) if s == "parallel" => self.emit_parallel_w(),
            TokenType::Identifier(ref s) if s == "worker" => self.emit_worker_w(),
            // Wave 2: Go-style concurrency primitives (`spawn` / `select`)
            TokenType::Identifier(ref s) if s == "spawn" && self.is_spawn_form() => {
                self.emit_spawn_w()
            }
            TokenType::Identifier(ref s) if s == "select" && self.is_select_form() => {
                self.emit_select_w()
            }
            // Wave 3 Stage 3C: source-level `trait`, `impl`, and `no_grad`
            TokenType::Identifier(ref s) if s == "trait" && self.is_trait_form() => {
                self.emit_trait_def_w()
            }
            TokenType::Identifier(ref s) if s == "impl" && self.is_impl_form() => {
                self.emit_impl_def_w()
            }
            TokenType::Identifier(ref s) if s == "kernel" && self.is_kernel_form() => {
                self.emit_kernel_def_w()
            }
            TokenType::Identifier(ref s) if s == "no_grad" && self.is_no_grad_form() => {
                self.emit_no_grad_w()
            }
            TokenType::Identifier(ref s) if s == "defer" && self.is_defer_form() => {
                self.emit_defer_w()
            }
            // v0.75.81: 事务家族 + eval 断言（顶层同嵌套分发）
            // v0.104: `assign` 并入本组 —— 它是 spec §14.2 的一等语句
            //（assign_stmt），需在顶层与嵌套上下文都走同一分派。
            TokenType::Identifier(ref s)
                if s == "transaction"
                    || s == "commit"
                    || s == "rollback"
                    || s == "eval"
                    || s == "aggregate"
                    || s == "assign" =>
            {
                self.emit_statement_expr_w()
            }
            // v0.80: algebraic effects 完整语法
            //   handle Effect { body } { handler } end
            //   perform Effect(args)
            // 不引入新 TokenType（不抢旧 identifier），与 transaction/eval 同模式。
            TokenType::Identifier(ref s) if s == "handle" => self.emit_handle_w(),
            TokenType::Identifier(ref s) if s == "perform" => self.emit_perform_w(),
            // v0.98: 显式 effect 签名 —— `effect Name(Hint...): Hint`。
            // 前瞻守卫：仅 `effect Identifier(` 模式拦截（不抢以 `effect`
            // 命名的变量的赋值/调用语句）。
            TokenType::Identifier(ref s)
                if s == "effect"
                    && matches!(
                        self.tokens.get(self.current + 1).map(|t| &t.token_type),
                        Some(TokenType::Identifier(_))
                    )
                    && matches!(
                        self.tokens.get(self.current + 2).map(|t| &t.token_type),
                        Some(TokenType::LParen)
                    ) =>
            {
                self.emit_effect_sig_w()
            }
            TokenType::Return | TokenType::Break | TokenType::Continue => {
                self.emit_return_break_continue_w()
            }
            TokenType::Type => self.emit_type_alias_w(),
            TokenType::Enum => self.emit_enum_def_w(),
            TokenType::Struct => self.emit_struct_def_w(),
            TokenType::Import => self.emit_import_w(),
            TokenType::Macro => self.emit_macro_def_w(),
            _ => {
                // 表达式语句（赋值/字面量/调用等）
                self.emit_expr_w()
            }
        }
    }

    /// 表达式入口（含赋值检测）。
    pub(super) fn emit_expr_w(&mut self) -> Option<MirWitness> {
        // v0.104.2: `if` 出现在**表达式位置**（spec §7.1 的核心示例）
        //     let x = if cond then "a" else "b" end
        // 以及 §7.3 的 `if i == 3 then continue end` 值形态。
        // `emit_if_w` 与其它表达式产出同契约，直接给出 witness。
        //
        // 注：此前这条接线被「合并点永不就绪」的执行器缺陷阻塞（`if` 之后的
        // 语句会静默消失、退出码 0）；该缺陷已随执行器「entry 取 pc 0 可达
        // 节点」的修复而消除（常量折叠后的 else 死块不再与函数入口并列成为
        // 入口），故此处可以安全接线。
        if self.check(&TokenType::If) {
            return self.emit_if_w();
        }
        // v0.80: algebraic effects 表达式位置 dispatch。
        // 表达式位置出现的 `handle` / `perform` 不是变量名 —— 走专门 emission。
        if let Some(TokenType::Identifier(s)) = self.peek().map(|t| &t.token_type).cloned() {
            if s == "handle" {
                return self.emit_handle_w();
            }
            if s == "perform" {
                return self.emit_perform_w();
            }
        }
        if matches!(self.peek()?.token_type, TokenType::Identifier(_)) {
            let ident_start = self.current;
            let name = self.consume_identifier("Expected variable name")?;

            if self.match_token(&[TokenType::Assign]) {
                let v_w = self.emit_expr_w()?;
                let span = self.span_of_current();
                let w = MirWitness {
                    kind: WitnessKind::Assign {
                        target: name,
                        value: Box::new(v_w),
                    },
                    span,
                };
                return Some(w);
            }
            self.current = ident_start;
        }
        let mut w = self.emit_pratt_w()?;
        // P4-1B: Indexed buffer store `buf[idx] = value` is permitted strictly inside `kernel` bodies;
        // regular Mora `list` containers remain immutable so `xs[i] = v` outside kernels stays a parse error.
        if self.scope.in_kernel_body && self.match_token_exact(TokenType::Assign) {
            if let WitnessKind::Call {
                callee: crate::mir::witness::WitnessCallee::Name(ref n),
                ref mut args,
                ref named_args,
            } = w.kind
                && n == "[]"
                && args.len() == 2
                && named_args.is_empty()
            {
                let v_w = self.emit_expr_w()?;
                let idx_w = args.pop().expect("index arg");
                let obj_w = args.pop().expect("object arg");
                return Some(MirWitness {
                    kind: WitnessKind::IndexAssign {
                        object: Box::new(obj_w),
                        index: Box::new(idx_w),
                        value: Box::new(v_w),
                    },
                    span: w.span,
                });
            }
            let msg = format!(
                "Parse error: invalid assignment target at line {}",
                self.current_line()
            );
            eprintln!("{msg}");
            self.diag = Some(msg);
            return None;
        }
        // v0.85: `as dyn Trait` coercion（§3.5 spec 承诺）— emit 路径中
        // 显式将 expression 包装为 Value::TraitObject。
        self.emit_dyn_coercion(w)
    }

    /// W2.3 / G1: 迭代式 Pratt（Shunting-Yard）表达式解析，同时用显式堆栈迭代处理
    /// 括号嵌套 `((...))`、一元前缀 (`-`/`!`/`not`)、右结合幂运算 `**` 与二元/管道链，
    /// 彻底消除深层表达式在 1 MiB 线程栈（含 debug `mora-lsp`）上的 OS 栈溢出风险。
    fn emit_pratt_w(&mut self) -> Option<MirWitness> {
        #[derive(Clone, Copy)]
        enum InfixOp {
            Or,
            And,
            Eq(BinaryOp),
            Pipe,
            Cmp(BinaryOp),
            AddSub(BinaryOp),
            MulDivMod(BinaryOp),
        }

        impl InfixOp {
            fn precedence(self) -> u8 {
                match self {
                    InfixOp::Or => 1,
                    InfixOp::And => 2,
                    InfixOp::Eq(_) => 3,
                    InfixOp::Pipe => 4,
                    InfixOp::Cmp(_) => 5,
                    InfixOp::AddSub(_) => 6,
                    InfixOp::MulDivMod(_) => 7,
                }
            }
        }

        #[derive(Clone, Copy)]
        enum UnaryPrefix {
            Not(Span),
            Minus(Span),
        }

        struct PowPending {
            lhs: MirWitness,
            pow_span: Span,
            outer_prefixes: Vec<UnaryPrefix>,
        }

        struct ParenFrame {
            operands: Vec<(MirWitness, Span)>,
            ops: Vec<InfixOp>,
            prefixes: Vec<UnaryPrefix>,
            pow_stack: Vec<PowPending>,
        }

        fn fold_infix(lhs: MirWitness, op: InfixOp, mut rhs: MirWitness, span: Span) -> MirWitness {
            match op {
                InfixOp::Or => MirWitness {
                    kind: WitnessKind::Or {
                        left: Box::new(lhs),
                        right: Box::new(rhs),
                    },
                    span,
                },
                InfixOp::And => MirWitness {
                    kind: WitnessKind::And {
                        left: Box::new(lhs),
                        right: Box::new(rhs),
                    },
                    span,
                },
                InfixOp::Eq(bop)
                | InfixOp::Cmp(bop)
                | InfixOp::AddSub(bop)
                | InfixOp::MulDivMod(bop) => MirWitness {
                    kind: WitnessKind::Binary {
                        left: Box::new(lhs),
                        op: bop,
                        right: Box::new(rhs),
                    },
                    span,
                },
                InfixOp::Pipe => {
                    let rhs_kind = std::mem::replace(&mut rhs.kind, WitnessKind::Commit);
                    match rhs_kind {
                        WitnessKind::Call {
                            callee: crate::mir::witness::WitnessCallee::Name(method),
                            args,
                            named_args,
                        } => MirWitness {
                            kind: WitnessKind::MethodCall {
                                receiver: Box::new(lhs),
                                method,
                                args,
                                named_args,
                            },
                            span,
                        },
                        WitnessKind::Variable(name) => MirWitness {
                            kind: WitnessKind::Pipe {
                                left: Box::new(lhs),
                                right: Box::new(MirWitness {
                                    kind: WitnessKind::Variable(name),
                                    span,
                                }),
                            },
                            span,
                        },
                        other => MirWitness {
                            kind: WitnessKind::Pipe {
                                left: Box::new(lhs),
                                right: Box::new(MirWitness { kind: other, span }),
                            },
                            span,
                        },
                    }
                }
            }
        }

        fn fold_all(
            operands: &mut Vec<(MirWitness, Span)>,
            ops: &mut Vec<InfixOp>,
        ) -> Option<MirWitness> {
            while let Some(top_op) = ops.pop() {
                let (rhs, span) = operands.pop()?;
                let (lhs, _) = operands.pop()?;
                operands.push((fold_infix(lhs, top_op, rhs, span), span));
            }
            operands.pop().map(|(w, _)| w)
        }

        let apply_unary_prefixes = |this: &Self,
                                    mut expr: MirWitness,
                                    prefixes: Vec<UnaryPrefix>|
         -> Option<MirWitness> {
            for p in prefixes.into_iter().rev() {
                expr = match p {
                    UnaryPrefix::Not(span) => {
                        this.check_unnegated_typed_int(&expr)?;
                        MirWitness {
                            kind: WitnessKind::Call {
                                callee: crate::mir::witness::WitnessCallee::Builtin(
                                    crate::mir::witness::BuiltinOp::Not,
                                ),
                                args: vec![expr],
                                named_args: Vec::new(),
                            },
                            span,
                        }
                    }
                    UnaryPrefix::Minus(span) => match &expr.kind {
                        WitnessKind::Literal(Literal::TypedInt(n, dt, _)) => {
                            let (n, dt) = (*n, *dt);
                            let neg = match n.checked_neg() {
                                Some(v) => v,
                                None => {
                                    eprintln!(
                                        "Parse error: integer literal '-{}' out of range for '{}' at line {}, column {}",
                                        n,
                                        dt.short_name(),
                                        span.line,
                                        span.column
                                    );
                                    return None;
                                }
                            };
                            if let Err(msg) = dt.check_int_range(neg) {
                                eprintln!(
                                    "Parse error: {} at line {}, column {}",
                                    msg, span.line, span.column
                                );
                                return None;
                            }
                            MirWitness {
                                kind: WitnessKind::Literal(Literal::TypedInt(neg, dt, span)),
                                span,
                            }
                        }
                        WitnessKind::Literal(Literal::TypedFloat(f, dt, _)) => {
                            let (f, dt) = (*f, *dt);
                            MirWitness {
                                kind: WitnessKind::Literal(Literal::TypedFloat(
                                    dt.quantize_f64(-f),
                                    dt,
                                    span,
                                )),
                                span,
                            }
                        }
                        _ => MirWitness {
                            kind: WitnessKind::Call {
                                callee: crate::mir::witness::WitnessCallee::Builtin(
                                    crate::mir::witness::BuiltinOp::Neg,
                                ),
                                args: vec![expr],
                                named_args: Vec::new(),
                            },
                            span,
                        },
                    },
                };
            }
            this.check_unnegated_typed_int(&expr)?;
            Some(expr)
        };

        let is_iterative_paren_start = |this: &Self| -> bool {
            if !this.check(&TokenType::LParen) {
                return false;
            }
            let Some(next) = this.tokens.get(this.current + 1) else {
                return false;
            };
            match &next.token_type {
                TokenType::If => false,
                TokenType::Identifier(s) if s == "handle" || s == "perform" => false,
                TokenType::Identifier(_) => !matches!(
                    this.tokens.get(this.current + 2).map(|t| &t.token_type),
                    Some(TokenType::Assign)
                ),
                _ => true,
            }
        };

        let peek_consume_infix = |this: &mut Self| -> Option<InfixOp> {
            if this
                .peek()
                .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "or"))
                .unwrap_or(false)
            {
                this.advance();
                return Some(InfixOp::Or);
            }
            if this
                .peek()
                .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "and"))
                .unwrap_or(false)
            {
                this.advance();
                return Some(InfixOp::And);
            }
            if let Some(op) = this.consume_binary_op(&[TokenType::Equal, TokenType::NotEqual]) {
                return Some(InfixOp::Eq(op));
            }
            if let Some(op) = this.consume_binary_op(&[
                TokenType::Less,
                TokenType::Greater,
                TokenType::LessEqual,
                TokenType::GreaterEqual,
            ]) {
                return Some(InfixOp::Cmp(op));
            }
            if let Some(op) = this.consume_binary_op(&[TokenType::Minus, TokenType::Plus]) {
                return Some(InfixOp::AddSub(op));
            }
            if this.in_struct_field_default
                && this.check(&TokenType::At)
                && matches!(
                    this.tokens.get(this.current + 1).map(|t| &t.token_type),
                    Some(TokenType::Identifier(s)) if s == "tag"
                )
                && matches!(
                    this.tokens.get(this.current + 2).map(|t| &t.token_type),
                    Some(TokenType::LParen)
                )
            {
                return None;
            }
            if let Some(op) = this.consume_binary_op(&[
                TokenType::Star,
                TokenType::Slash,
                TokenType::Percent,
                TokenType::At,
            ]) {
                return Some(InfixOp::MulDivMod(op));
            }
            // D119: 允许 `|>` 跨行；没看到 `|>` 时必须回退跳过的前导换行，避免吞掉下一条语句分隔符。
            let saved = this.current;
            while this.match_token(&[TokenType::Newline]) {}
            if this.match_token_exact(TokenType::Pipe) {
                return Some(InfixOp::Pipe);
            }
            this.current = saved;
            None
        };

        let mut paren_stack: Vec<ParenFrame> = Vec::new();
        let mut operands: Vec<(MirWitness, Span)> = Vec::new();
        let mut ops: Vec<InfixOp> = Vec::new();
        let mut prefixes: Vec<UnaryPrefix> = Vec::new();
        let mut pow_stack: Vec<PowPending> = Vec::new();

        'expect_operand: loop {
            // 1. Collect unary prefixes (`not`, `!`, `-`) iteratively
            loop {
                let span = self.span_of_current();
                if self
                    .peek()
                    .map(|t| {
                        matches!(&t.token_type, TokenType::Identifier(s) if s == "not")
                            || t.token_type == TokenType::Bang
                    })
                    .unwrap_or(false)
                {
                    self.advance();
                    prefixes.push(UnaryPrefix::Not(span));
                } else if self.match_token(&[TokenType::Minus]) {
                    prefixes.push(UnaryPrefix::Minus(span));
                } else {
                    break;
                }
            }

            // 2. Open nested parenthesized Pratt sub-expression iteratively on heap stack
            if is_iterative_paren_start(self) {
                self.advance(); // consume '('
                paren_stack.push(ParenFrame {
                    operands: std::mem::take(&mut operands),
                    ops: std::mem::take(&mut ops),
                    prefixes: std::mem::take(&mut prefixes),
                    pow_stack: std::mem::take(&mut pow_stack),
                });
                continue 'expect_operand;
            }

            // 3. Parse call/primary atom
            let mut atom = self.emit_call_w()?;

            'have_atom: loop {
                // 4. Right-associative `**` binds tighter than unary `-` on LHS
                if self.check(&TokenType::StarStar) {
                    self.check_unnegated_typed_int(&atom)?;
                    let pow_span = self.span_of_current();
                    self.advance();
                    pow_stack.push(PowPending {
                        lhs: atom,
                        pow_span,
                        outer_prefixes: std::mem::take(&mut prefixes),
                    });
                    continue 'expect_operand;
                }

                // 5. Apply current unary prefixes and unwind right-associative `**` chain
                let mut expr = apply_unary_prefixes(self, atom, std::mem::take(&mut prefixes))?;
                while let Some(pending) = pow_stack.pop() {
                    let pow_expr = MirWitness {
                        kind: WitnessKind::Binary {
                            left: Box::new(pending.lhs),
                            op: BinaryOp::Pow,
                            right: Box::new(expr),
                        },
                        span: pending.pow_span,
                    };
                    expr = apply_unary_prefixes(self, pow_expr, pending.outer_prefixes)?;
                }

                let span = self.span_of_current();
                operands.push((expr, span));

                // 6. Check for infix operator at current paren level
                if let Some(op) = peek_consume_infix(self) {
                    let prec = op.precedence();
                    while let Some(&top_op) = ops.last() {
                        if top_op.precedence() >= prec {
                            let top_op = ops.pop().expect("ops non-empty");
                            let (rhs, rspan) = operands.pop().expect("rhs operand present");
                            let (lhs, _) = operands.pop().expect("lhs operand present");
                            operands.push((fold_infix(lhs, top_op, rhs, rspan), rspan));
                        } else {
                            break;
                        }
                    }
                    ops.push(op);
                    continue 'expect_operand;
                }

                // 7. No infix op: if inside an iterative `(`, close it and apply postfix tail!
                if let Some(frame) = paren_stack.pop() {
                    let inner = fold_all(&mut operands, &mut ops)?;
                    let inner = self.emit_dyn_coercion(inner)?;
                    self.consume(TokenType::RParen, "Expected ')'")?;
                    atom = self.emit_call_tail_w(inner)?;
                    operands = frame.operands;
                    ops = frame.ops;
                    prefixes = frame.prefixes;
                    pow_stack = frame.pow_stack;
                    continue 'have_atom;
                }

                // 8. Top-level completion
                return fold_all(&mut operands, &mut ops);
            }
        }
    }

    fn check_unnegated_typed_int(&self, w: &MirWitness) -> Option<()> {
        if let WitnessKind::Literal(Literal::TypedInt(n, dt, lit_span)) = &w.kind
            && let Err(msg) = dt.check_int_range(*n)
        {
            eprintln!(
                "Parse error: {} at line {}, column {}",
                msg, lit_span.line, lit_span.column
            );
            return None;
        }
        Some(())
    }

    /// 调用链（函数 / 方法 / 索引 / DynTrait）。
    fn emit_call_w(&mut self) -> Option<MirWitness> {
        // P4-1A: Nominal generic constructor / associated call (`Linear<4, 3>(w, b)` or `Linear<4, 3>::new(w, b)`).
        if let TokenType::Identifier(type_name) = self.peek()?.token_type.clone()
            && self.declared_type_names.contains(type_name.as_str())
            && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.token_type),
                Some(TokenType::Less)
            )
        {
            let save = self.current;
            let save_diag = self.diag.clone();
            let span = self.span_of_current();
            self.advance(); // type_name
            self.advance(); // '<'
            let mut type_args = Vec::new();
            let mut ok = true;
            while !self.check(&TokenType::Greater) && !self.is_at_end() {
                if let Some(ty) = self.parse_type_annotation() {
                    type_args.push(crate::mir::hint::TypeHint::from_type(ty));
                } else {
                    ok = false;
                    break;
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            if ok && !type_args.is_empty() && self.match_token(&[TokenType::Greater]) {
                if self.match_token_exact(TokenType::ColonColon) {
                    let assoc =
                        self.consume_identifier("Expected associated function after '::'")?;
                    let full = format!("{type_name}::{assoc}");
                    let (arg_wits, named_args) = if self.match_token_exact(TokenType::LParen) {
                        self.emit_arg_list_w()?
                    } else {
                        (Vec::new(), Vec::new())
                    };
                    let w = MirWitness {
                        kind: WitnessKind::Call {
                            callee: crate::mir::witness::WitnessCallee::GenericName {
                                name: full,
                                type_args,
                            },
                            args: arg_wits,
                            named_args,
                        },
                        span,
                    };
                    return self.emit_call_tail_w(w);
                }
                if self.match_token_exact(TokenType::LParen) {
                    let (arg_wits, named_args) = self.emit_arg_list_w()?;
                    let w = MirWitness {
                        kind: WitnessKind::Call {
                            callee: crate::mir::witness::WitnessCallee::GenericName {
                                name: type_name,
                                type_args,
                            },
                            args: arg_wits,
                            named_args,
                        },
                        span,
                    };
                    return self.emit_call_tail_w(w);
                }
            }
            self.current = save;
            self.diag = save_diag;
        }
        // `Type::member(args)` 关联调用（例如 Enum::Variant / Trait::new），
        // 编译为按名调用 "Type::member"，由 `runtime::vm::builtins` 分发。
        if let TokenType::Identifier(type_name) = self.peek()?.token_type.clone() {
            let save = self.current;
            let span = self.span_of_current();
            self.advance();
            if self.match_token_exact(TokenType::ColonColon) {
                let assoc = self.consume_identifier("Expected associated function after '::'")?;
                let full = format!("{}::{}", type_name, assoc);
                let (arg_wits, named_args) = if self.match_token_exact(TokenType::LParen) {
                    self.emit_arg_list_w()?
                } else {
                    (Vec::new(), Vec::new())
                };
                let w = MirWitness {
                    kind: WitnessKind::Call {
                        callee: crate::mir::witness::WitnessCallee::Name(full),
                        args: arg_wits,
                        named_args,
                    },
                    span,
                };
                return self.emit_call_tail_w(w);
            }
            self.current = save;
        }
        // 函数名调用：`name(args)` — 与 lower Call 一致（不 emit Var 加载）。
        if let TokenType::Identifier(name) = self.peek()?.token_type.clone() {
            let save = self.current;
            let span = self.span_of_current();
            self.advance();
            if self.match_token_exact(TokenType::LParen) {
                let (arg_wits, named_args) = self.emit_arg_list_w()?;
                let w = MirWitness {
                    kind: WitnessKind::Call {
                        callee: crate::mir::witness::WitnessCallee::Name(name),
                        args: arg_wits,
                        named_args,
                    },
                    span,
                };
                // 后缀链（方法/索引）可能改写 witness
                return self.emit_call_tail_w(w);
            }
            self.current = save; // 非调用，回退走 primary
        }

        let callee_w = self.emit_primary_w()?;
        self.emit_call_tail_w(callee_w)
    }

    /// 后缀链（witness 嵌套版）：方法调用 obj.m(args) / 索引与多维切片 obj[idx, ...] /
    /// 值调用 `f(…)(…)`。
    fn emit_call_tail_w(&mut self, mut callee_w: MirWitness) -> Option<MirWitness> {
        loop {
            if self.match_token_exact(TokenType::Dot) {
                self.check_unnegated_typed_int(&callee_w)?;
                let method_name = self.consume_identifier("Expected method name")?;
                let span = self.span_of_current();
                let mut arg_wits = Vec::new();
                let mut named_args = Vec::new();
                if self.match_token_exact(TokenType::LParen) {
                    let (aw, na) = self.emit_arg_list_w()?;
                    arg_wits = aw;
                    named_args = na;
                }
                callee_w = MirWitness {
                    kind: WitnessKind::MethodCall {
                        receiver: Box::new(callee_w),
                        method: method_name,
                        args: arg_wits,
                        named_args,
                    },
                    span,
                };
            } else if self.match_token_exact(TokenType::LBracket) {
                self.check_unnegated_typed_int(&callee_w)?;
                let span = self.span_of_current();
                callee_w = self.emit_index_or_slice_w(callee_w, span)?;
            } else if self.match_token_exact(TokenType::LParen) {
                self.check_unnegated_typed_int(&callee_w)?;
                let span = self.span_of_current();
                let (arg_wits, named_args) = self.emit_arg_list_w()?;
                callee_w = MirWitness {
                    kind: WitnessKind::Call {
                        callee: crate::mir::witness::WitnessCallee::Evaluated(Box::new(callee_w)),
                        args: arg_wits,
                        named_args,
                    },
                    span,
                };
            } else if self.match_token_exact(TokenType::Question) {
                self.check_unnegated_typed_int(&callee_w)?;
                let span = self.span_of_current();
                callee_w = MirWitness {
                    kind: WitnessKind::Try {
                        expr: Box::new(callee_w),
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Some(callee_w)
    }

    /// Parse optional `stop` and `:step` after an opening `:` or `..` inside a slice axis.
    fn parse_slice_stop_step(&mut self) -> Option<SliceStopStep> {
        let stop = if self.check(&TokenType::Colon)
            || self.check(&TokenType::Comma)
            || self.check(&TokenType::RBracket)
        {
            None
        } else {
            Some(Box::new(self.emit_expr_w()?))
        };
        let step = if self.match_token_exact(TokenType::Colon) {
            if self.check(&TokenType::Comma) || self.check(&TokenType::RBracket) {
                None
            } else {
                Some(Box::new(self.emit_expr_w()?))
            }
        } else {
            None
        };
        Some((stop, step))
    }

    /// Parse single indexing `obj[idx]` OR multi-dimensional tensor slicing `t[0..4, :, -1]`,
    /// `t[1:4:2, ..., None]`. Single plain index lowers to existing `[]` call for full
    /// list/dict/string compatibility; multi-axis or slice specs emit `WitnessKind::TensorSlice`.
    fn emit_index_or_slice_w(&mut self, target: MirWitness, span: Span) -> Option<MirWitness> {
        use crate::mir::witness::WitnessSliceAxis;
        let mut axes: Vec<WitnessSliceAxis> = Vec::new();
        while !self.check(&TokenType::RBracket) && !self.is_at_end() {
            let axis = if self.match_token_exact(TokenType::DotDotDot) {
                WitnessSliceAxis::Ellipsis
            } else if matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Identifier(s)) if s == "None"
            ) && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.token_type),
                Some(TokenType::Comma | TokenType::RBracket)
            ) {
                self.advance();
                WitnessSliceAxis::NewAxis
            } else if self.match_token_exact(TokenType::ColonColon) {
                let step = if self.check(&TokenType::Comma) || self.check(&TokenType::RBracket) {
                    None
                } else {
                    Some(Box::new(self.emit_expr_w()?))
                };
                WitnessSliceAxis::Range {
                    start: None,
                    stop: None,
                    step,
                }
            } else if self.match_token(&[TokenType::Colon, TokenType::DotDot]) {
                let (stop, step) = self.parse_slice_stop_step()?;
                WitnessSliceAxis::Range {
                    start: None,
                    stop,
                    step,
                }
            } else {
                let start_expr = self.emit_expr_w()?;
                if self.match_token_exact(TokenType::ColonColon) {
                    let step = if self.check(&TokenType::Comma) || self.check(&TokenType::RBracket)
                    {
                        None
                    } else {
                        Some(Box::new(self.emit_expr_w()?))
                    };
                    WitnessSliceAxis::Range {
                        start: Some(Box::new(start_expr)),
                        stop: None,
                        step,
                    }
                } else if self.match_token(&[TokenType::Colon, TokenType::DotDot]) {
                    let (stop, step) = self.parse_slice_stop_step()?;
                    WitnessSliceAxis::Range {
                        start: Some(Box::new(start_expr)),
                        stop,
                        step,
                    }
                } else {
                    WitnessSliceAxis::Index(Box::new(start_expr))
                }
            };
            axes.push(axis);
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RBracket, "Expected ']' after index or slice")?;
        if axes.is_empty() {
            let msg = format!(
                "Parse error: empty index expression `[]` at line {}",
                self.current_line()
            );
            eprintln!("{msg}");
            self.diag = Some(msg);
            return None;
        }
        if axes.len() == 1 && matches!(axes.first(), Some(WitnessSliceAxis::Index(_))) {
            let Some(WitnessSliceAxis::Index(idx_w)) = axes.pop() else {
                unreachable!("verified Index axis");
            };
            return Some(MirWitness {
                kind: WitnessKind::Call {
                    callee: crate::mir::witness::WitnessCallee::Name("[]".to_string()),
                    args: vec![target, *idx_w],
                    named_args: Vec::new(),
                },
                span,
            });
        }
        Some(MirWitness {
            kind: WitnessKind::TensorSlice {
                target: Box::new(target),
                axes,
            },
            span,
        })
    }

    /// Parse positional arguments followed by optional Python-style keyword arguments `name = expr`.
    fn emit_arg_list_w(&mut self) -> Option<ParsedArgList> {
        let mut wits = Vec::new();
        let mut named: Vec<(String, MirWitness)> = Vec::new();
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            if let Some(Token {
                token_type: TokenType::Identifier(kw_name),
                ..
            }) = self.peek().cloned()
                && matches!(
                    self.tokens.get(self.current + 1).map(|t| &t.token_type),
                    Some(TokenType::Assign)
                )
            {
                if named.iter().any(|(k, _)| k == &kw_name) {
                    let msg = format!(
                        "Parse error: duplicate keyword argument '{}' at line {}",
                        kw_name,
                        self.current_line()
                    );
                    eprintln!("{msg}");
                    self.diag = Some(msg);
                    return None;
                }
                self.advance(); // kw_name
                self.advance(); // '='
                let val_w = self.emit_expr_w()?;
                named.push((kw_name, val_w));
            } else {
                if !named.is_empty() {
                    let msg = format!(
                        "Parse error: positional argument cannot follow keyword argument at line {}",
                        self.current_line()
                    );
                    eprintln!("{msg}");
                    self.diag = Some(msg);
                    return None;
                }
                let w = self.emit_expr_w()?;
                wits.push(w);
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RParen, "Expected ')'")?;
        Some((wits, named))
    }

    /// 主表达式 — 产出嵌套 witness。
    /// v0.75.41: witness 递归构建（子节点嵌进父节点，typeck/LSP 消费树形）。
    fn emit_primary_w(&mut self) -> Option<MirWitness> {
        if self.is_spawn_form() {
            return self.emit_spawn_w();
        }
        if self.is_select_form() {
            return self.emit_select_w();
        }
        if self.is_no_grad_form() {
            return self.emit_no_grad_w();
        }
        let token = self.peek().cloned()?;
        let span = crate::common::Span::new(token.line, token.column);

        let wit = match token.token_type {
            TokenType::Int(val) => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::Int(val, span)),
                    span,
                }
            }
            TokenType::TypedInt(val, dt) => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::TypedInt(val, dt, span)),
                    span,
                }
            }
            TokenType::Float(val) => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::Float(val, span)),
                    span,
                }
            }
            TokenType::TypedFloat(val, dt) => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::TypedFloat(val, dt, span)),
                    span,
                }
            }
            // v0.104.6 D15：char 字面量 `'a'`。
            //
            // **spec 两处明确定义了它**（`docs/mora-spec.md:109` 的类型表
            // `| char | 'a' |`，以及 EBNF `literal = NUMBER | … | CHAR | …`
            // 见 :1397），词法器也**早已支持**（`lexer.rs` 的 `'` 分支 →
            // `TokenType::Char(ch)`），但主表达式发射器这里曾唯独漏了，于是
            //
            // ```text
            // let c = 'a'   →  Failed to parse at line 2（真实 CLI exit 2）
            // ```
            //
            // 后果不止「少个语法」：`Value::Char` 有 Display 臂、有
            // `Value::methods()` 条目、且由 `s[i]` 字符串索引产生，却在源码层
            // **无法直接构造** —— spec 里写着的字面量写不出来。
            //
            // 与 `TokenType::String` 臂同构。
            TokenType::Char(c) => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::Char(c, span)),
                    span,
                }
            }
            TokenType::String(ref s) => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::String(s.clone(), span)),
                    span,
                }
            }
            TokenType::True => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::Bool(true, span)),
                    span,
                }
            }
            TokenType::False => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::Bool(false, span)),
                    span,
                }
            }
            TokenType::Nil => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(span)),
                    span,
                }
            }
            TokenType::Identifier(name) => {
                self.advance();

                MirWitness {
                    kind: WitnessKind::Variable(name),
                    span,
                }
            }
            TokenType::LBracket => return self.emit_list_w(),
            TokenType::LBrace => return self.emit_dict_w(),
            // v0.87: match 表达式位置 — `let x = match ... { ... }` 在 emit_let_w
            // → emit_expr_w → emit_or_w → ... → emit_primary_w 链末端必须能产出
            // match 指令。修复前：emit_primary_w 仅处理字面量/标识符/列表/字典/闭包/
            // quote，Match 落到 _ 默认分支返回 None，令 `let result = match 42 { ... }`
            // 解析失败。此处与嵌套语句分发（line 1515）及顶层分发（line 43）保持
            // 同一入口，确保 match 在任意表达式位置可用。
            TokenType::Match => return self.emit_match_w(),
            TokenType::LParen => {
                self.advance();
                let w = self.emit_expr_w()?;
                self.consume(TokenType::RParen, "Expected ')'")?;
                w
            }
            TokenType::Fn => {
                self.advance();
                // v0.104.6：泛型闭包子句 `fn<T>(…)`。闭包本身无 `type_params`
                // 字段（它靠 let-泛化成为多态），但形参标注里的 `T` 需被认作
                // 类型变量 ⇒ 与 task 同理，进入/退出泛型作用域。
                let saved_generic = std::mem::take(&mut self.generic_params);
                let type_params = match self.parse_generic_clause() {
                    Some(tp) => {
                        if tp.is_empty() {
                            self.generic_params = saved_generic.clone();
                        }
                        tp
                    }
                    None => {
                        self.generic_params = saved_generic;
                        return None;
                    }
                };
                if !self.match_token_exact(TokenType::LParen) {
                    self.generic_params = saved_generic;
                    return None;
                }
                let param_wits = match self.parse_witness_params(false, true) {
                    Some((ps, _)) => ps,
                    None => {
                        self.generic_params = saved_generic;
                        return None;
                    }
                };
                if self
                    .consume(TokenType::RParen, "Expected ')' after parameters")
                    .is_none()
                {
                    self.generic_params = saved_generic;
                    return None;
                }
                // 子上下文：闭包体是独立作用域（`is_program_top = false`, `parse_loop_depth = 0`）
                let body_res = self.in_nested_scope(|this| {
                    if this.match_token_exact(TokenType::FatArrow)
                        || this.inline_closure_body_is_expr()
                    {
                        let w = this.emit_expr_w()?;
                        // 可选的显式 `end`：两种拼法都要认。
                        this.match_token_exact(TokenType::End);
                        Some(w)
                    } else {
                        this.emit_block_w()
                    }
                });
                // 退出泛型作用域 —— 无论体解析成功或失败都必须还原，
                // 避免 `generic_params` 泄漏到后续无关标注。
                self.generic_params = saved_generic;
                let body_w = body_res?;

                MirWitness {
                    kind: WitnessKind::Closure {
                        type_params,
                        params: param_wits,
                        body: Box::new(body_w),
                    },
                    span,
                }
            }
            // v0.86: Lisp homoiconicity — `quote(expr)` 在解析期提取 expr 源码文本，
            // 以 Value::String 传入 builtin quote()，quote() 返回 Value::Code。
            TokenType::Quote => return self.emit_quote_w(),
            // v0.88: `expr` — Lisp quasiquote（反引号 + unquote/unquote-splice）。
            // 与 quote(expr) 对称：quote 冻结整个源码；quasiquote 选择性冻结。
            TokenType::Backtick => return self.emit_quasiquote_w(),
            _ => return None,
        };
        Some(wit)
    }

    /// v0.86: `quote(expr)` — Lisp homoiconicity 的第三块基石。
    ///
    /// 解析期语义：
    ///   1. consume `quote` 关键字
    ///   2. consume `(`
    ///   3. 从 ParserV3.source 中切片出 `(` 到 `)` 之间的源码文本（跟踪括号深度）
    ///   4. consume `)`
    ///   5. emit `Const(str_reg, String(quoted_text))`
    ///   6. emit `Call(dst_reg, "quote", [str_reg])`
    ///
    /// 运行时 `quote` builtin 将 Value::String → Value::Code，完成
    /// eval↔quote 往返不变式：eval(quote(expr)) == expr。
    fn emit_quote_w(&mut self) -> Option<MirWitness> {
        let quote_span = self.span_of_current();
        self.consume(TokenType::Quote, "Expected 'quote' keyword")?;
        // `quote` 后应紧跟 `(`
        let open_paren_span = self.span_of_current();
        self.consume(TokenType::LParen, "Expected '(' after quote")?;
        // 跟踪括号深度，找到匹配的 `)`
        let mut depth = 1usize;
        while depth > 0 && !self.is_at_end() {
            let tok = self.peek().cloned();
            if let Some(t) = tok {
                match t.token_type {
                    TokenType::LParen => depth += 1,
                    TokenType::RParen => depth -= 1,
                    _ => {}
                }
            }
            if depth > 0 {
                self.advance();
            }
        }
        if depth != 0 {
            return None; // 括号不匹配
        }
        let close_paren_span = self.span_of_current();
        self.consume(TokenType::RParen, "Expected ')' to close quote")?;

        // 从 source 中切片：(expr) → expr 的源码文本
        let start_byte = self.source_byte_at(open_paren_span.line, open_paren_span.column) + 1;
        let end_byte = self.source_byte_at(close_paren_span.line, close_paren_span.column);
        let quoted_text = &self.source[start_byte..end_byte];

        let span = quote_span;
        // emit: Const(str_reg, String(quoted_text))
        // emit: Call(dst_reg, "quote", [str_reg])

        let w = MirWitness {
            kind: WitnessKind::Call {
                callee: crate::mir::witness::WitnessCallee::Name("quote".to_string()),
                args: vec![MirWitness {
                    kind: WitnessKind::Literal(Literal::String(quoted_text.to_string(), span)),
                    span,
                }],
                named_args: Vec::new(),
            },
            span,
        };
        Some(w)
    }

    /// v0.88: `expr` — Lisp quasiquote（反引号 + unquote / unquote-splice）。
    ///
    /// 解析期语义（与 quote(expr) 对称）：
    ///   1. consume Backtick `` ` ``，记录 start_span
    ///   2. 遍历后续 token，跟踪括号深度 `()` `[]` `{}` `<>`
    ///   3. 深度 0 时，`Comma` → unquote（先 emit 子表达式，记录 reg）
    ///   4. 深度 0 时，`CommaComma` → unquote-splice
    ///   5. 深度 > 0 时，逗号为静态源码的一部分
    ///   6. 深度 0 时遇到 Newline → quasiquote 结束（EOF 由 is_at_end 自然终止）
    ///   7. 静态源码片段用 source_byte_at 切片
    ///
    /// 运行时 `eval_quasiquote` 遍历 segments：Quote 直接拼接，Unquote 取寄存器值
    /// 经 Mora Display 格式化，UnquoteSplice 取 List 展平为逗号分隔代码。
    /// 最终 dst 写入 Value::Code(重组源码字符串)，与 quote(expr) 返回类型一致。
    fn emit_quasiquote_w(&mut self) -> Option<MirWitness> {
        let start_span = self.span_of_current();
        self.consume(TokenType::Backtick, "Expected '`' for quasiquote")?;

        let mut capture_start = match self.peek() {
            Some(t) => self.source_byte_at(t.line, t.column),
            None => self.source.len(),
        };

        let mut witness_segments: Vec<MirWitness> = Vec::new();
        let mut depth = 0usize;

        'scan: while !self.is_at_end() {
            let tok = match self.peek() {
                Some(t) => t,
                None => break,
            };

            match &tok.token_type {
                TokenType::LParen | TokenType::LBracket | TokenType::LBrace | TokenType::Less => {
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
                _ if depth == 0 => match &tok.token_type {
                    TokenType::Comma | TokenType::CommaComma => {
                        let is_splice = matches!(&tok.token_type, TokenType::CommaComma);

                        let end = self.source_byte_at(tok.line, tok.column);
                        if end > capture_start {
                            let text = &self.source[capture_start..end];
                            if !text.is_empty() {
                                witness_segments.push(MirWitness {
                                    kind: WitnessKind::Literal(Literal::String(
                                        text.to_string(),
                                        start_span,
                                    )),
                                    span: start_span,
                                });
                            }
                        }

                        self.advance();
                        {
                            let w = self.emit_expr_w()?;
                            if is_splice {
                                // v0.104.6 D278：把 splice 标记**写进 witness**。
                                //
                                // 标记形态：`Literal(Bool(true))` 独占一段 ——
                                // `mir::control::lower` 按此把它后面的子表达式
                                // 降成 `ControlQqSegment::UnquoteSplice`。
                                let mark_span = self.span_of_current();
                                witness_segments.push(MirWitness {
                                    kind: WitnessKind::Literal(Literal::Bool(true, mark_span)),
                                    span: mark_span,
                                });
                            } else {
                                // 非 splice 段：不产出标记，直接推子表达式 witness。
                            }
                            witness_segments.push(w);
                            if let Some(next) = self.peek() {
                                capture_start = self.source_byte_at(next.line, next.column);
                            }
                        }
                        continue 'scan;
                    }
                    _ => {
                        self.advance();
                    }
                },
                _ => {
                    self.advance();
                }
            }
        }

        // Capture trailing static text up to the terminating token (Newline/EOF)
        if let Some(tok) = self.peek() {
            let end = self.source_byte_at(tok.line, tok.column);
            if end > capture_start {
                let text = &self.source[capture_start..end];
                if !text.is_empty() {
                    witness_segments.push(MirWitness {
                        kind: WitnessKind::Literal(Literal::String(text.to_string(), start_span)),
                        span: start_span,
                    });
                }
            }
        }

        let w = MirWitness {
            kind: WitnessKind::Quasiquote {
                segments: witness_segments,
            },
            span: start_span,
        };
        Some(w)
    }

    fn emit_list_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.consume(TokenType::LBracket, "Expected '['")?;
        let mut item_wits = Vec::new();
        while !self.check(&TokenType::RBracket) && !self.is_at_end() {
            if let Some(wit) = self.emit_expr_w() {
                item_wits.push(wit);
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RBracket, "Expected ']'")?;
        let w = MirWitness {
            kind: WitnessKind::List(item_wits),
            span,
        };
        Some(w)
    }

    fn emit_dict_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.consume(TokenType::LBrace, "Expected '{'")?;
        let mut entry_wits = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            if let Some(key) = self.emit_dict_key() {
                self.consume(TokenType::Colon, "Expected ':' after dict key")?;
                if let Some(wit) = self.emit_expr_w() {
                    entry_wits.push((key, wit));
                }
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RBrace, "Expected '}'")?;
        let w = MirWitness {
            kind: WitnessKind::Dict(entry_wits),
            span,
        };
        Some(w)
    }

    fn emit_dict_key(&mut self) -> Option<String> {
        let _span = self.span_of_current();
        // dict key：Identifier / String 字面量 / **关键字**。
        //
        // v0.103: 接受关键字作键 —— 此前只认 Identifier 与 String，导致
        // `{name: {type: "string"}}`（spec §18.1 MCP 工具 schema 的标准写法）
        // 因 `type` 是 TokenType::Type 关键字而解析失败。dict 键位置是
        // 标识符语义（非语法关键字），与 consume_identifier 同规则。
        let tok = self.peek().cloned()?;
        match tok.token_type {
            TokenType::Identifier(name) => {
                self.advance();
                Some(name)
            }
            TokenType::String(s) => {
                self.advance();
                Some(s)
            }
            other => {
                if let Some(name) = token_to_identifier_name(&other) {
                    self.advance();
                    Some(name.to_string())
                } else {
                    None
                }
            }
        }
    }

    // ── 语句 emit ──

    fn emit_import_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'import'
        // v0.92: import 路径支持三种形式：
        //   1. `import "path/to/file.mora"` — 引号字符串（含 `/` 和 `.`）
        //   2. `import path/to/file.mora`   — 裸多组件路径（`/` `.` 分隔）
        //   3. `import mod_a`               — 单标识符（向后兼容）
        let path = match self.peek().cloned() {
            Some(tok) => match tok.token_type {
                // 形式 1：引号字符串
                TokenType::String(ref s) => {
                    let p = s.clone();
                    self.advance();
                    p
                }
                _ => {
                    // 形式 2/3：拼接标识符 + `/` + `.` 组成路径，直到行尾/空白。
                    // 例：`tests/fixtures/mod_a.mora`
                    //
                    // v0.104.6 D29：**换行必须终止本循环，且不得被消费**。
                    // 旧代码把 `Newline` 放进「继续」集合里（只 `push` 空串、
                    // 然后照样 `advance()`），于是解析跨过行尾继续收下一行的
                    // 标识符：
                    //
                    // ```text
                    // import nosuchmod print(1)   →  路径 "nosuchmodprint"
                    // import nosuchmod
                    // print(1)                     →  路径 "nosuchmodprint"（跨行！）
                    // ```
                    //
                    // 即**正常的多行 `import` 会把下一行第一个标识符吞进模块
                    // 路径**，`import math` + 下一行 `print(...)` 变成
                    // `mathprint` —— 报出来的模块名与源码里写的完全对不上，
                    // 排查时根本看不出真实问题。
                    let mut p = String::new();
                    loop {
                        match self.peek().cloned() {
                            Some(t)
                                if matches!(
                                    t.token_type,
                                    TokenType::Identifier(_) | TokenType::Slash | TokenType::Dot
                                ) =>
                            {
                                match &t.token_type {
                                    TokenType::Slash => p.push('/'),
                                    TokenType::Dot => p.push('.'),
                                    TokenType::Identifier(name) => p.push_str(name),
                                    _ => unreachable!(),
                                }
                                self.advance();
                            }
                            _ => break,
                        }
                    }
                    if p.is_empty() {
                        // 回退：单标识符（错误信息保持原样）
                        self.consume_identifier("Expected import path")?
                    } else {
                        p
                    }
                }
            },
            None => self.consume_identifier("Expected import path")?,
        };
        let mut visited = std::collections::HashSet::new();
        self.prepopulate_imported_type_names(&path, &mut visited);
        Some(MirWitness {
            kind: WitnessKind::Import(path),
            span,
        })
    }

    /// Pre-register exported nominal type names (`struct`/`enum`/`trait`/`type`) from an
    /// imported module so caller type annotations (`x: HttpRequest`) parse in a single pass.
    fn prepopulate_imported_type_names(
        &mut self,
        path: &str,
        visited: &mut std::collections::HashSet<String>,
    ) {
        if !visited.insert(path.to_string()) {
            return;
        }
        let src = if let Some(s) = self
            .virtual_modules
            .as_ref()
            .and_then(|m| m.get(path))
            .cloned()
        {
            s
        } else if let Some(s) = crate::resolve_std_module(path) {
            s.to_string()
        } else if let Ok(s) = std::fs::read_to_string(path) {
            s
        } else {
            return;
        };
        let toks = crate::lexer::Lexer::new(&src).scan_tokens();
        let mut i = 0usize;
        while i < toks.len() {
            match &toks[i].token_type {
                TokenType::Import => {
                    if let Some(tok) = toks.get(i + 1)
                        && let TokenType::String(sub) = &tok.token_type
                    {
                        self.prepopulate_imported_type_names(sub, visited);
                        i += 2;
                        continue;
                    }
                }
                TokenType::Identifier(s) if s == "export" => {
                    if let (Some(kind_tok), Some(name_tok)) = (toks.get(i + 1), toks.get(i + 2)) {
                        let is_type_decl = matches!(
                            &kind_tok.token_type,
                            TokenType::Struct | TokenType::Enum | TokenType::Type
                        ) || matches!(
                            &kind_tok.token_type,
                            TokenType::Identifier(s) if s == "trait"
                        );
                        if is_type_decl && let TokenType::Identifier(name) = &name_tok.token_type {
                            self.declared_type_names.insert(name.clone());
                            i += 3;
                            continue;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// v0.80: perform Effect(arg1, arg2, ...) 解析。
    ///
    /// syntax: `perform Effect(arg1, arg2, ...)` — args 是 `emit_expr_w` 列表。
    /// 不引入新 TokenType（按 Identifier 路径分发，避免抢旧 identifier）。
    /// 未被 handler 覆盖的 perform 由 typeck 在编译期拦截（row-poly HM 效果边界断言）。
    fn emit_perform_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'perform'
        let effect = self.consume_identifier("Expected effect name after 'perform'")?;
        let mut args: Vec<MirWitness> = Vec::new();
        if self.match_token_exact(TokenType::LParen) {
            if !self.check(&TokenType::RParen) {
                loop {
                    let w = self.emit_expr_w()?;
                    args.push(w);
                    if !self.match_token(&[TokenType::Comma]) {
                        break;
                    }
                }
            }
            self.consume(TokenType::RParen, "Expected ')' after perform args")?;
        }
        // v0.104.6 D472：裸 `perform` 必须有结构化见证 ——
        // 否则顶层裸 `perform` 会静默 no-op（`g = perform Ai("x")` 把 `g` 绑成
        // 旧值，用户写的 effect 请求被无声吞掉）。见证层补齐后，
        // `mir::control::lower` 将其降成 `ControlTerminator::Perform`，
        // 由 VM 在无匹配 handler 时报 "unhandled effect"。
        Some(MirWitness {
            kind: WitnessKind::Perform { effect, args },
            span,
        })
    }

    /// v0.80: handle Effect { body } { handler } 完整解析。
    ///
    /// syntax: `handle Effect { body_stmts } { handler_stmts }`（花括号形式）。
    /// body 与 handler 是两棵独立的见证子树，各自单独解析成块。
    /// 不引入新 TokenType（按 Identifier 路径分发）。
    /// handler 可使用 `resume k resume-value` 续名续 + 标 typing。
    ///
    /// 见证层只保留 `WitnessKind::Handle { effect, body, handler, k_param }`，
    /// `k_param` 固定为 `"resume"`；指令序列由 `mir::control::lower` 生成
    /// （`ControlInst::Handle` + `ControlTerminator::Perform`）。
    fn emit_handle_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'handle'
        let effect = self.consume_identifier("Expected effect name after 'handle'")?;
        // body 块（必须花括号形式）
        self.consume(TokenType::LBrace, "Expected '{' after effect name")?;
        let body_w = self.emit_brace_block_w()?;
        self.consume(TokenType::RBrace, "Expected '}' after handle body")?;
        // handler 块（必须花括号形式）
        self.consume(TokenType::LBrace, "Expected '{' for handler block")?;
        let handler_w = self.emit_brace_block_w()?;
        self.consume(TokenType::RBrace, "Expected '}' after handler block")?;
        Some(MirWitness {
            kind: WitnessKind::Handle {
                effect,
                body: Box::new(body_w),
                handler: Box::new(handler_w),
                k_param: "resume".to_string(),
            },
            span,
        })
    }

    /// v0.80: 花括号包裹的 block 解析器（区别于 emit_block_w 的 `end` 终止）。
    /// 用于 handle 块的 body/handler —— consume 直到匹配的 `}`。
    /// 后续 parser_state.token_at() 应在调用前已被 consume `{`。
    fn emit_brace_block_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        let mut stmt_wits = Vec::new();
        // 花括号块：跳过换行（与 emit_block_w 的 if 分支一致）
        while self.match_token(&[TokenType::Newline]) {}
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            let w = self.emit_statement_expr_w()?;
            stmt_wits.push(w);
            while self.match_token(&[TokenType::Newline]) {}
        }
        Some(Self::block_witness(stmt_wits, span))
    }

    fn emit_macro_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'macro'
        let name = self.consume_identifier("Expected macro name")?;
        let mut params = Vec::new();
        if self.match_token_exact(TokenType::LParen) {
            while !self.check(&TokenType::RParen) && !self.is_at_end() {
                // v0.104.6 D354：macro 参数是**声明位**，走严格版（同闭包）。
                if let Some(p) = self.consume_plain_identifier("Expected parameter name") {
                    params.push(p);
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(TokenType::RParen, "Expected ')' after params")?;
        }
        // 子上下文：宏体是独立作用域（`is_program_top = false`, `parse_loop_depth = 0`）
        let body_w = self.in_nested_scope(|this| {
            if this.match_token_exact(TokenType::Newline) {
                let mut stmt_wits = Vec::new();
                while this.match_token(&[TokenType::Newline]) {}
                while !this.check(&TokenType::End) && !this.is_at_end() {
                    let w = this.emit_statement_expr_w()?;
                    stmt_wits.push(w);
                    while this.match_token(&[TokenType::Newline]) {}
                }
                this.consume(TokenType::End, "Expected 'end' after macro body")?;
                Some(Self::block_witness(stmt_wits, span))
            } else {
                this.consume(TokenType::End, "Expected 'end' after macro body")?;
                Some(MirWitness {
                    kind: WitnessKind::Sequence(Vec::new()),
                    span,
                })
            }
        })?;
        Some(MirWitness {
            kind: WitnessKind::MacroDef {
                name,
                params,
                body: Box::new(body_w),
            },
            span,
        })
    }

    fn emit_if_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'if'
        let cond_w = self.emit_expr_w()?;
        // v0.75.79: if 结果走**值传递**（块参数）—— 不再经 env 临时名
        // `__if_result`（Assign 写未定义变量静默失败，分支值丢失）。
        // lower 侧把各分支汇入同一个 merge 块，选中分支的值作为块参数。

        // v0.92: 三种分支语法统一收敛：
        //   1. `then expr`        — then 关键字分支（单表达式）
        //   2. `{ stmts }`        — 现有花括号块
        //   3. `\n ... end`       — 顶层无括号块（向后兼容）
        // v0.104.2: `then` 之后是**块**时也接受 —— spec §14.2 EBNF
        //（`if_stmt = "if" expr "then" { statement } … "end"`）与 §3.2/§7.3/
        // §11.5 的示例一律写作
        //     if condition then
        //       …
        //     end
        // 即「`then` + 换行 + 语句块 + `end`」。此前 `then` 分支无条件走
        // `emit_expr_w`（只吃一个表达式），换行立刻让解析失败 →
        // 规范里 4 处 `if … then` 示例全部不可解析，而花括号形式可用 ——
        // 同一语句的两种拼写一边通一边不通。
        // 判据：`then` 后紧跟换行或 `{` → 块体；否则单表达式。
        let has_then_kw = self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Then))
            .unwrap_or(false);

        let then_w = if has_then_kw {
            self.advance(); // consume 'then'
            let then_is_block = matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Newline) | Some(TokenType::LBrace)
            );

            if then_is_block {
                self.emit_block_w()?
            } else {
                // v0.104.2: `then <stmt>` 的单语句形态（spec §7.1 L83
                // `if cond then "a" else "b" end`、§7.3 L412/413
                // `if i == 3 then continue end`）。走**语句**分派而非表达式
                // 分派 —— `continue`/`break`/`return` 是语句，`emit_expr_w`
                // 会把它们当标识符解析（"Failed to parse"）。
                // 尾随 `end` **由本分支消费**，否则留给外层块解析器当语句起始。
                let w = self.emit_statement_expr_w()?;
                self.match_token_exact(TokenType::End);
                w
            }
        } else {
            self.emit_block_w()?
        };
        let else_w = if self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "else"))
            .unwrap_or(false)
        {
            self.advance();
            // v0.104.2: `else if …` —— 链式条件分支（spec §14.2 if_stmt 的
            // `{ "else" "if" expr "then" … }` 形态，§7.3 亦使用）。
            //
            // **缺陷**：`else` 之后无条件走 `emit_block_w`，而它只认
            // 块起始（`{` / 换行到 `end`）—— 遇到作为**表达式**的 `if`
            // 会落到"把 if 当语句"的路径并最终解析失败；于是
            //   `if c1 { a } else if c2 { b }`（值形态）**不可解析**，
            // 而 `if c1 { a } else { b }` 可解析 —— 同一语法的两级深浅
            // 一边通一边不通。
            // 现在：`else if` 递归走 `emit_if_w`（它同样返回嵌套 witness，
            // 与块体路径共用同一契约）。
            let else_is_if = self.check(&TokenType::If);
            let w = if else_is_if {
                self.emit_if_w()?
            } else {
                self.emit_block_w()?
            };
            Some(Box::new(w))
        } else {
            None
        };
        let w = MirWitness {
            kind: WitnessKind::If {
                cond: Box::new(cond_w),
                then: Box::new(then_w),
                r#else: else_w,
            },
            span,
        };
        Some(w)
    }

    /// D118：闭包体**与 `(`…`)` 同行**、且首 token 是**表达式起点**时，
    /// 应按 spec §14.2 的 `closure = "fn" "(" params ")" expr` 走单表达式解析。
    ///
    /// 两个条件都必要：
    /// - **同行**（`)` 与体首 token 之间无 Newline）—— 换行块
    ///   `fn()\n  print(1)\n  print(2)\nend` 的首 token 也是 Identifier，
    ///   只看白名单会把它误降成「只执行第一条语句」。
    /// - **白名单**而非黑名单：`return` / `let` / `if` / `for` / `match` 等
    ///   语句起始不是表达式（`return` 也不在 `emit_expr_w` 里）。白名单漏一个
    ///   表达式起点只会退回原路径（行为不变），黑名单漏一个则会让语句被
    ///   当表达式解析而报错 —— 失败方向相反。
    ///
    /// `handle` / `perform` / `assign` 是 `Identifier` 却是语句式分发，
    /// 显式排除以保持原路径。
    fn inline_closure_body_is_expr(&self) -> bool {
        // 同判据：前一个 token 必须是刚消费掉的 `)`，中间没有 Newline
        let prev_is_rparen = self.current > 0
            && matches!(
                self.tokens.get(self.current - 1).map(|t| &t.token_type),
                Some(TokenType::RParen)
            );
        if !prev_is_rparen {
            return false;
        }
        match self.peek().map(|t| &t.token_type) {
            Some(TokenType::Identifier(s)) => {
                !matches!(s.as_str(), "handle" | "perform" | "assign")
            }
            Some(
                TokenType::String(_)
                | TokenType::Char(_)
                | TokenType::Int(_)
                | TokenType::TypedInt(_, _)
                | TokenType::Float(_)
                | TokenType::TypedFloat(_, _)
                | TokenType::True
                | TokenType::False
                | TokenType::Nil
                | TokenType::LParen
                | TokenType::Minus
                | TokenType::Bang
                | TokenType::Backtick
                | TokenType::Quote
                | TokenType::Self_
                | TokenType::Dyn,
            ) => true,
            _ => false,
        }
    }

    /// emit 一个块（{} 或换行到 end），返回块 witness。
    /// 块 witness：单条语句取该语句的 witness；多条语句嵌套为 Sequence。
    pub(super) fn emit_block_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        let mut stmt_wits = Vec::new();
        if self.match_token_exact(TokenType::LBrace) {
            // v0.75.78: 块内语句间允许换行（`if c {\n  stmt\n}`）。修复前
            // `{` 后不跳换行，多行 brace 块解析失败（差分测试只覆盖单行 if，
            // 未暴露）。
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::RBrace) && !self.is_at_end() {
                let w = self.emit_statement_expr_w()?;
                stmt_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.consume(TokenType::RBrace, "Expected '}' after block")?;
        } else {
            // 换行到 end（v0.87：或 EOF / 右括号 / 逗号 终止）
            while self.match_token(&[TokenType::Newline]) {}
            let is_block_end = |p: &ParserV3| -> bool {
                match p.peek().map(|t| &t.token_type) {
                    Some(
                        TokenType::End
                        | TokenType::RParen
                        | TokenType::RBrace
                        | TokenType::Comma
                        | TokenType::EOF,
                    ) => true,
                    // v0.104.2: `else` 也是块终止符 ——
                    //     if c then
                    //       …
                    //     else          ← 必须在此停下
                    //       …
                    //     end
                    // 此前 `else` 不在终止集内，块体把它当**语句**解析
                    // → 「Unbound variable 'else'」。规范 §14.2 的 if_stmt
                    // 与 §7.3/§11.5 示例都是这个形状。
                    Some(TokenType::Identifier(s)) => s == "else",
                    None => true, // EOF 也是终止符
                    Some(_) => false,
                }
            };
            while !is_block_end(self) {
                let w = self.emit_statement_expr_w()?;
                stmt_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            // v0.87: end 可选——EOF 或右括号终止的闭包体（如 fn(x) x + 1）
            // 不要求显式 end；match_token_exact 在 token 不匹配时静默返回 false。
            //
            // v0.104.6 D118：省略 `end` 时**必须有终止符**。终止集（is_block_end）
            // 不含 Newline，于是同行的省略式闭包 `let f = fn() 1` 会把该行**之后
            // 的全部顶层语句**当成自己的块体吞掉 —— 程序静默地在那一行终止，
            // exit 0、零诊断、后续 print 全部消失（实测：只有 `A:start` 输出）。
            //
            // 「多语句 + 靠 EOF 收尾」几乎只可能是这个吞并：正常的省略式闭包
            // （`fn(x) x + 1` 作最后一行 / 作实参）只解析 1 条语句。
            // 这里把它从静默丢弃改成解析失败。
            let ended_by_end = self.match_token_exact(TokenType::End);
            if !ended_by_end && stmt_wits.len() > 1 && self.check(&TokenType::EOF) {
                return None;
            }
        }
        Some(Self::block_witness(stmt_wits, span))
    }

    /// 块/函数体的语句列表 → 单条则直出，多条嵌套为 Sequence。
    pub(super) fn block_witness(stmt_wits: Vec<MirWitness>, span: Span) -> MirWitness {
        if stmt_wits.len() == 1 {
            stmt_wits.into_iter().next().expect("len==1 verified above")
        } else if stmt_wits.is_empty() {
            MirWitness {
                kind: WitnessKind::Sequence(Vec::new()),
                span,
            }
        } else {
            MirWitness {
                kind: WitnessKind::Sequence(stmt_wits),
                span,
            }
        }
    }

    /// 块内语句 → witness。
    /// 嵌套语句级分发（Let 优先 > Match > If > For > While）。
    /// v0.75.78: 补齐 Let/If/Match/For/While 分发 — 修复前嵌套上下文（task 体、
    /// 闭包体、for 体）中这些构造直接落 emit_expr_w → 解析失败。
    /// v0.75.81: 事务家族（transaction/commit/rollback）+ eval 断言接入
    /// 语句分发（lexer 无对应关键字，经 peek_is_identifier 识别，try/while
    /// 先例）。
    pub(super) fn emit_statement_expr_w(&mut self) -> Option<MirWitness> {
        if self.check(&TokenType::Let) {
            return self.emit_let_w();
        }
        // v0.104: `assign <name> = <expr>`（spec §14.2 assign_stmt）—— 与
        // `while`/`transaction` 同属标识符分发（不占新 TokenType）。前瞻守卫：
        // 仅当 `assign` 后紧跟标识符时才拦截，保留 `assign(...)` 这类以
        // assign 为名的普通调用。
        if let Some(TokenType::Identifier(s)) = self.peek().map(|t| &t.token_type)
            && s == "assign"
            && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.token_type),
                Some(TokenType::Identifier(_))
            )
        {
            return self.emit_assign_w();
        }
        match self.peek()?.token_type.clone() {
            TokenType::Return | TokenType::Break | TokenType::Continue => {
                let w = self.emit_return_break_continue_w()?;
                Some(w)
            }
            TokenType::Match => self.emit_match_w(),
            TokenType::If => self.emit_if_w(),
            TokenType::For => self.emit_loop_w(),
            TokenType::Identifier(n) if n == "while" => self.emit_while_w(),
            TokenType::Identifier(n) if n == "transaction" => self.emit_transaction_w(),
            TokenType::Identifier(n) if n == "eval" => self.emit_eval_w(),
            TokenType::Identifier(n) if n == "aggregate" => self.emit_aggregate_w(),
            TokenType::Identifier(n) if n == "handle" => self.emit_handle_w(),
            TokenType::Identifier(n) if n == "perform" => self.emit_perform_w(),
            // v0.98: effect 签名声明（前瞻守卫同上）
            TokenType::Identifier(n)
                if n == "effect"
                    && matches!(
                        self.tokens.get(self.current + 1).map(|t| &t.token_type),
                        Some(TokenType::Identifier(_))
                    )
                    && matches!(
                        self.tokens.get(self.current + 2).map(|t| &t.token_type),
                        Some(TokenType::LParen)
                    ) =>
            {
                self.emit_effect_sig_w()
            }
            // v0.103: export（嵌套上下文）
            TokenType::Identifier(ref s) if s == "export" => self.emit_export_w(),
            // v0.103: 并行块 / worker（嵌套上下文）
            TokenType::Identifier(ref s) if s == "parallel" => self.emit_parallel_w(),
            TokenType::Identifier(ref s) if s == "worker" => self.emit_worker_w(),
            // Wave 2: Go-style concurrency primitives (`spawn` / `select`)
            TokenType::Identifier(ref s) if s == "spawn" && self.is_spawn_form() => {
                self.emit_spawn_w()
            }
            TokenType::Identifier(ref s) if s == "select" && self.is_select_form() => {
                self.emit_select_w()
            }
            // Wave 3 Stage 3C: `no_grad` autograd suppression block
            TokenType::Identifier(ref s) if s == "no_grad" && self.is_no_grad_form() => {
                self.emit_no_grad_w()
            }
            TokenType::Identifier(ref s) if s == "defer" && self.is_defer_form() => {
                self.emit_defer_w()
            }
            TokenType::Identifier(ref s) if s == "kernel" && self.is_kernel_form() => {
                self.emit_kernel_def_w()
            }
            TokenType::Identifier(n) if n == "commit" => {
                let span = self.span_of_current();
                self.advance(); // 'commit'
                // v0.104.2: `commit` 是 unit 语句 —— 见证层只带一个空变体，
                // `mir::control::lower` 降成无 dst 的 `ControlInst::Commit`。
                let w = MirWitness {
                    kind: WitnessKind::Commit,
                    span,
                };
                Some(w)
            }
            TokenType::Identifier(n) if n == "rollback" => {
                let span = self.span_of_current();
                self.advance(); // 'rollback'
                // 同 commit：降成无 dst 的 `ControlInst::Rollback`。
                let w = MirWitness {
                    kind: WitnessKind::Rollback,
                    span,
                };
                Some(w)
            }
            _ => self.emit_expr_w(),
        }
    }

    fn emit_loop_w(&mut self) -> Option<MirWitness> {
        // 'for' var 'in' iterable newline body 'end'（镜像 lower Loop）
        let span = self.span_of_current();
        self.advance(); // 'for'
        // v0.104.6 D354：循环变量是**声明位**（引入新名字），与 `let` 同类，
        // 走严格版。此前用 `consume_identifier` 会吃关键字兜底 ⇒
        // `for if in xs` exit 0 声明成功，而体内 `print(if)` 解析失败。
        let var = self.consume_plain_identifier("Expected loop variable")?;
        self.consume(TokenType::In, "Expected 'in' in for loop")?;
        let iter_w = self.emit_expr_w()?;

        // v0.104.6 D474：循环深度 +1，让体里的 `break`/`continue` 通过
        // 「是否在循环内」的解析期判据。深度记在 `ParserScope` 里 —— 嵌套的
        // 函数体（task / closure / …）深度自动归零。
        self.scope.parse_loop_depth += 1;
        let body_res = self.emit_block_w();
        self.scope.parse_loop_depth -= 1;
        let body_w = body_res?;
        let w = MirWitness {
            kind: WitnessKind::Loop {
                var,
                iterable: Box::new(iter_w),
                body: Box::new(body_w),
            },
            span,
        };
        Some(w)
    }

    fn emit_while_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'while'
        let cond_w = self.emit_expr_w()?;

        // v0.104.6 D474：同 `emit_loop_w` —— 循环深度 +1，体的 `break`/
        // `continue` 才被认作「在循环内」。
        self.scope.parse_loop_depth += 1;
        let body_res = self.emit_block_w();
        self.scope.parse_loop_depth -= 1;
        let body_w = body_res?;
        // v0.104.6 缺陷修复：同 emit_for_w —— 按作用域登记回填，不做全区间扫描。
        // while 无循环变量增量，故 `continue` 目标仍是 `loop_label`。

        let w = MirWitness {
            kind: WitnessKind::While {
                cond: Box::new(cond_w),
                body: Box::new(body_w),
            },
            span,
        };
        Some(w)
    }

    /// v0.75.81: 事务块（spec 9.3, Ballerina 启发）。
    ///
    /// ```mora
    /// transaction
    ///   <body 语句，可含 commit / rollback>
    /// [compensation
    ///   <补偿语句>]
    /// end
    /// ```
    ///
    /// 事务语义：body 独立成子函数，body 内 `rollback` 降成 `ControlInst::Rollback`
    /// → VM 抛 "Transaction rolled back" → 执行 compensation 后向上抛错；
    /// `commit` 降成 `ControlInst::Commit`（无操作）。body 终止于 `compensation`
    /// 或 `end`。witness = Sequence(body + compensation 语句)（无值语句，typeck 得 Nil）。
    fn emit_transaction_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'transaction'
        let body_wits = self.in_nested_scope(|this| {
            let mut wits = Vec::new();
            while this.match_token(&[TokenType::Newline]) {}
            while !this.check(&TokenType::End)
                && !this.peek_is_identifier("compensation")
                && !this.is_at_end()
            {
                let w = this.emit_statement_expr_w()?;
                wits.push(w);
                while this.match_token(&[TokenType::Newline]) {}
            }
            Some(wits)
        })?;

        // compensation 段（可选）：`compensation` 后语句循环到 `end`
        let comp_wits = if self.peek_is_identifier("compensation") {
            self.advance(); // 'compensation'
            self.in_nested_scope(|this| {
                let mut wits = Vec::new();
                while this.match_token(&[TokenType::Newline]) {}
                while !this.check(&TokenType::End) && !this.is_at_end() {
                    let w = this.emit_statement_expr_w()?;
                    wits.push(w);
                    while this.match_token(&[TokenType::Newline]) {}
                }
                Some(wits)
            })?
        } else {
            Vec::new()
        };
        self.consume(TokenType::End, "Expected 'end' after transaction")?;

        let w = MirWitness {
            kind: WitnessKind::Transaction {
                body: Box::new(Self::block_witness(body_wits, span)),
                compensation: Box::new(Self::block_witness(comp_wits, span)),
            },
            span,
        };
        Some(w)
    }

    /// v0.75.81: eval 断言语句（α.8 Eval 原语前端，v0.25 Agent 行为回归测试）。
    ///
    /// ```mora
    /// eval ["name"] given_expr, expect1, expect2, ...
    /// ```
    /// 首 token 为字符串字面量时作为断言名；given 与各 expect 为表达式。
    /// `mir::control::lower` 降成 `ControlInst::EvalAssert`：VM 用 `eval_assert`
    /// 把 given 与每个 expect 逐一比较（tolerance 未设），任一不等报错（断言失败）。
    /// 语句值为 given 表达式的值（见下方注释）。
    fn emit_eval_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'eval'
        let token = self.peek().cloned()?;
        let name = if let TokenType::String(s) = token.token_type {
            self.advance();
            s
        } else {
            String::new()
        };
        let given_w = self.emit_expr_w()?;
        let mut expect_wits = Vec::new();
        while self.match_token(&[TokenType::Comma]) {
            let w = self.emit_expr_w()?;
            expect_wits.push(w);
        }
        // v0.104.6 D472：① witness 携带 given / expects（此前被丢弃）——
        //    否则 lowering 无从生成 `ControlInst::EvalAssert` 的期望集。
        //
        // ② 语句的返回值 = **given 表达式的值**，**不是** Nil。
        //    ⚠ 这一条必须**逐字保留**：`eval("1")` 在表达式位被 eval 语句
        //    分派接住（spec §14.2 的 `eval_expr` 产生式），其值按 spec
        //    期望是 `String("1")` —— 即 **given 的值**。若改成「恒为 Nil」，
        //    该 spec 用例会得到 Nil。
        //    （同理 `aggregate`：值 = 被贡献表达式的值。）
        Some(MirWitness {
            kind: WitnessKind::Eval {
                name,
                given: Box::new(given_w),
                expects: expect_wits,
                tolerance: None,
                replay_path: None,
            },
            span,
        })
    }

    /// v0.75.83: aggregate 语句 — 向聚合器贡献值。
    ///
    /// ```mora
    /// aggregate name, value_expr
    /// ```
    /// name 为聚合器名，value_expr 为贡献值。见证层携带贡献值的表达式；
    /// `mir::control::lower` 直接求值该表达式并以其值为语句值
    /// （聚合器名当前不参与 lowering）。
    fn emit_aggregate_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'aggregate'
        let name = self.consume_identifier("Expected aggregator name after 'aggregate'")?;
        self.consume(TokenType::Comma, "Expected ',' after aggregator name")?;
        let value_w = self.emit_expr_w()?;
        // v0.104.6 D472：① witness 携带贡献值的表达式（此前被丢弃）；
        // ② 语句的返回值 = **贡献值的值**（与 eval 同形，逐字保留原语义）。
        Some(MirWitness {
            kind: WitnessKind::Aggregate {
                name,
                value: Box::new(value_w),
            },
            span,
        })
    }

    fn emit_match_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'match'
        let scrutinee_w = self.emit_expr_w()?;
        // v0.104.3: **两种 arm 体形态** ——
        //   1. spec §14.2 EBNF（`match_stmt = "match" expr "with"
        //      { pattern [ "when" expr ] "->" expr } "end"`）与 §7.3/§7.4/§7.5
        //      的模式匹配教学章节、§2.2/§12 示例共 8+ 处：`with … -> … end`
        //   2. 既有实现形态：`{ … => … }`（全部 fixtures 用此形态）
        //
        // **缺陷**：`with … -> … end` 从未被实现 —— `emit_match_w` 无条件
        // `consume(TokenType::LBrace)`，于是规范自己**整章**的模式匹配文档
        // （§7.3 模式匹配 / §7.4 守卫 / §7.5 列表 rest）逐字无法运行，
        // 报 "Expected '{' after match subject"。而 CHANGELOG 中**没有**
        // 该形态被移除的记录（对比 `route`：有明确的「已移除」声明与条目），
        // 因此属「承诺语法未接线」，与 `assign` 同类的真实缺陷。
        // 两种形态共用 arm emitter，仅体终止符（`end` / `}`）与 arm 分隔
        // （换行 / `,`）不同。
        let with_form = self.match_token_exact(TokenType::With);
        let mut arm_wits = Vec::new();
        if with_form {
            while !self.check(&TokenType::End) && !self.is_at_end() {
                // v0.104.6 D44b：**先**吃掉 arm 之间的换行，再尝试解析 arm。
                //
                // 换行清理此前写在 arm 尝试**之后**，于是循环第一次进入时 token
                // 必是 Newline → `parse_pattern` 失败 → 落到 `self.advance()`
                // 恢复分支。当时那个恢复分支是**承重的**（不是纯防御）：直接
                // 换成报错会让**所有** match 形式失效（实测 7 个形态全挂）。
                //
                // 风险**已由「清理前移」解除**，而不是靠保留恢复分支：现在
                // 进入 arm 解析时 token 必已是 arm 的第一个 token，恢复分支
                // 没有存在理由，故已按 D44b 换成显式报错（见下方 else）。
                //
                // ⚠ 不要把清理挪回 arm 尝试**之后**。「`valid_match_forms_
                // still_work` 一条测试撑着」曾是这个不变量的全部护栏；现在
                // `tests::d44b_all_valid_match_forms_still_compile` 就近钉住
                // 它，重排会让那条测试立刻红。
                while self.match_token(&[TokenType::Newline]) {}
                if self.check(&TokenType::End) || self.is_at_end() {
                    break;
                }
                if let Some(arm) = self.emit_match_arm_w() {
                    arm_wits.push(arm.witness);
                } else {
                    // 分隔符已吃完仍解析不出 arm —— 是**真的**畸形 arm，必须报错。
                    //
                    // 缺陷：此前 `self.advance()` 只为「不静默死循环」而前进一
                    // token，畸形 arm 于是**凭空消失**：
                    //
                    // ```mora
                    // task w()
                    //   match 1 with
                    //     1 -> 5
                    //     2 -> return 9   // arm 体只接受 expr（spec §14.2：
                    //     //                   pattern ["when" expr] "->" expr）
                    //   end
                    // end
                    // w()
                    // ```
                    //
                    // 修前 `w()` 返回 `nil`（好 arm 也失效，见 D44 的上下文
                    // 泄漏）；修好 D44 后返回 `1.0`（好 arm 恢复、坏 arm 仍被
                    // 吞）。两者都是**静默的错误答案 + exit 0**。
                    // `1 -> @@@` 这种纯垃圾更是被照单全收。
                    //
                    // 与 D1/D39/D42 同源的原则：**输入存在但无效 → 报错**。
                    eprintln!(
                        "Parse error: malformed match arm (line {}) — \
                         an arm body must be an expression: `pattern [when <expr>] \
                         -> <expr>`",
                        self.current_line()
                    );
                    return None;
                }
            }
            self.consume(TokenType::End, "Expected 'end' after match arms")?;
        } else {
            self.consume(
                TokenType::LBrace,
                "Expected '{' or 'with' after match subject",
            )?;
            while !self.check(&TokenType::RBrace) && !self.is_at_end() {
                // v0.104.6 D44b：同 `with` 形态 —— 先吃掉分隔符（换行/逗号）
                // 再解析 arm，否则畸形 arm 会被静默吞掉。
                while self.match_token(&[TokenType::Newline]) {}
                if self.check(&TokenType::RBrace) || self.is_at_end() {
                    break;
                }
                if let Some(arm) = self.emit_match_arm_w() {
                    arm_wits.push(arm.witness);
                    let _ = self.match_token(&[TokenType::Comma]);
                } else {
                    eprintln!(
                        "Parse error: malformed match arm (line {}) — \
                         an arm body must be an expression: `pattern [when <expr>] \
                         => <expr>`",
                        self.current_line()
                    );
                    return None;
                }
            }
            self.consume(TokenType::RBrace, "Expected '}' after match arms")?;
        }
        // match 的结果由**外层**的合并块承载：所有 arm 汇入同一个 merge 块，
        // 选中 arm 的值作为该块的块参数。见证层只需给出
        // `WitnessKind::Match { scrutinee, arms }`，块结构由
        // `mir::control::lower` 的 `lower_match` 生成。
        let w = MirWitness {
            kind: WitnessKind::Match {
                scrutinee: Box::new(scrutinee_w),
                arms: arm_wits,
            },
            span,
        };
        Some(w)
    }

    fn emit_match_arm_w(&mut self) -> Option<EmittedMatchArm> {
        let pattern = self.emit_pattern()?;
        // v0.104.3: arm 箭头有**两种拼写** —— `=>`（既有实现形态、全部
        // fixtures 使用）与 `->`（spec §14.2 EBNF 与 §7.3/§7.4/§7.5 教学章节
        // 一律使用）。lexer 早有 `TokenType::Arrow`，只是 match arm 从未接受它。
        let is_arrow = |t: &TokenType| matches!(t, TokenType::FatArrow | TokenType::Arrow);
        // v0.87: Detect optional "when <guard_expr>" before the arm arrow
        //
        // v0.104.3 修复：守卫与 arm body 同构 —— 都是**延迟求值**的子表达式，
        // 由 `mir::control::lower` 在**模式绑定之后**求值（`lower_match` 把
        // guard 放在 matched 块内）。
        //
        // 守卫此前在外层作用域求值，而守卫通常引用**模式绑定变量**
        //（`x when x > 0`）—— 绑定只发生在匹配成功之后。外层求值时该变量尚不
        // 存在 → 读到 Nil → `is_truthy(Nil) == false` → **该 arm 恒被跳过**，
        // `when` 完全失效。实测 `match -5i { x when x > 0i => "positive"
        // x when x < 0i => "negative" _ => "zero" }` 返回 "positive"；
        // fixture `match_guard.mora` 之所以"通过"只因取值 42/0 恰好让首守卫
        // 为真 —— 属侥幸。
        let guard = if let TokenType::Identifier(ref name) = self.peek()?.token_type
            && name == "when"
        {
            self.advance(); // consume "when"
            // 独立作用域（与 arm body 同规则）
            let guard_w = self.in_nested_scope(|this| this.emit_expr_w())?;
            if !self
                .peek()
                .map(|t| is_arrow(&t.token_type))
                .unwrap_or(false)
            {
                return None;
            }
            self.advance(); // consume '=>' 或 '->'
            Some(guard_w)
        } else {
            if !self
                .peek()
                .map(|t| is_arrow(&t.token_type))
                .unwrap_or(false)
            {
                return None;
            }
            self.advance(); // consume '=>' 或 '->'
            None
        };
        // 子上下文：arm body 是独立作用域（`is_program_top = false`, `parse_loop_depth = 0`）
        let body_w = self.in_nested_scope(|this| this.emit_expr_w())?;
        let witness = crate::mir::witness::WitnessArm {
            pattern,
            guard,
            body: body_w,
        };
        Some(EmittedMatchArm { witness })
    }

    /// v0.92: parse_pattern 返回 WitnessPattern（witness-native）。
    fn emit_pattern(&mut self) -> Option<crate::mir::witness::WitnessPattern> {
        // 复用完整 pattern 解析器（通配/字面量/变量/元组/列表/dict/类型标注）
        self.parse_pattern()
    }

    /// v0.85: `as dyn Trait` coercion（§3.5 spec 承诺）。
    /// 检测当前 token 序列为 `as dyn <TraitName>`，产出
    /// `WitnessKind::DynTrait` —— lower 侧据此把 plain value 包装为
    /// `Value::TraitObject`。未检测到则原样返回。
    fn emit_dyn_coercion(&mut self, src_w: MirWitness) -> Option<MirWitness> {
        if self.check(&TokenType::As) {
            self.advance(); // 'as'
            if self.check(&TokenType::Dyn) {
                self.advance(); // 'dyn'
                let trait_name = self.consume_identifier("Expected trait name after 'dyn'")?;
                let mut generics = Vec::new();
                if self.match_token(&[TokenType::Less]) {
                    loop {
                        let ty = self.parse_type_annotation()?;
                        generics.push(crate::mir::hint::TypeHint::from_type(ty));
                        if !self.match_token(&[TokenType::Comma]) {
                            break;
                        }
                    }
                    self.consume(TokenType::Greater, "Expected '>' after dyn trait generics")?;
                }
                let span = src_w.span;
                return Some(MirWitness {
                    kind: WitnessKind::DynTrait {
                        expr: Box::new(src_w),
                        trait_name,
                        generics,
                    },
                    span,
                });
            }
        }
        Some(src_w)
    }

    /// Shared parser for `end`-delimited nested block bodies (`parallel`, `worker`),
    /// enforcing token progress inside a nested `ParserScope`.
    fn emit_end_delimited_body_w(&mut self, block_kind: &str) -> Option<Vec<MirWitness>> {
        self.in_nested_scope(|this| {
            let mut body_wits = Vec::new();
            while !this.check(&TokenType::End) && !this.is_at_end() {
                let before = this.current;
                match this.emit_statement_expr_w() {
                    Some(w) => body_wits.push(w),
                    None => {
                        eprintln!(
                            "Parse error: unparsable statement in {block_kind} block (line {})",
                            this.current_line()
                        );
                        return None;
                    }
                }
                while this.match_token(&[TokenType::Newline]) {}
                if this.current == before {
                    eprintln!(
                        "Parse error: parser made no progress in {block_kind} block (line {})",
                        this.current_line()
                    );
                    return None;
                }
            }
            this.consume(
                TokenType::End,
                &format!("Expected 'end' after {block_kind} block"),
            )?;
            Some(body_wits)
        })
    }

    /// v0.103: `parallel ... end` / `worker <name> do ... end`（spec §9.1/§9.2）。
    ///
    /// `parallel` 块内各 `worker` 声明并发执行；`worker` 在块外等价于普通块。
    fn emit_parallel_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'parallel'
        let _ = self.match_token(&[TokenType::Newline]);
        let body_wits = self.emit_end_delimited_body_w("parallel")?;
        Some(MirWitness {
            kind: WitnessKind::Parallel {
                body: Box::new(Self::block_witness(body_wits, span)),
            },
            span,
        })
    }

    /// `worker <name> do ... end` —— 并发单元（含于 parallel 内时并发）。
    fn emit_worker_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'worker'
        let name = self.consume_identifier("Expected worker name after 'worker'")?;
        let _ = self.match_token_exact(TokenType::Do);
        let _ = self.match_token(&[TokenType::Newline]);
        let body_wits = self.emit_end_delimited_body_w("worker")?;
        Some(MirWitness {
            kind: WitnessKind::Worker {
                name,
                body: Box::new(Self::block_witness(body_wits, span)),
            },
            span,
        })
    }
    ///
    /// 解析内部声明后从其 witness 提取产生的绑定名，包一层
    /// `WitnessKind::Export`（typeck 侧据此计算导入可见性；lower 侧据此 emit
    /// `ControlInst::ExportGlobal` / `ControlInst::ExportDyn`，运行时把名字
    /// 写入环境导出集）。
    fn emit_export_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'export'
        // 内部声明：与 emit_statement_w 同分派（但不接受嵌套 export）
        let decl = match self.peek()?.token_type.clone() {
            TokenType::Task => self.emit_fn_def_w(),
            TokenType::Let => self.emit_let_w(),
            TokenType::Struct => self.emit_struct_def_w(),
            TokenType::Enum => self.emit_enum_def_w(),
            TokenType::Type => self.emit_type_alias_w(),
            TokenType::Macro => self.emit_macro_def_w(),
            TokenType::Identifier(ref s) if s == "trait" && self.is_trait_form() => {
                self.emit_trait_def_w()
            }
            TokenType::Identifier(ref s) if s == "impl" && self.is_impl_form() => {
                self.emit_impl_def_w()
            }
            TokenType::Identifier(ref s) if s == "kernel" && self.is_kernel_form() => {
                self.emit_kernel_def_w()
            }
            _ => return None,
        }?;
        let names = Self::exported_names_of(&decl);
        for _n in &names {}
        Some(MirWitness {
            kind: WitnessKind::Export {
                names,
                decl: Box::new(decl),
            },
            span,
        })
    }

    /// 从声明 witness 提取其产生的顶层绑定名（export 的标记对象）。
    /// 返回空 = 该声明没有可导出的名字（如裸表达式）。
    pub(super) fn exported_names_of(w: &MirWitness) -> Vec<String> {
        match &w.kind {
            WitnessKind::LetBinding { name, .. }
            | WitnessKind::FnDef { name, .. }
            | WitnessKind::StructDef { name, .. }
            | WitnessKind::EnumDef { name, .. }
            | WitnessKind::TraitDef { name, .. }
            | WitnessKind::TypeAlias { name, .. }
            | WitnessKind::MacroDef { name, .. }
            | WitnessKind::KernelDef { name, .. } => vec![name.clone()],
            // Sequence 可含多条声明（`export` 后跟块）—— 收集全部
            WitnessKind::Sequence(items) => {
                items.iter().flat_map(Self::exported_names_of).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Stage 3C: Contextual lookahead for `trait Name ... end` (preserves `let trait = 1`).
    fn is_trait_form(&self) -> bool {
        let Some(tok) = self.peek() else {
            return false;
        };
        if !matches!(&tok.token_type, TokenType::Identifier(s) if s == "trait") {
            return false;
        }
        matches!(
            self.tokens.get(self.current + 1).map(|t| &t.token_type),
            Some(TokenType::Identifier(_))
        )
    }

    /// Stage 3C: Contextual lookahead for `impl<T> ...` or `impl Name ... end`.
    fn is_impl_form(&self) -> bool {
        let Some(tok) = self.peek() else {
            return false;
        };
        if !matches!(&tok.token_type, TokenType::Identifier(s) if s == "impl") {
            return false;
        }
        matches!(
            self.tokens.get(self.current + 1).map(|t| &t.token_type),
            Some(TokenType::Identifier(_) | TokenType::Less)
        )
    }

    /// P4-1B: Contextual lookahead for `kernel name(params...) ... end` (preserves `let kernel = 1`).
    fn is_kernel_form(&self) -> bool {
        let Some(tok) = self.peek() else {
            return false;
        };
        if !matches!(&tok.token_type, TokenType::Identifier(s) if s == "kernel") {
            return false;
        }
        matches!(
            self.tokens.get(self.current + 1).map(|t| &t.token_type),
            Some(TokenType::Identifier(_))
        ) && matches!(
            self.tokens.get(self.current + 2).map(|t| &t.token_type),
            Some(TokenType::LParen)
        )
    }

    /// Stage 3C: Contextual lookahead for `no_grad ... end` / `no_grad do ... end` / `no_grad { ... }`.
    fn is_no_grad_form(&self) -> bool {
        let Some(tok) = self.peek() else {
            return false;
        };
        if !matches!(&tok.token_type, TokenType::Identifier(s) if s == "no_grad") {
            return false;
        }
        matches!(
            self.tokens.get(self.current + 1).map(|t| &t.token_type),
            Some(TokenType::Newline | TokenType::Do | TokenType::LBrace)
        )
    }

    /// Stage 3C: Parse `no_grad ... end` / `no_grad do ... end` / `no_grad { ... }`.
    fn emit_no_grad_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // consume 'no_grad'
        let body = if self.check(&TokenType::LBrace) {
            self.in_nested_scope(|this| this.emit_block_w())?
        } else {
            let _ = self.match_token_exact(TokenType::Do);
            while self.match_token(&[TokenType::Newline]) {}
            let stmts = self.emit_end_delimited_body_w("no_grad")?;
            Self::block_witness(stmts, span)
        };
        Some(MirWitness {
            kind: WitnessKind::NoGrad {
                body: Box::new(body),
            },
            span,
        })
    }

    /// G2: Contextual lookahead for `defer <stmt/expr>` / `defer do ... end` / `defer { ... }`.
    fn is_defer_form(&self) -> bool {
        let Some(tok) = self.peek() else {
            return false;
        };
        if !matches!(&tok.token_type, TokenType::Identifier(s) if s == "defer") {
            return false;
        }
        let Some(next) = self.tokens.get(self.current + 1) else {
            return false;
        };
        !matches!(
            next.token_type,
            TokenType::Assign
                | TokenType::LParen
                | TokenType::RParen
                | TokenType::RBracket
                | TokenType::RBrace
                | TokenType::Comma
                | TokenType::Dot
                | TokenType::Colon
                | TokenType::Newline
                | TokenType::End
                | TokenType::Plus
                | TokenType::Minus
                | TokenType::Star
                | TokenType::StarStar
                | TokenType::Slash
                | TokenType::Percent
                | TokenType::At
                | TokenType::Equal
                | TokenType::NotEqual
                | TokenType::Less
                | TokenType::LessEqual
                | TokenType::Greater
                | TokenType::GreaterEqual
                | TokenType::Pipe
                | TokenType::Question
                | TokenType::EOF
        )
    }

    /// G2: Parse `defer do ... end`, `defer { ... }`, or `defer <statement/expr>`.
    fn emit_defer_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // consume 'defer'
        let body = if self.check(&TokenType::LBrace) {
            self.in_nested_scope(|this| this.emit_block_w())?
        } else if self.match_token_exact(TokenType::Do) {
            while self.match_token(&[TokenType::Newline]) {}
            let stmts = self.emit_end_delimited_body_w("defer")?;
            Self::block_witness(stmts, span)
        } else {
            self.emit_statement_expr_w()?
        };
        Some(MirWitness {
            kind: WitnessKind::Defer {
                body: Box::new(body),
            },
            span,
        })
    }

    fn is_spawn_form(&self) -> bool {
        let Some(tok) = self.peek() else {
            return false;
        };
        if !matches!(&tok.token_type, TokenType::Identifier(s) if s == "spawn") {
            return false;
        }
        let Some(next) = self.tokens.get(self.current + 1) else {
            return false;
        };
        !matches!(
            next.token_type,
            TokenType::Assign
                | TokenType::Dot
                | TokenType::Comma
                | TokenType::RParen
                | TokenType::RBracket
                | TokenType::RBrace
                | TokenType::Newline
                | TokenType::EOF
                | TokenType::Plus
                | TokenType::Minus
                | TokenType::Star
                | TokenType::StarStar
                | TokenType::Slash
                | TokenType::Percent
                | TokenType::Equal
                | TokenType::NotEqual
                | TokenType::Greater
                | TokenType::Less
                | TokenType::GreaterEqual
                | TokenType::LessEqual
                | TokenType::Arrow
                | TokenType::FatArrow
                | TokenType::Colon
        )
    }

    fn is_select_form(&self) -> bool {
        let Some(tok) = self.peek() else {
            return false;
        };
        if !matches!(&tok.token_type, TokenType::Identifier(s) if s == "select") {
            return false;
        }
        let mut idx = self.current + 1;
        while matches!(
            self.tokens.get(idx).map(|t| &t.token_type),
            Some(TokenType::Newline)
        ) {
            idx += 1;
        }
        matches!(
            self.tokens.get(idx).map(|t| &t.token_type),
            Some(TokenType::LBrace | TokenType::Identifier(_) | TokenType::LParen)
        )
    }

    fn emit_spawn_w(&mut self) -> Option<MirWitness> {
        use crate::mir::witness::WitnessSpawnTarget;
        let span = self.span_of_current();
        self.advance(); // consume 'spawn'

        let target = if self.match_token_exact(TokenType::Do) {
            let _ = self.match_token(&[TokenType::Newline]);
            let body_wits = self.emit_end_delimited_body_w("spawn")?;
            let closure = MirWitness {
                kind: WitnessKind::Closure {
                    type_params: Vec::new(),
                    params: Vec::new(),
                    body: Box::new(Self::block_witness(body_wits, span)),
                },
                span,
            };
            WitnessSpawnTarget::Expr(Box::new(closure))
        } else if self.check(&TokenType::LBrace) {
            let body = self.in_nested_scope(|this| this.emit_block_w())?;
            let closure = MirWitness {
                kind: WitnessKind::Closure {
                    type_params: Vec::new(),
                    params: Vec::new(),
                    body: Box::new(body),
                },
                span,
            };
            WitnessSpawnTarget::Expr(Box::new(closure))
        } else {
            let mut expr = self.emit_expr_w()?;
            let kind = std::mem::replace(&mut expr.kind, WitnessKind::Commit);
            match kind {
                WitnessKind::Call {
                    callee,
                    args,
                    named_args,
                } => WitnessSpawnTarget::Call {
                    callee,
                    args,
                    named_args,
                },
                WitnessKind::MethodCall {
                    receiver,
                    method,
                    args,
                    named_args,
                } => WitnessSpawnTarget::MethodCall {
                    receiver,
                    method,
                    args,
                    named_args,
                },
                other => WitnessSpawnTarget::Expr(Box::new(MirWitness {
                    kind: other,
                    span: expr.span,
                })),
            }
        };

        Some(MirWitness {
            kind: WitnessKind::Spawn { target },
            span,
        })
    }

    fn emit_select_w(&mut self) -> Option<MirWitness> {
        use crate::mir::witness::WitnessSelectArm;
        let span = self.span_of_current();
        self.advance(); // consume 'select'
        while self.match_token(&[TokenType::Newline]) {}
        let brace_mode = self.match_token_exact(TokenType::LBrace);
        let is_close = |p: &ParserV3| -> bool {
            if brace_mode {
                p.check(&TokenType::RBrace)
            } else {
                p.check(&TokenType::End)
            }
        };

        let mut arms = Vec::new();
        while !is_close(self) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline, TokenType::Comma]) {}
            if is_close(self) || self.is_at_end() {
                break;
            }
            if matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Identifier(s)) if s == "case"
            ) {
                self.advance();
            }

            let is_arrow = |t: &TokenType| matches!(t, TokenType::Arrow | TokenType::FatArrow);
            let arm = if matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Identifier(s)) if s == "default"
            ) && self
                .tokens
                .get(self.current + 1)
                .map(|t| is_arrow(&t.token_type))
                .unwrap_or(false)
            {
                self.advance(); // 'default'
                self.advance(); // '->' or '=>'
                let body = self.emit_select_arm_body()?;
                WitnessSelectArm::Default {
                    body: Box::new(body),
                }
            } else if matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Identifier(s)) if s == "timeout"
            ) && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.token_type),
                Some(TokenType::LParen)
            ) {
                self.advance(); // 'timeout'
                self.consume(TokenType::LParen, "Expected '(' after timeout")?;
                let ms = self.emit_expr_w()?;
                self.consume(TokenType::RParen, "Expected ')' after timeout duration")?;
                if !self
                    .peek()
                    .map(|t| is_arrow(&t.token_type))
                    .unwrap_or(false)
                {
                    eprintln!(
                        "Parse error: expected '->' or '=>' after timeout(...) in select (line {})",
                        self.current_line()
                    );
                    return None;
                }
                self.advance(); // '->' or '=>'
                let body = self.emit_select_arm_body()?;
                WitnessSelectArm::Timeout {
                    ms: Box::new(ms),
                    body: Box::new(body),
                }
            } else if matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Identifier(_))
            ) && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.token_type),
                Some(TokenType::Assign)
            ) {
                let binding = self.consume_plain_identifier("Expected variable name")?;
                self.advance(); // '='
                let mut op_expr = self.emit_expr_w()?;
                let kind = std::mem::replace(&mut op_expr.kind, WitnessKind::Commit);
                let WitnessKind::MethodCall {
                    receiver,
                    method,
                    args,
                    named_args,
                } = kind
                else {
                    eprintln!(
                        "Parse error: expected '<channel>.recv()' after '=' in select arm (line {})",
                        self.current_line()
                    );
                    return None;
                };
                if method != "recv" || !args.is_empty() || !named_args.is_empty() {
                    eprintln!(
                        "Parse error: select binding arm must call '.recv()' with no arguments (line {})",
                        self.current_line()
                    );
                    return None;
                }
                if !self
                    .peek()
                    .map(|t| is_arrow(&t.token_type))
                    .unwrap_or(false)
                {
                    eprintln!(
                        "Parse error: expected '->' or '=>' after recv() in select arm (line {})",
                        self.current_line()
                    );
                    return None;
                }
                self.advance(); // '->' or '=>'
                let body = self.emit_select_arm_body()?;
                WitnessSelectArm::Recv {
                    binding: Some(binding),
                    channel: receiver,
                    body: Box::new(body),
                }
            } else {
                let mut op_expr = self.emit_expr_w()?;
                if !self
                    .peek()
                    .map(|t| is_arrow(&t.token_type))
                    .unwrap_or(false)
                {
                    eprintln!(
                        "Parse error: expected '->' or '=>' after select case condition (line {})",
                        self.current_line()
                    );
                    return None;
                }
                self.advance(); // '->' or '=>'
                let kind = std::mem::replace(&mut op_expr.kind, WitnessKind::Commit);
                match kind {
                    WitnessKind::MethodCall {
                        receiver,
                        method,
                        mut args,
                        named_args,
                    } if method == "send" && args.len() == 1 && named_args.is_empty() => {
                        let value = Box::new(args.remove(0));
                        let body = self.emit_select_arm_body()?;
                        WitnessSelectArm::Send {
                            channel: receiver,
                            value,
                            body: Box::new(body),
                        }
                    }
                    WitnessKind::MethodCall {
                        receiver,
                        method,
                        args,
                        named_args,
                    } if method == "recv" && args.is_empty() && named_args.is_empty() => {
                        let body = self.emit_select_arm_body()?;
                        WitnessSelectArm::Recv {
                            binding: None,
                            channel: receiver,
                            body: Box::new(body),
                        }
                    }
                    _ => {
                        eprintln!(
                            "Parse error: invalid select arm condition (expected ch.recv(), ch.send(v), timeout(ms), or default) at line {}",
                            self.current_line()
                        );
                        return None;
                    }
                }
            };
            arms.push(arm);
            let _ = self.match_token(&[TokenType::Comma]);
        }

        if brace_mode {
            self.consume(TokenType::RBrace, "Expected '}' after select block")?;
        } else {
            self.consume(TokenType::End, "Expected 'end' after select block")?;
        }

        Some(MirWitness {
            kind: WitnessKind::Select { arms },
            span,
        })
    }

    fn emit_select_arm_body(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.in_nested_scope(|this| {
            if this.match_token_exact(TokenType::Do) {
                let _ = this.match_token(&[TokenType::Newline]);
                let body_wits = this.emit_end_delimited_body_w("select arm")?;
                Some(Self::block_witness(body_wits, span))
            } else {
                this.emit_expr_w()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::parser_v3::ParserV3;

    fn compiles(src: &str) -> bool {
        ParserV3::compile(src).is_ok()
    }

    /// D44b 不变量的**就近**护栏。
    ///
    /// 这条不变量此前只有 `tests/parser_recovery.rs::valid_match_forms_still_work`
    /// 一条**集成**测试守着，而它离出问题的地方隔着一个 `tests/` 目录。改动
    /// `emit_match_w` 的人不会先看到它。
    ///
    /// 形态覆盖：`with … end` 与 `{ … }` 两族、各 arm 数、arm 体是字面量 /
    /// 返回值、带守卫 / 不带守卫。任一形态退化都会红。
    ///
    /// 关键失效方向：若有人把换行清理挪回 arm 尝试**之后**，循环首次进入时
    /// token 是 Newline → 首个 arm 解析失败 → 现在会走 else 的显式报错 →
    /// 编译被拒 ⇒ 本测试红。
    #[test]
    fn d44b_all_valid_match_forms_still_compile() {
        let cases: &[(&str, &str)] = &[
            ("with / 1 arm", "let v = match 1 with\n  1 -> 5\nend\nv\n"),
            (
                "with / 2 arms",
                "let v = match 1 with\n  1 -> 5\n  2 -> 9\nend\nv\n",
            ),
            (
                "with / guard",
                "let v = match 1 with\n  x when x > 0 -> 5\n  _ -> 0\nend\nv\n",
            ),
            (
                "with / task 内",
                "task w()\n  match 1 with\n    1 -> 5\n  end\nend\nw()\n",
            ),
            ("brace / 1 arm", "let v = match 1 {\n  1 -> 5\n}\nv\n"),
            (
                "brace / 2 arms",
                "let v = match 1 {\n  1 -> 5,\n  _ -> 0,\n}\nv\n",
            ),
            (
                "brace / task 内",
                "task w()\n  match 1 {\n    1 -> 5\n  }\nend\nw()\n",
            ),
        ];
        let mut failures = Vec::new();
        for (name, src) in cases {
            if !compiles(src) {
                failures.push(*name);
            }
        }
        assert!(
            failures.is_empty(),
            "这些**合法** match 形态必须照常编译通过。\
             典型成因是把换行清理挪回 arm 尝试之后（首个 arm 前是 Newline → \
             解析失败 → 走显式报错分支）。失效形态：{failures:?}"
        );
    }

    /// D44b 的另一半：畸形 arm 必须**编译期报错**，不得被静默吞掉。
    ///
    /// 与上一条构成一对：上条钉住「合法的别误伤」，本条钉住「非法的别放过」。
    /// 若有人把显式报错换回 `self.advance()` 恢复分支（畸形 arm 凭空消失），
    /// 本测试立刻红 —— 那正是 D44b 修掉的静默错答案。
    ///
    /// 畸形 arm 触发的是 `eprintln!` 诊断，故本测试输出里会有 stderr。
    #[test]
    fn d44b_malformed_match_arm_is_still_rejected() {
        let cases: &[(&str, &str)] = &[
            (
                "arm 体是 return 而非表达式",
                "task w()\n  match 1 with\n    1 -> 5\n    2 -> return 9\n  end\nend\nw()\n",
            ),
            (
                "纯垃圾 arm",
                "task w()\n  match 1 with\n    1 -> 5\n    @@@\n  end\nend\nw()\n",
            ),
        ];
        let mut failures = Vec::new();
        for (name, src) in cases {
            if compiles(src) {
                failures.push(*name);
            }
        }
        assert!(
            failures.is_empty(),
            "畸形 match arm 必须编译期报错。修前它们被 `self.advance()` \
             静默吞掉、退出码 0，还连带让**合法** arm 失效（返回 nil）。\
             如今编译通过说明恢复分支被请回来了：{failures:?}"
        );
    }
}
