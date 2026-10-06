use super::*;

impl ParserV3 {
    /// v0.75.40: 顶层语句循环 — emit 每条语句 + 顶层 witness。
    pub(super) fn emit_program(&mut self) -> Result<(), String> {
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
                ))
                .map(|_: ParseError| ())
                .map_err(|e| e.0);
            }
            match self.emit_statement_w() {
                Some(w) => self.witnesses.push(w),
                None => {
                    // v0.104.6 D30：词法错误 token 带着精确病因（超范围字面量、
                    // 非法 bigint、未闭合字符串…）。解析器只把它当陌生物 token，
                    // 于是这些信息全被丢弃、只报一句 "Failed to parse at line N"。
                    // 先回头找词法层的真实诊断再决定报什么。
                    return Err(match self.lexical_error_ahead() {
                        Some((msg, line, col)) => {
                            format!("{} at line {}, column {}", msg, line, col)
                        }
                        None => format!("Failed to parse at line {}", self.current_line()),
                    });
                }
            }
        }
        Ok(())
    }

    // ===================================================================
    // v0.75.40: emit 家族 — 单遍编译（表达式 → MirInst + MirWitness）
    // ===================================================================
    // 镜像 parse 链的优先级结构，但在构造处直接 emit 指令 + 建 witness。
    // 指令序列与 lower.rs 逐字节等价（差分测试锁定）。每函数返回结果
    // 寄存器 Reg（表达式）或 ()（语句）。

    /// 顶层语句分发 — 镜像 parse_expression_statement。
    /// 顶层语句 → witness（嵌套树，emit 时直接构建）。
    fn emit_statement_w(&mut self) -> Option<MirWitness> {
        match self.peek()?.token_type {
            TokenType::Task => self.emit_fn_def_w(),
            TokenType::Let => self.emit_let_w(),
            TokenType::Match => self.emit_match_w().map(|(_, w)| w),
            TokenType::If => self.emit_if_w().map(|(_, w)| w),
            TokenType::For => self.emit_loop_w().map(|(_, w)| w),
            TokenType::Identifier(ref s) if s == "while" => self.emit_while_w().map(|(_, w)| w),
            // v0.88: TEA app 块
            TokenType::App => self.emit_app_def_w(),
            // v0.103: 命名 section 声明
            TokenType::Prompt => self.emit_section_w(true),
            TokenType::Document => self.emit_section_w(false),
            // v0.103: export（标识符分派，spec §10.2）
            TokenType::Identifier(ref s) if s == "export" => self.emit_export_w(),
            // v0.103: TEA 独立声明 `model Name ... end` / `msg Name ... end`
            //（spec §9.6 / §14.2 EBNF）。前瞻守卫：仅 `model`/`msg` 后紧跟
            // 标识符时拦截（不抢以 model/msg 命名的变量/调用语句）。
            TokenType::Identifier(ref s)
                if (s == "model" || s == "msg")
                    && matches!(
                        self.tokens.get(self.current + 1).map(|t| &t.token_type),
                        Some(TokenType::Identifier(_))
                    ) =>
            {
                if s == "model" {
                    self.emit_model_def_w()
                } else {
                    self.emit_msg_def_w()
                }
            }
            // v0.103: TEA 独立 `update(params) ... end`（spec §9.6 工作示例）。
            //
            // 守卫：`update` 后紧跟 `(` 且括号内**全为裸标识符**（形参表形态）。
            // 普通调用 `update(a, b)` 与声明 `update(msg, model)` 在这一位置上
            // 形似，故再要求「`)` 后是同语句体（换行/`{`/表达式）」不足以区分；
            // 本守卫以「形参只能是标识符」为界 —— `update(x + 1, y)` 这类
            // 含表达式的调用不受影响，而恰好「全部实参都是裸标识符」的语句级
            // 调用会被解析为声明。这与 `model`/`msg` 的守卫同一取舍：为落地
            // 规范承诺的声明形式，保留一个边界明确的名字。
            // （接收者调用 `x.update(...)` 不受影响 —— 本分派只在语句首 token
            // 是 `update` 时触发。）
            TokenType::Identifier(ref s) if s == "update" && self.looks_like_update_decl() => {
                self.emit_update_def_w()
            }
            // v0.103: 可观测性块
            TokenType::Identifier(ref s) if s == "observe" => self.emit_observe_w(),
            TokenType::Identifier(ref s) if s == "span" => self.emit_span_w(),
            // v0.103: 并行块 / worker
            TokenType::Identifier(ref s) if s == "parallel" => self.emit_parallel_w(),
            TokenType::Identifier(ref s) if s == "worker" => self.emit_worker_w(),
            // v0.102: 声明式范式（逻辑式/关系式）
            TokenType::Rel => self.emit_rel_def_w(),
            TokenType::Solve => self.emit_solve_w().map(|(_, w)| w),
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
                self.emit_statement_expr_w().map(|(_, w)| w)
            }
            // v0.80: algebraic effects 完整语法（Stage 2/4 落地）
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
            TokenType::Orchestrate => self.emit_orchestrate_w(),
            // v0.85: `with mock_llm = [...]` 配置桥接块（§19.4 spec 承诺）。
            TokenType::With => self.emit_with_w(),
            _ => {
                // 表达式语句（赋值/字面量/调用等）
                self.emit_expr_w().map(|(_, w)| w)
            }
        }
    }

    /// 表达式入口（witness 嵌套版）— 镜像 parse_assignment（含赋值检测）。
    pub(super) fn emit_expr_w(&mut self) -> Option<(Reg, MirWitness)> {
        // v0.104.2: `if` 出现在**表达式位置**（spec §7.1 的核心示例）
        //     let x = if cond then "a" else "b" end
        // 以及 §7.3 的 `if i == 3 then continue end` 值形态。
        // `emit_if_w` 返回 (结果寄存器, witness)，与其它表达式产出同契约。
        //
        // 注：此前这条接线被「合并点永不就绪」的执行器缺陷阻塞（`if` 之后的
        // 语句会静默消失、退出码 0）；该缺陷已随 `dag.rs` 的
        // 「entry 取 pc 0 可达节点」修复而消除（常量折叠后的 else 死块不再
        // 与函数入口并列成为入口），故此处可以安全接线。
        if self.check(&TokenType::If) {
            return self.emit_if_w();
        }
        // v0.80: algebraic effects 表达式位置 dispatch（Stage 2.0）。
        // 表达式位置出现的 `handle` / `perform` 不是变量名 —— 走专门 emission。
        if let Some(TokenType::Identifier(s)) = self.peek().map(|t| &t.token_type).cloned() {
            if s == "handle" {
                return self.emit_handle_w().map(|w| (0, w));
            }
            if s == "perform" {
                return self.emit_perform_w().map(|w| (0, w));
            }
        }
        if matches!(self.peek()?.token_type, TokenType::Identifier(_)) {
            let ident_start = self.current;
            let name = self.consume_identifier("Expected variable name")?;

            if self.match_token(&[TokenType::Assign]) {
                let (v, v_w) = self.emit_expr_w()?;
                let span = self.span_of_current();
                self.emit.emit(MirInst::Assign(name.clone(), v));
                let w = MirWitness {
                    kind: WitnessKind::Assign {
                        target: name,
                        value: Box::new(v_w),
                    },
                    span,
                };
                return Some((v, w));
            }
            self.current = ident_start;
        }
        let (reg, w) = self.emit_or_w()?;
        // v0.85: `as dyn Trait` coercion（§3.5 spec 承诺）— emit 路径中
        // 显式将 expression 包装为 Value::TraitObject。
        self.emit_dyn_coercion(reg, w)
    }

    fn emit_or_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_and_w()?;
        while self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "or"))
            .unwrap_or(false)
        {
            self.advance();
            let (right, right_w) = self.emit_and_w()?;
            let dst = self.emit.alloc_reg();
            let l = left;
            self.emit.emit(MirInst::JumpIf(l, 0));
            let jump_idx = self.emit.insts.len() - 1;
            let r = right;
            self.emit
                .emit(MirInst::BinaryOp(dst, l, BinaryOp::NotEqual, r));
            let end = self.emit.insts.len();
            self.emit.patch_label_at(jump_idx, end);
            let span = self.span_of_current();
            left_w = MirWitness {
                kind: WitnessKind::Or {
                    left: Box::new(left_w),
                    right: Box::new(right_w),
                },
                span,
            };
            left = dst;
        }
        Some((left, left_w))
    }

    fn emit_and_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_equality_w()?;
        while self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "and"))
            .unwrap_or(false)
        {
            self.advance();
            let (right, right_w) = self.emit_equality_w()?;
            let dst = self.emit.alloc_reg();
            let l = left;
            self.emit.emit(MirInst::JumpIfNot(l, 0));
            let jump_idx = self.emit.insts.len() - 1;
            let r = right;
            self.emit
                .emit(MirInst::BinaryOp(dst, l, BinaryOp::Equal, r));
            let end = self.emit.insts.len();
            self.emit.patch_label_at(jump_idx, end);
            let span = self.span_of_current();
            left_w = MirWitness {
                kind: WitnessKind::And {
                    left: Box::new(left_w),
                    right: Box::new(right_w),
                },
                span,
            };
            left = dst;
        }
        Some((left, left_w))
    }

    fn emit_equality_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_pipe_w()?;
        while let Some(op) = self.consume_binary_op(&[TokenType::Equal, TokenType::NotEqual]) {
            let (right, right_w) = self.emit_pipe_w()?;
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::BinaryOp(dst, left, op.clone(), right));
            let span = self.span_of_current();
            left_w = MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(left_w),
                    op,
                    right: Box::new(right_w),
                },
                span,
            };
            left = dst;
        }
        Some((left, left_w))
    }

    /// 管道 `|>` —— 两种形态（spec §7.6 / §18.1 / §18.2）：
    ///
    /// - `x |> f`        裸标识符 → **值应用** `f(x)`（§7.6 `5 |> double`）。
    /// - `x |> f(args)`  调用形态 → **方法调用** `x.f(args)`。
    ///
    /// **缺陷（v0.104.2 修复）**：`x |> f(args)` 此前 witness 与 MIR 语义分叉 ——
    /// witness 脱糖为 `Call{f, [x, ...args]}`（看起来是「f 接收 x 作为首参」），
    /// 但 MIR 发的是 `Pipe(new_dst, x, rhs)`，而 **rhs 是 RHS 表达式已被求值的
    /// 寄存器** —— 求值 `f(args)` 本身就是一次「自由函数调用」，对
    /// `map`/`filter`/`upper`/`split`/`route`/`tool`/`serve`/`listen` 这些
    /// **只有方法形态**的名字必然失败（"Undefined function or task: map"），
    /// 于是 spec 里 6 处管道示例全部不可用：
    ///   §2.1  `[1,2,3] |> map(fn(x) x*2 end)`
    ///   §7.6  `"hello world" |> upper() |> split(" ") |> map(...) |> filter(...)`
    ///   §18.1 `router |> route("POST", …) |> listen(…)`
    ///   §18.2 `server |> tool(…) |> serve()`
    /// 规范对这六处的形态高度一致：`f(args)` 一律是**接收者的方法**。
    /// 现在按此实现 —— 就地把握手前刚发出的 `Call(dst, f, argv)` 改写为
    /// `MethodCall(dst, x, f, argv)`，witness 同步为 `MethodCall`。
    fn emit_pipe_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_comparison_w()?;
        loop {
            // D119：允许 `|>` **跨行**。spec 的管道示例一律是跨行形态 ——
            //   §2.1  `[1,2,3] |> map(fn(x) x*2 end)`
            //   §7.6  `let result = "hello world"` 换行 `|> upper()` 换行 `|> split(" ")` …
            //   §18.1 `router` 换行 `|> route("POST", …)` 换行 `|> listen(…)`
            //   §18.2 `server` 换行 `|> tool(…)` 换行 `|> serve()`
            // 修复前换行一出现循环就退出、`|>` 残留，**整段报 parse error**。
            //
            // ⚠ 跳过的换行在**没看到 `|>` 时必须回退**：下一行若是普通语句
            //   （`let a = 1` 换行 `print(6)`），已吞掉的换行就是语句分隔符，
            //   不回退会把它并进前一条语句。`self.current` 是下标，直接复位即可。
            let saved = self.current;
            while self.match_token(&[TokenType::Newline]) {}
            if !self.match_token_exact(TokenType::Pipe) {
                self.current = saved;
                break;
            }
            // 记录 RHS 的发射起点，供「就地改写为方法调用」精确定位
            let mark = self.emit.insts.len();
            let (rhs, rhs_w) = self.emit_comparison_w()?;
            let span = self.span_of_current();
            left_w = match rhs_w.kind {
                // ── `x |> f(args)` → 方法调用 `x.f(args)` ──
                WitnessKind::Call {
                    callee: crate::mir::witness::WitnessCallee::Name(method),
                    args,
                } => {
                    // 把本次 RHS 发射区间内、以 rhs 为目标寄存器的 Call 改写为
                    // MethodCall（接收者 = 管道左侧）。寄存器有唯一生产者，
                    // 区间又限定为「本次 RHS」，故定位精确。
                    let mut patched = false;
                    for inst in self.emit.insts[mark..].iter_mut() {
                        if let MirInst::Call(d, name, argv) = inst
                            && *d == rhs
                            && *name == method
                        {
                            let argv = argv.clone();
                            *inst = MirInst::MethodCall(rhs, left, method.clone(), argv);
                            patched = true;
                            break;
                        }
                    }
                    if !patched {
                        // 形状不符预期（如 `x |> f(1).g(2)` 的 RHS 结果来自
                        // 尾链的最后一个调用）—— 退回值应用语义，保持旧行为。
                        let dst = self.emit.alloc_reg();
                        self.emit.emit(MirInst::Pipe(dst, left, rhs));
                        left = dst;
                    } else {
                        left = rhs;
                    }
                    MirWitness {
                        kind: WitnessKind::MethodCall {
                            receiver: Box::new(left_w),
                            method,
                            args,
                        },
                        span,
                    }
                }
                // ── `x |> f`（裸标识符）→ 值应用 `f(x)` ──
                WitnessKind::Variable(name) => {
                    let dst = self.emit.alloc_reg();
                    self.emit.emit(MirInst::Pipe(dst, left, rhs));
                    left = dst;
                    MirWitness {
                        kind: WitnessKind::Call {
                            callee: crate::mir::witness::WitnessCallee::Name(name),
                            args: vec![left_w],
                        },
                        span,
                    }
                }
                // ── 其它 → 值应用（把 RHS 的值当可调用对象）──
                other => {
                    let dst = self.emit.alloc_reg();
                    self.emit.emit(MirInst::Pipe(dst, left, rhs));
                    left = dst;
                    MirWitness {
                        kind: WitnessKind::Call {
                            callee: crate::mir::witness::WitnessCallee::Name("|>".to_string()),
                            args: vec![left_w, MirWitness { kind: other, span }],
                        },
                        span,
                    }
                }
            };
        }
        Some((left, left_w))
    }

    fn emit_comparison_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_term_w()?;
        while let Some(op) = self.consume_binary_op(&[
            TokenType::Less,
            TokenType::Greater,
            TokenType::LessEqual,
            TokenType::GreaterEqual,
        ]) {
            let (right, right_w) = self.emit_term_w()?;
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::BinaryOp(dst, left, op.clone(), right));
            let span = self.span_of_current();
            left_w = MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(left_w),
                    op,
                    right: Box::new(right_w),
                },
                span,
            };
            left = dst;
        }
        Some((left, left_w))
    }

    fn emit_term_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_factor_w()?;
        while let Some(op) = self.consume_binary_op(&[TokenType::Minus, TokenType::Plus]) {
            let (right, right_w) = self.emit_factor_w()?;
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::BinaryOp(dst, left, op.clone(), right));
            let span = self.span_of_current();
            left_w = MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(left_w),
                    op,
                    right: Box::new(right_w),
                },
                span,
            };
            left = dst;
        }
        Some((left, left_w))
    }

    fn emit_factor_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_unary_w()?;
        while let Some(op) =
            self.consume_binary_op(&[TokenType::Star, TokenType::Slash, TokenType::Percent])
        {
            let (right, right_w) = self.emit_unary_w()?;
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::BinaryOp(dst, left, op.clone(), right));
            let span = self.span_of_current();
            left_w = MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(left_w),
                    op,
                    right: Box::new(right_w),
                },
                span,
            };
            left = dst;
        }
        Some((left, left_w))
    }

    fn emit_unary_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();

        // 'not' keyword → 0 == x
        if self
            .peek()
            .map(|t| matches!(&t.token_type, TokenType::Identifier(s) if s == "not"))
            .unwrap_or(false)
        {
            self.advance();
            let (operand, operand_w) = self.emit_unary_w()?;
            let zero = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::Const(zero, crate::value::Value::Int(0)));
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::BinaryOp(dst, zero, BinaryOp::Equal, operand));
            let w = MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(MirWitness {
                        kind: WitnessKind::Literal(Literal::Int(0, span)),
                        span,
                    }),
                    op: BinaryOp::Equal,
                    right: Box::new(operand_w),
                },
                span,
            };
            return Some((dst, w));
        }

        // Unary minus / bang
        if self.match_token(&[TokenType::Minus, TokenType::Bang]) {
            // 镜像 parse_unary：-x → 0 - x；!x → 0 == x（truthiness）
            let (operand, operand_w) = self.emit_unary_w()?;
            let zero = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::Const(zero, crate::value::Value::Int(0)));
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::BinaryOp(dst, zero, BinaryOp::Sub, operand));
            let w = MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(MirWitness {
                        kind: WitnessKind::Literal(Literal::Int(0, span)),
                        span,
                    }),
                    op: BinaryOp::Sub,
                    right: Box::new(operand_w),
                },
                span,
            };
            return Some((dst, w));
        }

        let (r, w) = self.emit_call_w()?;
        Some((r, w))
    }

    /// 调用链（witness 嵌套版）— 镜像 parse_call（函数/方法/索引/DynTrait）。
    fn emit_call_w(&mut self) -> Option<(Reg, MirWitness)> {
        // v0.103: `Type::new(args)` 关联函数调用 —— CLI 帮助与 spec 承诺
        // `Router::new()` / `McpServer::new()` / `Trait::new()`，而 `::` 此前
        // 只被词法化为 TokenType::ColonColon、parser 从不消费（`::` 零使用点）。
        // 编译为按名调用 "Type::new"（运行时 method_dispatch 以该键查表）。
        if let TokenType::Identifier(type_name) = self.peek()?.token_type.clone() {
            let save = self.current;
            let span = self.span_of_current();
            self.advance();
            if self.match_token_exact(TokenType::ColonColon) {
                let assoc = self.consume_identifier("Expected associated function after '::'")?;
                let full = format!("{}::{}", type_name, assoc);
                let (args, arg_wits) = if self.match_token_exact(TokenType::LParen) {
                    self.emit_arg_list_w()?
                } else {
                    (Vec::new(), Vec::new())
                };
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::Call(dst, full.clone(), args));
                let w = MirWitness {
                    kind: WitnessKind::Call {
                        callee: crate::mir::witness::WitnessCallee::Name(full),
                        args: arg_wits,
                    },
                    span,
                };
                return self.emit_call_tail_w(dst, w);
            }
            self.current = save;
        }
        // 函数名调用：`name(args)` — 与 lower Call 一致（不 emit Var 加载）。
        if let TokenType::Identifier(name) = self.peek()?.token_type.clone() {
            let save = self.current;
            let span = self.span_of_current();
            self.advance();
            if self.match_token_exact(TokenType::LParen) {
                let (args, arg_wits) = self.emit_arg_list_w()?;
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::Call(dst, name.clone(), args));
                let w = MirWitness {
                    kind: WitnessKind::Call {
                        callee: crate::mir::witness::WitnessCallee::Name(name),
                        args: arg_wits,
                    },
                    span,
                };
                // 后缀链（方法/索引）可能改写 witness
                return self.emit_call_tail_w(dst, w);
            }
            self.current = save; // 非调用，回退走 primary
        }

        let (callee_reg, callee_w) = self.emit_primary_w()?;
        self.emit_call_tail_w(callee_reg, callee_w)
    }

    /// 后缀链（witness 嵌套版）：方法调用 obj.m(args) / 索引 obj[idx]。
    fn emit_call_tail_w(
        &mut self,
        mut callee_reg: Reg,
        mut callee_w: MirWitness,
    ) -> Option<(Reg, MirWitness)> {
        loop {
            if self.match_token_exact(TokenType::Dot) {
                let method_name = self.consume_identifier("Expected method name")?;
                let span = self.span_of_current();
                let mut args = Vec::new();
                let mut arg_wits = Vec::new();
                if self.match_token_exact(TokenType::LParen) {
                    let (a, aw) = self.emit_arg_list_w()?;
                    args = a;
                    arg_wits = aw;
                }
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::MethodCall(
                    dst,
                    callee_reg,
                    method_name.clone(),
                    args,
                ));
                callee_reg = dst;
                callee_w = MirWitness {
                    kind: WitnessKind::MethodCall {
                        receiver: Box::new(callee_w),
                        method: method_name,
                        args: arg_wits,
                    },
                    span,
                };
            } else if self.match_token_exact(TokenType::LBracket) {
                // Indexing: obj[idx]
                let span = self.span_of_current();
                let (idx, idx_w) = self.emit_expr_w()?;
                self.consume(TokenType::RBracket, "Expected ']' after index")?;
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::Index(dst, callee_reg, idx));
                callee_reg = dst;
                callee_w = MirWitness {
                    kind: WitnessKind::Call {
                        callee: crate::mir::witness::WitnessCallee::Name("[]".to_string()),
                        args: vec![callee_w, idx_w],
                    },
                    span,
                };
            } else if self.check(&TokenType::LParen) {
                // D123：值**不支持**被调用的后缀链 `f(…)(…)`，但**绝不能静默**。
                //
                // 本函数的后缀链只认 `.`（方法）与 `[`（索引），没有 `(` 分支。
                // 修复前 `let mk = fn(x) fn(y) x + y end end` + `mk(1)(2)` 的行为是：
                // `mk(1)` 正常求值并**丢弃**返回的闭包，紧随其后的 `(2)` 被外层
                // 当成一个**独立的括号表达式**语句求值 —— 末值得到 `2.0` 而不是
                // `1 + 2 = 3.0`，**退出码 0、零诊断**。
                // 同一个式子放进实参位（`print(mk(1)(2))`）则报 `Expected ')'` ——
                // 同一个表达式两种行为，静默的那个更糟。
                //
                // spec §14.2 的 EBNF 里 `call` 的被调用者只能是 `IDENTIFIER`，
                // **无 `postfix` 产生式**，故本形态属未承诺的能力缺口
                // （已记入 CHANGELOG D123）。此处只负责**消除静默**：
                // 从此明确解析失败，而不是算出一个错误的值。
                eprintln!("Parse error: 对表达式的结果再次调用（`f(…)(…)`）尚不支持");
                return None;
            } else {
                break;
            }
        }
        Some((callee_reg, callee_w))
    }

    fn emit_arg_list_w(&mut self) -> Option<(Vec<Reg>, Vec<MirWitness>)> {
        let mut args = Vec::new();
        let mut wits = Vec::new();
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            // v0.87: 参数解析失败必须传播（禁止静默丢弃失败的参数，否则
            // fn(x) x + 1 作为参数时会被吞掉，导致零参数调用）。
            let (r, w) = self.emit_expr_w()?;
            args.push(r);
            wits.push(w);
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RParen, "Expected ')'")?;
        Some((args, wits))
    }

    /// 主表达式 — 镜像 parse_primary。
    /// 主表达式 — 镜像 parse_primary。返回 (结果寄存器, 嵌套 witness)。
    /// v0.75.41: witness 递归构建（子节点嵌进父节点，typeck/LSP 消费树形）。
    fn emit_primary_w(&mut self) -> Option<(Reg, MirWitness)> {
        let token = self.peek().cloned()?;
        let span = crate::common::Span::new(token.line, token.column);

        let (reg, wit) = match token.token_type {
            // v0.102: solve 作为表达式（`let r = solve { ... }`）
            TokenType::Solve => return self.emit_solve_w(),
            TokenType::Int(val) => {
                self.advance();
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Int(val)));
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::Int(val, span)),
                    span,
                };
                (dst, w)
            }
            TokenType::Float(val) => {
                self.advance();
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Float(val)));
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::Float(val, span)),
                    span,
                };
                (dst, w)
            }
            // v0.91: BigInt 字面量
            TokenType::BigInt(val) => {
                self.advance();
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::Const(
                    dst,
                    crate::value::Value::BigInt(val.clone()),
                ));
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::BigInt(val, span)),
                    span,
                };
                (dst, w)
            }
            // v0.104.6 D15：char 字面量 `'a'`。
            //
            // **spec 两处明确定义了它**（`docs/mora-spec.md:109` 的类型表
            // `| char | 'a' |`，以及 EBNF `literal = NUMBER | … | CHAR | …`
            // 见 :1270），词法器也**早已支持**（`lexer.rs:357` 的 `'`
            // 分支 → `TokenType::Char(ch)`），`parser_v3/rel.rs:533` 的声明式
            // term 解析**也接了** —— 但主表达式发射器这里唯独漏了，于是
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
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Char(c)));
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::Char(c, span)),
                    span,
                };
                (dst, w)
            }
            TokenType::String(ref s) => {
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::String(s.clone())));
                self.advance();
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::String(s.clone(), span)),
                    span,
                };
                (dst, w)
            }
            TokenType::PromptString(ref s) => {
                self.advance();
                // p"..." 拆分为 parts 再 emit Prompt（parts 是纯树，非 token 流）。
                // v0.92: parse_prompt_parts 直接返回 MirWitness。
                let parts = parse_prompt_parts(s, span);
                let mut part_regs = Vec::new();
                let mut part_wits = Vec::new();
                for part in parts {
                    let r = match &part.kind {
                        WitnessKind::Literal(Literal::String(text, _)) => {
                            let dst = self.emit.alloc_reg();
                            self.emit.emit(MirInst::Const(
                                dst,
                                crate::value::Value::String(text.clone()),
                            ));
                            dst
                        }
                        WitnessKind::Variable(name) => {
                            let dst = self.emit.alloc_reg();
                            self.emit.emit(MirInst::Var(dst, name.clone()));
                            dst
                        }
                        _ => self.emit.alloc_reg(),
                    };
                    part_regs.push(r);
                    part_wits.push(part);
                }
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::Prompt(dst, part_regs));
                let w = MirWitness {
                    kind: WitnessKind::Prompt { parts: part_wits },
                    span,
                };
                (dst, w)
            }
            TokenType::True => {
                self.advance();
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Bool(true)));
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::Bool(true, span)),
                    span,
                };
                (dst, w)
            }
            TokenType::False => {
                self.advance();
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Bool(false)));
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::Bool(false, span)),
                    span,
                };
                (dst, w)
            }
            TokenType::Nil => {
                self.advance();
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Nil));
                let w = MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(span)),
                    span,
                };
                (dst, w)
            }
            TokenType::Identifier(name) => {
                self.advance();
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::Var(dst, name.clone()));
                let w = MirWitness {
                    kind: WitnessKind::Variable(name),
                    span,
                };
                (dst, w)
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
                let (inner, w) = self.emit_expr_w()?;
                self.consume(TokenType::RParen, "Expected ')'")?;
                (inner, w)
            }
            TokenType::Fn => {
                self.advance();
                if !self.match_token_exact(TokenType::LParen) {
                    return None;
                }
                let mut params = Vec::new();
                while !self.check(&TokenType::RParen) && !self.is_at_end() {
                    // v0.104.6 D354：闭包参数是**声明位**，走严格版。
                    // `fn(if) … end` 修前 exit 0 声明成功，但参数**永远引用不到**。
                    if let Some(p) = self.consume_plain_identifier("Expected parameter name") {
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
                // 子上下文：闭包体是独立寄存器空间（镜像 lower Closure 分支）
                let parent =
                    std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
                // v0.75.78: 非 FatArrow 闭包体走 emit_block_w（镜像 parse 侧
                // parse_block_body）— 支持多语句与嵌套构造（if/for/match/let）。
                // 修复前用 emit_expr_w：`fn(n) if n<=1 {..} else {..} end` 解析失败。
                // emit_block_w 已消费 End，无需外部 consume。
                // v0.90.3: 真实 body_w 进入 witness（此前是占位空 Sequence —
                // witness 树无法重建闭包代码，9 层管线（witness→FCFG）断裂）。
                // v0.104.6 D118：spec §14.2 的 `closure` 产生式是
                //     closure = "fn" "(" params ")" ( expr | "{" … "}" ) ;
                // —— **没有 `end`**，体就是单个表达式（§1.2 表里的示例也写作
                // `fn(x) x + 1 end`，两种都得认）。
                //
                // 修复前只有 `=>` 走表达式路径；`fn(x) x + 1`（无 FatArrow）落到
                // emit_block_w，而块终止集（is_block_end）不含 Newline 且 `end`
                // 可选（v0.87 为支持行尾/实参位置而允许省略），于是它把**后续
                // 全部顶层语句**当成自己的块体吞掉：`let g = fn(a) a + 1` +
                // `print(g(1))` 编译通过、exit 0，但 print 被移进闭包体，
                // 顶层零输出。
                //
                // 修法：`)` 与体首 token **同行**、且首 token 是表达式起点时，
                // 直接按单表达式解析（只吃一个表达式，天然不会吞后续语句）。
                // 其余情况（`return`/`let`/`if`/换行块/花括号块）保持原路径 ——
                // `return` 不在 emit_expr_w 里，走表达式路径会让 `fn() return 1 end` 回归。
                let (body_reg, body_w) = if self.match_token_exact(TokenType::FatArrow)
                    || self.inline_closure_body_is_expr()
                {
                    let (r, w) = self.emit_expr_w()?;
                    // 可选的显式 `end`：§14.2 的产生式没有 `end`，但 §1.2 的表格
                    // 与全仓既有代码写的是 `fn(x) x * 2 end`。expr 路径不经过
                    // emit_block_w，消费 `end` 的那一行也在那里 —— 不补这句，
                    // `end` 会残留成未解析 token（实测 `let f = fn(x) x * 2 end`
                    // 变成 "Failed to parse at line 1"）。两种拼法都要认。
                    self.match_token_exact(TokenType::End);
                    (r, w)
                } else {
                    self.emit_block_w()?
                };
                self.emit.emit_tail_return(Some(body_reg));
                let body_mir = std::mem::replace(&mut self.emit, parent).finish();
                let dst = self.emit.alloc_reg();
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let param_wits: Vec<crate::mir::witness::WitnessParam> = params
                    .iter()
                    .map(|p| crate::mir::witness::WitnessParam {
                        name: p.name.clone(),
                        type_hint: p
                            .type_hint
                            .clone()
                            .map(crate::mir::hint::TypeHint::from_type),
                        default: None,
                    })
                    .collect();
                self.emit.emit(MirInst::Closure {
                    dst,
                    params: param_names,
                    body: Box::new(body_mir),
                });
                let w = MirWitness {
                    kind: WitnessKind::Closure {
                        params: param_wits,
                        body: Box::new(body_w),
                    },
                    span,
                };
                (dst, w)
            }
            // v0.86: Lisp homoiconicity — `quote(expr)` 在解析期提取 expr 源码文本，
            // 以 Value::String 传入 builtin quote()，quote() 返回 Value::Code。
            TokenType::Quote => return self.emit_quote_w(),
            // v0.88: `expr` — Lisp quasiquote（反引号 + unquote/unquote-splice）。
            // 与 quote(expr) 对称：quote 冻结整个源码；quasiquote 选择性冻结。
            TokenType::Backtick => return self.emit_quasiquote_w(),
            _ => return None,
        };
        Some((reg, wit))
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
    fn emit_quote_w(&mut self) -> Option<(Reg, MirWitness)> {
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
        let str_reg = self.emit.alloc_reg();
        self.emit.emit(MirInst::Const(
            str_reg,
            crate::value::Value::String(quoted_text.to_string()),
        ));
        // emit: Call(dst_reg, "quote", [str_reg])
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Call(dst, "quote".to_string(), vec![str_reg]));

        let w = MirWitness {
            kind: WitnessKind::Call {
                callee: crate::mir::witness::WitnessCallee::Name("quote".to_string()),
                args: vec![MirWitness {
                    kind: WitnessKind::Literal(Literal::String(quoted_text.to_string(), span)),
                    span,
                }],
            },
            span,
        };
        Some((dst, w))
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
    ///   8. emit `MirInst::Quasiquote { dst, segments }`
    ///
    /// 运行时 h_quasiquote 遍历 segments：Quote 直接拼接，Unquote 取寄存器值
    /// 经 Mora Display 格式化，UnquoteSplice 取 List 展平为逗号分隔代码。
    /// 最终 dst 写入 Value::Code(重组源码字符串)，与 quote(expr) 返回类型一致。
    fn emit_quasiquote_w(&mut self) -> Option<(Reg, MirWitness)> {
        let start_span = self.span_of_current();
        self.consume(TokenType::Backtick, "Expected '`' for quasiquote")?;

        let mut capture_start = match self.peek() {
            Some(t) => self.source_byte_at(t.line, t.column),
            None => self.source.len(),
        };

        let mut segments: Vec<crate::mir::QuasiquoteSegment> = Vec::new();
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
                                segments
                                    .push(crate::mir::QuasiquoteSegment::Quote(text.to_string()));
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
                        if let Some((reg, w)) = self.emit_expr_w() {
                            if is_splice {
                                segments.push(crate::mir::QuasiquoteSegment::UnquoteSplice(reg));
                                // v0.104.6 D278：把 splice 标记**写进 witness**。
                                //
                                // 修前这里只 `witness_segments.push(w)` —— 标记只进了
                                // `segments`（emit.rs 自己用），witness 里**一个字节都没留**。
                                // ⇒ witness 层的 splice 段与普通 unquote 段**完全不可区分**，
                                // `witness_to_fcfg` 只能按 Unquote 处理（把值 stringify），
                                // 9 层路径把 `` `,,xs `` 渲染成 `List([1.0, 2.0, 3.0])`
                                // 而 emit.rs 路径给 `1, 2, 3` —— 且**差分判它通过**。
                                //
                                // 标记形态沿用 `witness_to_fcfg.rs` 里**早已写好**的
                                // 消费约定（`Literal(Boolean(true))` 独占一段）。
                                let mark_span = self.span_of_current();
                                witness_segments.push(MirWitness {
                                    kind: WitnessKind::Literal(Literal::Bool(true, mark_span)),
                                    span: mark_span,
                                });
                            } else {
                                segments.push(crate::mir::QuasiquoteSegment::Unquote(reg));
                            }
                            witness_segments.push(w);
                            if let Some(next) = self.peek() {
                                capture_start = self.source_byte_at(next.line, next.column);
                            }
                        } else {
                            return None;
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
                    segments.push(crate::mir::QuasiquoteSegment::Quote(text.to_string()));
                    witness_segments.push(MirWitness {
                        kind: WitnessKind::Literal(Literal::String(text.to_string(), start_span)),
                        span: start_span,
                    });
                }
            }
        }

        let dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::Quasiquote { dst, segments });

        let w = MirWitness {
            kind: WitnessKind::Quasiquote {
                segments: witness_segments,
            },
            span: start_span,
        };
        Some((dst, w))
    }

    fn emit_list_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.consume(TokenType::LBracket, "Expected '['")?;
        let mut items = Vec::new();
        let mut item_wits = Vec::new();
        while !self.check(&TokenType::RBracket) && !self.is_at_end() {
            if let Some((item, wit)) = self.emit_expr_w() {
                items.push(item);
                item_wits.push(wit);
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RBracket, "Expected ']'")?;
        let dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::ListLit(dst, items));
        let w = MirWitness {
            kind: WitnessKind::List(item_wits),
            span,
        };
        Some((dst, w))
    }

    fn emit_dict_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.consume(TokenType::LBrace, "Expected '{'")?;
        let mut entries = Vec::new();
        let mut entry_wits = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            if let Some(key) = self.emit_dict_key() {
                self.consume(TokenType::Colon, "Expected ':' after dict key")?;
                if let Some((value, wit)) = self.emit_expr_w() {
                    entries.push((key.clone(), value));
                    entry_wits.push((key, wit));
                }
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RBrace, "Expected '}'")?;
        let dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::DictLit(dst, entries));
        let w = MirWitness {
            kind: WitnessKind::Dict(entry_wits),
            span,
        };
        Some((dst, w))
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
        self.emit.emit(MirInst::Import(path.clone()));
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::Import(path),
            span,
        })
    }

    /// v0.80: perform Effect(arg1, arg2, ...) 解析。
    ///
    /// syntax: `perform EffectName(args)?` — args 是 `emit_expr_w` 列表。
    /// 不引入新 TokenType（按 Identifier 路径分发，避免抢旧 identifier）。
    /// Stage 2.x 升级：未生效 perform 编译期 typeck 拦截（Stage 2.3 row-poly HM）。
    fn emit_perform_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'perform'
        let effect = self.consume_identifier("Expected effect name after 'perform'")?;
        let mut args: Vec<MirWitness> = Vec::new();
        if self.match_token_exact(TokenType::LParen) {
            if !self.check(&TokenType::RParen) {
                loop {
                    let (_, w) = self.emit_expr_w()?;
                    args.push(w);
                    if !self.match_token(&[TokenType::Comma]) {
                        break;
                    }
                }
            }
            self.consume(TokenType::RParen, "Expected ')' after perform args")?;
        }
        Some(MirWitness {
            kind: WitnessKind::Perform { effect, args },
            span,
        })
    }

    /// v0.80: handle Effect { body } { handler } 完整解析 + 嵌套 EmitContext 切换。
    ///
    /// syntax: `handle Effect { body_stmts } { handler_stmts }`（花括号形式）。
    /// body 与 handler 各自独立 EmitContext（独立寄存器空间），与 TaskDef 一致。
    /// 不引入新 TokenType（按 Identifier 路径分发）。
    /// Stage 2.x 升级：handler 可使用 `resume k resume-value` 续名续 + 标 typing。
    ///
    /// 返回 `(k_dst, witness)`：**`k_dst` 就是 `MirInst::Handle` 写入、
    /// `h_handle` 填值、且 `let` 绑定应当引用的那个寄存器**。
    ///
    /// v0.104.6 D35：此前本函数只返回 witness，表达式位置只能硬编码
    /// `0` 当结果寄存器（见 `emit_expr_w` 的 `map(|w| (0, w))`），
    /// 于是 `let r = handle …` 编译成 `Define("r", 0)` —— 绑的是一个
    /// **从来没人写过的**寄存器 0。DAG 执行器的就绪门槛
    /// `node_ready(Define) = reg_ready[0]` 恒 false → Define 永不激活 →
    /// 它所在的 **Sequence 链**从此断裂 → 其后**所有** Effect 节点
    /// （含 `print`）静默消失，无报错、退出码 0。而 `handle` 作**语句**
    /// 时正常，因为那时没人引用那个返回值。
    ///
    /// v0.104.6 D35/D36（**未修，如实记录**）：本函数只返回 witness，
    /// 表达式位置因此硬编码 `(0, w)` 当结果寄存器。单遍路径下这会让
    /// `let r = handle …` 的绑定指向错误/无人生产的寄存器（D36）；
    /// 9 层路径不受影响（`witness_to_fcfg` 另行分配并写入 `Node::Handle`
    /// 的 `dst`，见 D35 的修复）。
    ///
    /// **曾试过让本函数返回 `(k_dst, witness)` 并把真实 k_dst 交出去 ——
    /// 已回退**：那样单遍路径的单层 handle 确实修好了，但 emit.rs 产出的
    /// 指令序列随之变化（内层 handle 的指令不再被回滚截掉，多出一条），
    /// 与 9 层管线的 `differential_check` 从「通过」变成「失败」
    /// （实测 `pipeline_mir=14 original_mir=15`，`inst[1]: pipeline="Define"
    /// original="Handle"`）→ 生产路径回落到 emit.rs，而回落路径的**嵌套**
    /// handle 仍然坏 —— 即**把原本正常的 9 层嵌套 handle 弄坏了**。
    /// 净负面，故回退。要真正修 D36 得先让两条路径在嵌套 handle 上产出
    /// 一致的指令序列，那是 D36 的前置工作。
    fn emit_handle_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'handle'
        let effect = self.consume_identifier("Expected effect name after 'handle'")?;
        // v0.90: 死代码平铺回滚 — brace block 解析经 emit_statement_expr_w
        // 会把 body/handler 指令平铺进主 EmitContext，但 Handle 携带的是
        // 嵌套 MirFunction（下方 lower_block_witness_to_mir 独立 lower）。
        // 平铺副本是死代码（每 handle 浪费 body+handler 条指令，且 handler
        // 平铺会引用未绑定的 __arg0）。解析后回滚，只保留 witness 子树。
        let saved_len = self.emit.insts.len();
        let saved_next_reg = self.emit.next_reg;
        // body 块（必须花括号形式）
        self.consume(TokenType::LBrace, "Expected '{' after effect name")?;
        let (_, body_w) = self.emit_brace_block_w()?;
        self.consume(TokenType::RBrace, "Expected '}' after handle body")?;
        // handler 块（必须花括号形式）
        self.consume(TokenType::LBrace, "Expected '{' for handler block")?;
        let (_, handler_w) = self.emit_brace_block_w()?;
        self.consume(TokenType::RBrace, "Expected '}' after handler block")?;
        // 回滚平铺发射（witness 已捕获，寄存器号复用）
        //
        // 回滚平铺发射（witness 已捕获，寄存器号复用）
        //
        // v0.104.6 D36（**未修，如实记录**）：body/handler 的**平铺副本**确实
        // 是死代码（注释所述），但其中混着**嵌套构造刚发射的真实指令** ——
        // 最典型的是 body 里再写一个 `handle`。此处 `truncate(saved_len)`
        // 一刀切，把内层刚发射的 `MirInst::Handle` 一起截掉。探针实测
        // （`let r = handle a { handle b {6}{8} } {7}`）：
        //
        // ```text
        // 内层 b: saved_len=0 平铺期间产生 2 条: ["Const(0,6.0)", "Const(1,8.0)"]
        // 外层 a: saved_len=0 平铺期间产生 2 条: ["Handle{effect:\"b\"}", "Const(1,7.0)"]
        //                                            ↑ 内层的真实指令就在外层的回滚范围内
        // ```
        //
        // 同时 `next_reg = saved_next_reg` 让**内外层 `k_dst` 撞同一个编号**
        // （实测内层 k_dst=0、外层也是 0）。
        //
        // **曾试过在此保留 `MirInst::Handle` 指令 + 让 next_reg 只回退到保留
        // 指令占用的最大编号 —— 已回退**：那样内层指令保住了、嵌套 handle 的
        // 内层值不再丢，但 emit.rs 产出的指令序列随之变化（多一条 Handle），
        // 与 9 层管线的 `differential_check` 从「通过」变「失败」
        // （`pipeline_mir=14 original_mir=15`，`inst[1]: pipeline="Define"
        // original="Handle"`）→ **生产路径回落到 emit.rs**，而回落路径的
        // 嵌套 handle 仍不完整 —— 即**把原本正常的 9 层嵌套 handle 弄坏**，
        // 净负面。
        //
        // **前置工作**：让两条路径在嵌套 handle 上产出**一致的指令序列**
        // （本仓库对所有其它构造都靠 `differential_check` 锁着等价性，
        // 唯独嵌套 handle 这条不变量已经破了）。在那之前不动此处。
        self.emit.insts.truncate(saved_len);
        self.emit.next_reg = saved_next_reg;
        // v0.80 Stage 2.0: 直接在 parser 单遍编译中 emit MirInst::Handle。
        // (parser → EmitContext 单遍，绕开 lower.rs::lower_expr 的 Handle 分支
        //  —— 那个分支是给旧 parse 路径用的，主路径走 emit_program 直接 emit)
        //
        // 把 witness 子树 lower 为 IR 序列（independent EmitContext）
        // 复用 lower.rs::lower_mir_exprs 的核心能力（body/handler 都是
        // 一个 Sequence witness）。
        use crate::mir::lower::lower_block_witness_to_mir;
        let body_mir = lower_block_witness_to_mir(&body_w);
        let handler_mir = lower_block_witness_to_mir(&handler_w);
        let k_dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::Handle {
            effect: effect.clone(),
            body: Box::new(body_mir),
            handler: Box::new(handler_mir),
            k_param: "resume".to_string(),
            k_dst,
        });
        // handle 块的整体返回值由 h_handle 在 body 执行后写入 regs[k_dst]
        // （body 末尾表达式的值，不是空 Nil）。
        // 第一版（Stage 2.0）single-shot：body 末尾表达式 = handle 整体返回值。
        //
        // v0.104.6 D35：此处**不要**把 `k_dst` 交出去。探针实测编译期
        // `k_dst` 与 `Define` 引用的寄存器**本来就一致**（都是 0）；D35 的
        // 错配是 **9 层管线的 `lower_fcfg` 重建 MirFunction 时**引入的
        // （`MORA_9LAYER=0` 下现象消失），与本函数无关。
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
    fn emit_brace_block_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        let mut stmt_wits = Vec::new();
        let mut last = 0;
        // 花括号块：跳过换行（与 emit_block_w 的 if 分支一致）
        while self.match_token(&[TokenType::Newline]) {}
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            let (r, w) = self.emit_statement_expr_w()?;
            last = r;
            stmt_wits.push(w);
            while self.match_token(&[TokenType::Newline]) {}
        }
        Some((last, Self::block_witness(stmt_wits, span)))
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
        // v0.86: 宏体编译 — 同 emit_fn_def_w 模式：子 EmitContext 编译宏体，
        // 父 EmitContext emit MacroDef 指令。宏体语句序列在子上下文中编译为
        // MirFunction，尾部 emit Return(body_reg)。运行期 dispatch.rs 中
        // Value::Macro 分支以 args 绑定 params，run_mir 执行 body。
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        // v0.104.6 D44：体发射失败时**必须还原父上下文**（见 emit_match_arm_w
        // 处的完整说明）。体被包进闭包，使 `?` 只退出闭包而不是整个函数。
        let body_res = (|| -> Option<(Reg, MirWitness)> {
            let (body_reg, body_w) = if self.match_token_exact(TokenType::Newline) {
                let mut stmt_wits = Vec::new();
                let mut last = 0;
                while self.match_token(&[TokenType::Newline]) {}
                while !self.check(&TokenType::End) && !self.is_at_end() {
                    let (r, w) = self.emit_statement_expr_w()?;
                    last = r;
                    stmt_wits.push(w);
                    while self.match_token(&[TokenType::Newline]) {}
                }
                self.consume(TokenType::End, "Expected 'end' after macro body")?;
                (last, Self::block_witness(stmt_wits, span))
            } else {
                self.consume(TokenType::End, "Expected 'end' after macro body")?;
                (
                    0,
                    MirWitness {
                        kind: WitnessKind::Sequence(Vec::new()),
                        span,
                    },
                )
            };
            self.emit.emit_tail_return(Some(body_reg));
            Some((body_reg, body_w))
        })();
        let Some((_body_reg, body_w)) = body_res else {
            self.emit = parent;
            return None;
        };
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        self.emit.emit(MirInst::MacroDef {
            name: name.clone(),
            params: params.clone(),
            body: Box::new(body_mir),
        });
        Some(MirWitness {
            kind: WitnessKind::MacroDef {
                name,
                params,
                body: Box::new(body_w),
            },
            span,
        })
    }

    fn emit_if_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.advance(); // 'if'
        let (cond, cond_w) = self.emit_expr_w()?;
        // v0.75.79: if 结果经寄存器传递（Copy dst=src）——不再经 env 临时名
        // `__if_result`（Assign 写未定义变量静默失败，分支值丢失）。
        // 分支值写各自 reg，尾端 Copy 到公共 dst，跳转使仅选中分支可达。
        self.emit.emit(MirInst::JumpIfNot(cond, 0));
        let jumpifnot_idx = self.emit.insts.len() - 1;
        let dst = self.emit.alloc_reg();

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

        let (_then_reg, then_w) = if has_then_kw {
            self.advance(); // consume 'then'
            let then_is_block = matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Newline) | Some(TokenType::LBrace)
            );
            let (r, w) = if then_is_block {
                self.emit_block_w()?
            } else {
                // v0.104.2: `then <stmt>` 的单语句形态（spec §7.1 L83
                // `if cond then "a" else "b" end`、§7.3 L412/413
                // `if i == 3 then continue end`）。走**语句**分派而非表达式
                // 分派 —— `continue`/`break`/`return` 是语句，`emit_expr_w`
                // 会把它们当标识符解析（"Failed to parse"）。
                // 尾随 `end` **由本分支消费**，否则留给外层块解析器当语句起始。
                let (r, w) = self.emit_statement_expr_w()?;
                self.match_token_exact(TokenType::End);
                (r, w)
            };
            self.emit.emit(MirInst::Copy(dst, r));
            (r, w)
        } else {
            let (r, w) = self.emit_block_w()?;
            self.emit.emit(MirInst::Copy(dst, r));
            (r, w)
        };
        self.emit.emit(MirInst::Jump(0));
        let jump_end_idx = self.emit.insts.len() - 1;
        let else_start = self.emit.insts.len();
        self.emit.patch_label_at(jumpifnot_idx, else_start);
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
            // 现在：`else if` 递归走 `emit_if_w`（它同样返回结果寄存器，
            // 与块体路径的 `Copy(dst, r)` 契约一致）。
            let else_is_if = self.check(&TokenType::If);
            let (else_reg, w) = if else_is_if {
                self.emit_if_w()?
            } else {
                self.emit_block_w()?
            };
            self.emit.emit(MirInst::Copy(dst, else_reg));
            Some(Box::new(w))
        } else {
            let nil_reg = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::Const(nil_reg, crate::value::Value::Nil));
            self.emit.emit(MirInst::Copy(dst, nil_reg));
            None
        };
        let end = self.emit.insts.len();
        self.emit.patch_label_at(jump_end_idx, end);
        let w = MirWitness {
            kind: WitnessKind::If {
                cond: Box::new(cond_w),
                then: Box::new(then_w),
                r#else: else_w,
            },
            span,
        };
        Some((dst, w))
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
                | TokenType::PromptString(_)
                | TokenType::Char(_)
                | TokenType::Int(_)
                | TokenType::Float(_)
                | TokenType::BigInt(_)
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

    /// emit 一个块（{} 或换行到 end），返回 (最后结果寄存器, 块 witness)。
    /// 块 witness：单条语句取该语句的 witness；多条语句嵌套为 Sequence。
    pub(super) fn emit_block_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        let mut stmt_wits = Vec::new();
        // v0.104.2: `Option<Reg>` —— 空块不得返回哨兵 0（见下方 None 分支）。
        let mut last: Option<Reg> = None;
        if self.match_token_exact(TokenType::LBrace) {
            // v0.75.78: 与 parse_block_body/else 分支对称 —— 块内语句间允许
            // 换行（`if c {\n  stmt\n}`）。修复前 `{` 后不跳换行，多行
            // brace 块在 compile 主路径解析失败（旧 parse 路径可解析，
            // 差分测试只覆盖单行 if，未暴露）。
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::RBrace) && !self.is_at_end() {
                let (r, w) = self.emit_statement_expr_w()?;
                last = Some(r);
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
                let (r, w) = self.emit_statement_expr_w()?;
                last = Some(r);
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
        let last = match last {
            Some(r) => r,
            // v0.104.2: 空块 → 分配一个真实的 Nil 寄存器，不再返回哨兵 0。
            // 空块（`if c then else … end`、`transaction end` 之类）在**独立
            // 寄存器空间**里可能一个寄存器都没分配，返回 0 会让消费者引用
            // 越界寄存器（`node_ready` 的 `reg_ready[0]` panic）。
            None => {
                let r = self.emit.alloc_reg();
                self.emit.emit(MirInst::Const(r, crate::value::Value::Nil));
                r
            }
        };
        Some((last, Self::block_witness(stmt_wits, span)))
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

    /// 块内语句 → (结果寄存器, witness)（表达式语句返回其 dst，其余返回 0）。
    /// 嵌套语句级分发 — 镜像 parse 侧语句级构造分发（Let 优先 > Match > If > For > While）。
    /// v0.75.78: 补齐 Let/If/Match/For/While 分发 — 修复前嵌套上下文（task 体、
    /// 闭包体、for 体）中这些构造直接落 emit_expr_w → 解析失败（compile
    /// 主路径自 v0.75.40 起缺此分发，旧 parse 路径支持）。
    /// v0.75.81: 事务家族（transaction/commit/rollback）+ eval 断言接入
    /// 语句分发（lexer 无对应关键字，经 peek_is_identifier 识别，try/while
    /// 先例）。
    pub(super) fn emit_statement_expr_w(&mut self) -> Option<(Reg, MirWitness)> {
        if self.check(&TokenType::Let) {
            return self.emit_let_w().map(|w| (0, w));
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
            return self.emit_assign_w().map(|w| (0, w));
        }
        match self.peek()?.token_type.clone() {
            TokenType::Return | TokenType::Break | TokenType::Continue => {
                let w = self.emit_return_break_continue_w()?;
                Some((0, w))
            }
            TokenType::Match => self.emit_match_w(),
            TokenType::If => self.emit_if_w(),
            TokenType::For => self.emit_loop_w(),
            TokenType::Identifier(n) if n == "while" => self.emit_while_w(),
            TokenType::Identifier(n) if n == "transaction" => self.emit_transaction_w(),
            TokenType::Identifier(n) if n == "eval" => self.emit_eval_w(),
            TokenType::Identifier(n) if n == "aggregate" => self.emit_aggregate_w(),
            TokenType::Identifier(n) if n == "handle" => self.emit_handle_w().map(|w| (0, w)),
            TokenType::Identifier(n) if n == "perform" => self.emit_perform_w().map(|w| (0, w)),
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
                self.emit_effect_sig_w().map(|w| (0, w))
            }
            // v0.85: `with` 配置块（可嵌套在 task body 内）
            TokenType::With => self.emit_with_w().map(|w| (0, w)),
            TokenType::App => self.emit_app_def_w().map(|w| (0, w)),
            // v0.103: 命名 section 声明（嵌套上下文）
            TokenType::Prompt => self.emit_section_w(true).map(|w| (0, w)),
            TokenType::Document => self.emit_section_w(false).map(|w| (0, w)),
            // v0.103: export（嵌套上下文）
            TokenType::Identifier(ref s) if s == "export" => self.emit_export_w().map(|w| (0, w)),
            // v0.103: TEA 独立声明（嵌套上下文）
            TokenType::Identifier(ref s)
                if (s == "model" || s == "msg")
                    && matches!(
                        self.tokens.get(self.current + 1).map(|t| &t.token_type),
                        Some(TokenType::Identifier(_))
                    ) =>
            {
                let w = if s == "model" {
                    self.emit_model_def_w()
                } else {
                    self.emit_msg_def_w()
                };
                w.map(|w| (0, w))
            }
            // v0.103: 可观测性块（嵌套上下文）
            TokenType::Identifier(ref s) if s == "observe" => self.emit_observe_w().map(|w| (0, w)),
            TokenType::Identifier(ref s) if s == "span" => self.emit_span_w().map(|w| (0, w)),
            // v0.103: 并行块 / worker（嵌套上下文）
            TokenType::Identifier(ref s) if s == "parallel" => {
                self.emit_parallel_w().map(|w| (0, w))
            }
            TokenType::Identifier(ref s) if s == "worker" => self.emit_worker_w().map(|w| (0, w)),
            // v0.102: 声明式范式（嵌套语句上下文）
            TokenType::Rel => self.emit_rel_def_w().map(|w| (0, w)),
            TokenType::Solve => self.emit_solve_w(),
            TokenType::Identifier(n) if n == "commit" => {
                let span = self.span_of_current();
                self.advance(); // 'commit'
                self.emit.emit(MirInst::Commit);
                // v0.104.2: 返回**真实分配**的寄存器并写入 Nil —— `Commit`
                // 是 unit 指令（无 dst），此前直接返回哨兵 `0`。事务体是独立
                // 寄存器空间，`transaction commit end` 里它一个寄存器都没分配，
                // 于是 `Some((0, _))` 被当作 body 的末值寄存器 → 返回语句引用
                // reg 0 → run_mir 拿到 n_regs=0 的函数 → `node_ready` 越界 panic。
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Nil));
                let w = MirWitness {
                    kind: WitnessKind::Sequence(vec![]),
                    span,
                };
                Some((dst, w))
            }
            TokenType::Identifier(n) if n == "rollback" => {
                let span = self.span_of_current();
                self.advance(); // 'rollback'
                self.emit.emit(MirInst::Rollback);
                // 同 commit：返回真实寄存器（rollback 通过 dispatch 返回 Err
                // 中断，该寄存器不会被读到，但保持与 commit 同一形状）。
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::Nil));
                let w = MirWitness {
                    kind: WitnessKind::Sequence(vec![]),
                    span,
                };
                Some((dst, w))
            }
            _ => self.emit_expr_w(),
        }
    }

    fn emit_loop_w(&mut self) -> Option<(Reg, MirWitness)> {
        // 'for' var 'in' iterable newline body 'end'（镜像 lower Loop）
        let span = self.span_of_current();
        self.advance(); // 'for'
        // v0.104.6 D354：循环变量是**声明位**（引入新名字），与 `let` 同类，
        // 走严格版。此前用 `consume_identifier` 会吃关键字兜底 ⇒
        // `for if in xs` exit 0 声明成功，而体内 `print(if)` 解析失败。
        let var = self.consume_plain_identifier("Expected loop variable")?;
        self.consume(TokenType::In, "Expected 'in' in for loop")?;
        let (iter_reg, iter_w) = self.emit_expr_w()?;
        use crate::value::Value;
        let i_reg = self.emit.alloc_reg();
        self.emit.emit(MirInst::Const(i_reg, Value::Int(0)));
        let len_reg = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Call(len_reg, "len".to_string(), vec![iter_reg]));
        let one_reg = self.emit.alloc_reg();
        self.emit.emit(MirInst::Const(one_reg, Value::Int(1)));

        let loop_label = self.emit.insts.len();
        let cond_reg = self.emit.alloc_reg();
        self.emit.emit(MirInst::BinaryOp(
            cond_reg,
            i_reg,
            BinaryOp::GreaterEqual,
            len_reg,
        ));
        self.emit.emit(MirInst::JumpIf(cond_reg, 0));
        let exit_jump_idx = self.emit.insts.len() - 1;

        let x_reg = self.emit.alloc_reg();
        self.emit.emit(MirInst::Index(x_reg, iter_reg, i_reg));
        self.emit.emit(MirInst::Define(var.clone(), x_reg));

        self.emit.push_loop_scope(loop_label, 0);
        let (_, body_w) = self.emit_block_w()?;
        // v0.104.6 缺陷修复：只回填**本层**登记的 break/continue 索引。
        //
        // 旧实现是 `for i in body_start..body_end` 全区间扫描重写标签。该区间在
        // 嵌套循环下**包含内层循环的全部指令** —— 外层回填会把内层已正确指向
        // 内层出口/内层增量的标签覆盖成外层的，于是：
        //   - 嵌套 `break`    → 内层 break 跳到了外层出口 → 外层只跑一轮；
        //   - 嵌套 `continue` → 被改成外层 for 的增量位置 → 控制流失控，
        //     整个程序静默结束（无任何输出、退出码 0）。
        // 详见 `EmitContext::break_slots` 的文档。
        let (my_breaks, my_continues) = self.emit.pop_loop_scope();

        self.emit
            .emit(MirInst::BinaryOp(i_reg, i_reg, BinaryOp::Add, one_reg));
        // v0.104.2: `continue` 的目标是**增量指令**，不是 `loop_label`。
        //
        // **缺陷（`for i in [1,2,3]` + `if i == 2 { continue }` 挂死）**：
        // `continue` 此前被后修补成 `loop_label` —— 即条件判定处，**跳过了
        // 循环变量增量**。`i` 因此永不前进，条件恒为「未越界」→ 无限循环。
        // （`break` 无此问题：它的目标是循环出口。）
        // 语义上 `continue` = 「跳过本次迭代剩余 body」，而「下一次迭代」
        // 在本 lowering 里必须先执行增量，故目标应为刚发出的这条 BinaryOp。
        let increment_idx = self.emit.insts.len() - 1;
        self.emit.emit(MirInst::Jump(loop_label));
        let end_label = self.emit.insts.len();
        self.emit.patch_label_at(exit_jump_idx, end_label);
        self.emit.patch_breaks_to(&my_breaks, end_label);
        self.emit.patch_continues_to(&my_continues, increment_idx);
        let dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::Const(dst, Value::Nil));
        let w = MirWitness {
            kind: WitnessKind::Loop {
                var,
                iterable: Box::new(iter_w),
                body: Box::new(body_w),
            },
            span,
        };
        Some((dst, w))
    }

    fn emit_while_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.advance(); // 'while'
        let loop_label = self.emit.insts.len();
        let (c, cond_w) = self.emit_expr_w()?;
        self.emit.emit(MirInst::JumpIfNot(c, 0));
        let exit_jump_idx = self.emit.insts.len() - 1;

        self.emit.push_loop_scope(loop_label, 0);
        let (_, body_w) = self.emit_block_w()?;
        // v0.104.6 缺陷修复：同 emit_for_w —— 按作用域登记回填，不做全区间扫描。
        // while 无循环变量增量，故 `continue` 目标仍是 `loop_label`。
        let (my_breaks, my_continues) = self.emit.pop_loop_scope();

        self.emit.emit(MirInst::Jump(loop_label));
        let end_label = self.emit.insts.len();
        self.emit.patch_label_at(exit_jump_idx, end_label);
        self.emit.patch_breaks_to(&my_breaks, end_label);
        self.emit.patch_continues_to(&my_continues, loop_label);
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        let w = MirWitness {
            kind: WitnessKind::While {
                cond: Box::new(cond_w),
                body: Box::new(body_w),
            },
            span,
        };
        Some((dst, w))
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
    /// 镜像 h_transaction 语义：body 独立寄存器空间（子上下文），
    /// body 内 `rollback` 经 MirInst::Rollback 返回 Err → run_isolated 得
    /// Err → 执行 compensation 后抛 "Transaction rolled back"；`commit`
    /// 为 no-op（MirInst::Commit → Ok）。body 终止于 `compensation` 或 `end`。
    /// witness = Sequence(body + compensation 语句)（无值语句，typeck 得 Nil）。
    fn emit_transaction_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.advance(); // 'transaction'
        // 子上下文：事务体是独立寄存器空间（镜像 lower/closure/task 分支）
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let mut body_wits = Vec::new();
        // v0.104.2: `Option<Reg>` 而非哨兵 0 —— 空事务体（`transaction end`）
        // 在**独立寄存器空间**里一个寄存器都没分配，此时引用 reg 0 会让
        // `run_mir` 拿到 n_regs=0 的函数 → `node_ready` 用 reg_ready[0] 越界
        // panic（"the len is 0 but the index is 0"）。与 emit_block_mir_and_wit /
        // emit_section_w 的既有一致写法。
        let mut last: Option<Reg> = None;
        // v0.104.6 D44：体发射失败时**还原父上下文**（见 emit_match_arm_w 处的
        // 完整说明）。体包进闭包，使 `?` 只退出闭包而不是整个函数。
        let body_ok = (|| -> Option<()> {
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::End)
                && !self.peek_is_identifier("compensation")
                && !self.is_at_end()
            {
                let (r, w) = self.emit_statement_expr_w()?;
                last = Some(r);
                body_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.emit.emit_tail_return(last);
            Some(())
        })();
        if body_ok.is_none() {
            self.emit = parent;
            return None;
        }
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();

        // compensation 段（可选）：`compensation` 后语句循环到 `end`
        let comp_mir = if self.peek_is_identifier("compensation") {
            self.advance(); // 'compensation'
            let parent2 = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
            let mut comp_wits = Vec::new();
            let mut comp_last: Option<Reg> = None;
            let comp_ok = (|| -> Option<()> {
                while self.match_token(&[TokenType::Newline]) {}
                while !self.check(&TokenType::End) && !self.is_at_end() {
                    let (r, w) = self.emit_statement_expr_w()?;
                    comp_last = Some(r);
                    comp_wits.push(w);
                    while self.match_token(&[TokenType::Newline]) {}
                }
                self.emit.emit_tail_return(comp_last);
                Some(())
            })();
            if comp_ok.is_none() {
                self.emit = parent2;
                return None;
            }
            let cm = std::mem::replace(&mut self.emit, parent2).finish();
            body_wits.extend(comp_wits);
            cm
        } else {
            MirFunction {
                params: vec![],
                body: vec![],
                n_regs: 0,
                ..Default::default()
            }
        };
        self.consume(TokenType::End, "Expected 'end' after transaction")?;

        self.emit.emit(MirInst::Transaction {
            body: Box::new(body_mir),
            compensation: Box::new(comp_mir),
        });
        let w = MirWitness {
            kind: WitnessKind::Sequence(body_wits),
            span,
        };
        // v0.104.6 D40：返回**真实分配**的寄存器 —— 与上面 v0.104.2 的
        // `last: Option<Reg>` / emit_tail_return 同一形状。那次只补了**内层**
        // （事务体自己的寄存器空间），本行是**外层**：`transaction` 语句本身
        // 在父上下文里只 emit 一条无 dst 的 `Transaction`，一个寄存器都没分配。
        // 当它是所在块的唯一/末条语句时（`task w() / transaction / 1 / end /
        // end`，嵌套 MirFunction = 独立寄存器空间 → 父函数 n_regs=0），
        // 块级 `emit_tail_return(Some(0))` 引用 reg 0 → `run_dag` 拿到
        // `reg_ready` 长度 0 → `node_ready` 的 `reg_ready[*r]` 越界 panic
        // （"the len is 0 but the index is 0"，exit=101）。
        // 与 commit / rollback 同款：unit 语句写入 Nil 到新分配的寄存器。
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some((dst, w))
    }

    /// v0.75.81: eval 断言语句（α.8 Eval 原语前端，v0.25 Agent 行为回归测试）。
    ///
    /// ```mora
    /// eval ["name"] given_expr, expect1, expect2, ...
    /// ```
    /// 首 token 为字符串字面量时作为断言名；given 与各 expect 为表达式。
    /// 经 h_eval 执行：given 与每个 expect 逐一比较（tolerance 未设），
    /// 任一不等报错（断言失败）。witness = 空 Sequence（无值语句）。
    fn emit_eval_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.advance(); // 'eval'
        let token = self.peek().cloned()?;
        let name = if let TokenType::String(s) = token.token_type {
            self.advance();
            s
        } else {
            String::new()
        };
        let (given_reg, _) = self.emit_expr_w()?;
        let mut expects = Vec::new();
        while self.match_token(&[TokenType::Comma]) {
            let (r, _) = self.emit_expr_w()?;
            expects.push(r);
        }
        self.emit.emit(MirInst::Eval {
            name,
            given_reg,
            expects,
            tolerance: None,
            replay_path: None,
        });
        let w = MirWitness {
            kind: WitnessKind::Sequence(vec![]),
            span,
        };
        Some((0, w))
    }

    /// v0.75.83: aggregate 语句 — 向 per-super-step 聚合器贡献值。
    ///
    /// ```mora
    /// aggregate name, value_expr
    /// ```
    /// name 为聚合器名（引擎 config.aggregators 声明，reducer Add/Max/Min/
    /// Last/Concat）；value_expr 为贡献值。经 h_aggregate push 到 MirHost
    /// 缓冲，Pregel 引擎超步末收集归约。witness = 空 Sequence（无值语句）。
    fn emit_aggregate_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.advance(); // 'aggregate'
        let name = self.consume_identifier("Expected aggregator name after 'aggregate'")?;
        self.consume(TokenType::Comma, "Expected ',' after aggregator name")?;
        let (value_reg, _) = self.emit_expr_w()?;
        self.emit.emit(MirInst::Aggregate {
            name,
            value: value_reg,
        });
        let w = MirWitness {
            kind: WitnessKind::Sequence(vec![]),
            span,
        };
        Some((0, w))
    }

    fn emit_match_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.advance(); // 'match'
        let (val_reg, scrutinee_w) = self.emit_expr_w()?;
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
        let mut arms = Vec::new();
        let mut arm_wits = Vec::new();
        if with_form {
            while !self.check(&TokenType::End) && !self.is_at_end() {
                // v0.104.6 D44b：**先**吃掉 arm 之间的换行，再尝试解析 arm。
                //
                // 此前换行清理写在 arm 尝试**之后**，于是循环第一次进入时 token
                // 必是 Newline → `parse_pattern` 失败 → 落到 `self.advance()`
                // 恢复分支。**那个恢复分支其实是承重的**（不是纯防御），把它
                // 直接换成报错会让**所有** match 形式失效。
                while self.match_token(&[TokenType::Newline]) {}
                if self.check(&TokenType::End) || self.is_at_end() {
                    break;
                }
                if let Some(arm) = self.emit_match_arm_w() {
                    arms.push((arm.pat_str, arm.guard, arm.body_mir, arm.val_reg));
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
                    arms.push((arm.pat_str, arm.guard, arm.body_mir, arm.val_reg));
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
        // v0.104: match 的结果寄存器必须位于**外层函数**的寄存器空间，且所有
        // arm 共用它（`h_match_expr` 把选中 arm 的返回值写进 `regs[output_reg]`；
        // `MirInst::dst()` 对 MatchExpr 取的正是 `arms.last().3`）。
        //
        // 缺陷：此前把每个 arm 的**arm 局部**结果寄存器（`arm_val_reg`，来自
        // arm 自己的 `EmitContext`，编号自 0 起）当作 output_reg。arm 寄存器
        // 空间与外层无关，编号常超出外层 `n_regs` → `h_match_expr` 越界 panic
        //（"index out of bounds: the len is 2 but the index is 4"）；即便不越界，
        // 结果也写进了外层从未读取的寄存器。另有一个 `dst` 被分配并返回，却
        // 没有任何 arm 写它 —— 消费者读到的恒为 Nil。
        // 统一为「先分配外层 dst，再让所有 arm 指向它」（与 fcfg_lower 的
        // `lower_match` 同契约）。
        let dst = self.emit.alloc_reg();
        for arm in &mut arms {
            arm.3 = dst;
        }
        self.emit.emit(MirInst::MatchExpr { val: val_reg, arms });
        let w = MirWitness {
            kind: WitnessKind::Match {
                scrutinee: Box::new(scrutinee_w),
                arms: arm_wits,
            },
            span,
        };
        Some((dst, w))
    }

    fn emit_match_arm_w(&mut self) -> Option<EmittedMatchArm> {
        let pattern = self.emit_pattern()?;
        // v0.104.3: arm 箭头有**两种拼写** —— `=>`（既有实现形态、全部
        // fixtures 使用）与 `->`（spec §14.2 EBNF 与 §7.3/§7.4/§7.5 教学章节
        // 一律使用）。lexer 早有 `TokenType::Arrow`，只是 match arm 从未接受它。
        let is_arrow = |t: &TokenType| matches!(t, TokenType::FatArrow | TokenType::Arrow);
        // v0.87: Detect optional "when <guard_expr>" before the arm arrow
        //
        // v0.104.3 修复：守卫与 arm body 同构 —— 发射成**延迟求值**的
        // MirFunction，由 `h_match_expr` 在**模式绑定之后**调用。
        //
        // 缺陷：守卫此前在外层寄存器空间求值，而守卫通常引用**模式绑定变量**
        //（`x when x > 0`）—— 绑定只发生在 `h_match_expr` 匹配成功之时
        //（`self_match_pattern` 把 val define 进 env）。外层求值时该变量尚不
        // 存在 → 读到 Nil → `is_truthy(Nil) == false` → **该 arm 恒被跳过**，
        // `when` 完全失效。实测 `match -5i { x when x > 0i => "positive"
        // x when x < 0i => "negative" _ => "zero" }` 返回 "positive"；
        // fixture `match_guard.mora` 之所以"通过"只因取值 42/0 恰好让首守卫
        // 为真 —— 属侥幸。
        let guard = if let TokenType::Identifier(ref name) = self.peek()?.token_type
            && name == "when"
        {
            self.advance(); // consume "when"
            // 独立寄存器空间（与 arm body 同规则）：守卫体自成 MirFunction
            //
            // v0.104.6 D44：无论体发射成功与否都**还原父上下文**。此前
            // `emit_expr_w()?` 早退时父上下文被丢弃、`self.emit` 停留在一次性的
            // 子上下文上 —— 此后**整个外层函数体的剩余部分都被发射进子上下文
            // 然后丢掉**（静默，无任何报错）。
            let guard_parent =
                std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
            let guard_res = self.emit_expr_w();
            let Some((guard_val_reg, guard_w)) = guard_res else {
                self.emit = guard_parent;
                return None;
            };
            self.emit.emit(MirInst::Return(Some(guard_val_reg)));
            let guard_mir = std::mem::replace(&mut self.emit, guard_parent).finish();
            if !self
                .peek()
                .map(|t| is_arrow(&t.token_type))
                .unwrap_or(false)
            {
                return None;
            }
            self.advance(); // consume '=>' 或 '->'
            Some((Box::new(guard_mir), guard_w))
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
        // 子上下文：arm body 是独立寄存器空间（镜像 lower Match 分支）
        //
        // v0.104.6 D44：无论体发射成功与否都**还原父上下文**（同上，守卫那处）。
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let arm_res = self.emit_expr_w();
        let Some((arm_val_reg, body_w)) = arm_res else {
            self.emit = parent;
            return None;
        };
        self.emit.emit(MirInst::Return(Some(arm_val_reg)));
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        let pat_str = crate::mir::lower::pattern_to_string(&pattern);
        let witness = crate::mir::witness::WitnessArm {
            pattern,
            guard: guard.as_ref().map(|(_, w)| w.clone()),
            body: body_w,
        };
        Some(EmittedMatchArm {
            pat_str,
            guard: guard.map(|(m, _)| m),
            body_mir: Box::new(body_mir),
            val_reg: arm_val_reg,
            witness,
        })
    }

    /// v0.92: parse_pattern 返回 WitnessPattern（witness-native）。
    fn emit_pattern(&mut self) -> Option<crate::mir::witness::WitnessPattern> {
        // 复用完整 pattern 解析器（通配/字面量/变量/元组/列表/dict/类型标注）
        self.parse_pattern()
    }

    fn emit_orchestrate_w(&mut self) -> Option<MirWitness> {
        // v0.92: parse_orchestrate_statement 直接返回 MirWitness（Orchestrate 变体）
        // —— orchestrate 是数据构造指令，运行时引擎执行，不参与递归 emit。
        let witness = self.parse_orchestrate_statement()?;
        if let WitnessKind::Orchestrate {
            input_var,
            result_var,
            kind,
        } = &witness.kind
        {
            // WitnessOrchestrateKind → MirOrchestrateKind（含 agent/edge/expert 转换）
            let mir_kind = crate::mir::orchestrate::MirOrchestrateKind::from_witness_kind(kind);
            self.emit.emit(MirInst::Orchestrate {
                input_var: input_var.clone(),
                result_var: result_var.clone(),
                kind: Box::new(mir_kind),
            });
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::Const(dst, crate::value::Value::Nil));
            Some(witness)
        } else {
            None
        }
    }

    /// v0.85: `as dyn Trait` coercion（§3.5 spec 承诺）。
    /// 检测当前 token 序列为 `as dyn <TraitName>`，emit MirInst::DynTrait
    /// 指令，将 src_reg 的 plain value 包装为 Value::TraitObject。
    /// 未检测到则原样返回。
    fn emit_dyn_coercion(&mut self, src_reg: Reg, src_w: MirWitness) -> Option<(Reg, MirWitness)> {
        if self.check(&TokenType::As) {
            self.advance(); // 'as'
            if self.check(&TokenType::Dyn) {
                self.advance(); // 'dyn'
                let trait_name = self.consume_identifier("Expected trait name after 'dyn'")?;
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::DynTrait {
                    dst,
                    src: src_reg,
                    trait_generics: Vec::new(),
                    trait_name: trait_name.clone(),
                });
                let span = src_w.span;
                return Some((
                    dst,
                    MirWitness {
                        kind: WitnessKind::DynTrait {
                            expr: Box::new(src_w),
                            trait_name,
                            generics: Vec::new(),
                        },
                        span,
                    },
                ));
            }
        }
        Some((src_reg, src_w))
    }

    /// v0.85: `with key = value, key2 = value2` body end — 配置桥接块（§19.4 spec 承诺）。
    /// bindings 是 (name, value_reg) 列表，body 是嵌套 MirFunction。
    /// MirInst::WithConfig 经解释器保存到 current_ai_config 中（mock_llm/mock_responses）。
    /// 与 handle/perform 同模式（Identifier 派生），不引入新 TokenType。
    /// v0.103: 命名 section 声明 —— `prompt "name" do ... end` /
    /// `document "name" do ... end`。
    ///
    /// 与 `with` 块同构：独立寄存器空间求 body，产出
    /// `MirInst::PromptSection`/`DocumentSection`（含 name）。运行时 h_* 把
    /// `Value::PromptSection` 绑定到该名字下；typeck 亦登记同名类型。
    ///
    /// 缺陷背景：`prompt` 是词法关键字但 parser 从不消费（TokenType::Prompt
    /// 零使用点）；`compose_prompt` 这个已注册的活 builtin 因此永远拿不到
    /// section，其错误消息还指引用户写这个当时不存在的语法。
    fn emit_section_w(&mut self, is_prompt: bool) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'prompt' / 'document'
        // section 名：字符串字面量
        let name = match self.peek()?.token_type.clone() {
            TokenType::String(s) => {
                self.advance();
                s
            }
            // 允许裸标识符名（`prompt sys do ... end`）
            TokenType::Identifier(s) => {
                self.advance();
                s
            }
            _ => return None,
        };
        // 可选 `do` 引导
        let _ = self.match_token_exact(TokenType::Do);
        let _ = self.match_token(&[TokenType::Newline]);

        // 子上下文：body 是独立寄存器空间
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let mut body_wits = Vec::new();
        let mut last: Option<Reg> = None;
        // v0.104.6 D44：失败时还原父上下文（见 emit_match_arm_w 处的说明）
        let sec_ok = (|| -> Option<()> {
            while !self.check(&TokenType::End) && !self.is_at_end() {
                // v0.104.6 D45：显式的「保证进度」不变量 —— 一轮循环必须至少
                // 消费一个 token，否则报错。失败路径（None）已由下面处理；这一
                // 条覆盖「返回 Some 却不消费」的情况，杜绝任何形式的空转。
                let before = self.current;
                match self.emit_statement_expr_w() {
                    Some((r, w)) => {
                        last = Some(r);
                        body_wits.push(w);
                    }
                    // v0.104.6 D45：此前是 `if let Some(..) {}` —— 语句解析失败
                    // 就**跳过**，而失败路径不消费任何 token → `while` 条件恒真
                    // → **死循环，编译器永不返回**（实测 `prompt`/`document`
                    // 块内放一条无法解析的语句即挂死）。
                    // 改为报错：既保证进度，也不再静默吞语句（D44b 同原则）。
                    None => {
                        eprintln!(
                            "Parse error: unparsable statement in section block \
                             (line {})",
                            self.current_line()
                        );
                        return None;
                    }
                }
                while self.match_token(&[TokenType::Newline]) {}
                if self.current == before {
                    eprintln!(
                        "Parse error: parser made no progress in section block (line {})",
                        self.current_line()
                    );
                    return None;
                }
            }
            self.consume(TokenType::End, "Expected 'end' after section block")?;
            Some(())
        })();
        if sec_ok.is_none() {
            self.emit = parent;
            return None;
        }
        // body 求值 → Return（h_* 取此值作为 section text）
        self.emit.emit_tail_return(last);
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();

        if is_prompt {
            self.emit.emit(MirInst::PromptSection {
                name: name.clone(),
                body: Box::new(body_mir),
            });
        } else {
            self.emit.emit(MirInst::DocumentSection {
                name: name.clone(),
                body: Box::new(body_mir),
            });
        }
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));

        let body_wit = Self::block_witness(body_wits, span);
        Some(MirWitness {
            kind: if is_prompt {
                WitnessKind::PromptSection {
                    name,
                    body: Box::new(body_wit),
                }
            } else {
                WitnessKind::DocumentSection {
                    name,
                    body: Box::new(body_wit),
                }
            },
            span,
        })
    }

    /// v0.103: `observe <kind> "<name>" do ... end` / `span "<name>" tags {..} do ... end`。
    ///
    /// spec §11.4 承诺的可观测性块。此前 MirInst::Observe/Span 有 handler
    /// 但零 producer（parser 不产出），语法完全不可用。
    fn emit_observe_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'observe'
        // kind：trace / metric / log（裸标识符）
        let kind = self.consume_identifier("Expected observe kind (e.g. trace)")?;
        // 名：字符串或裸标识符
        let name = match self.peek()?.token_type.clone() {
            TokenType::String(s) => {
                self.advance();
                s
            }
            TokenType::Identifier(s) => {
                self.advance();
                s
            }
            _ => String::new(),
        };
        let config = if name.is_empty() {
            kind
        } else {
            format!("{} {}", kind, name)
        };
        let (body_mir, body_wit) = self.emit_block_mir_and_wit("observe")?;
        self.emit.emit(MirInst::Observe {
            config: config.clone(),
            body: Box::new(body_mir),
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::Observe {
                config,
                body: Box::new(body_wit),
            },
            span,
        })
    }

    /// `span "<name>" tags {k: "v", ...} do ... end` —— 命名追踪 span。
    fn emit_span_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'span'
        let name = match self.peek()?.token_type.clone() {
            TokenType::String(s) => {
                self.advance();
                s
            }
            TokenType::Identifier(s) => {
                self.advance();
                s
            }
            _ => return None,
        };
        // 可选 `tags {k: "v", ...}`（键为标识符，值为字符串或标识符）
        let mut tags: Vec<(String, String)> = Vec::new();
        if matches!(self.peek().map(|t| &t.token_type), Some(TokenType::Identifier(s)) if s == "tags")
        {
            self.advance(); // 'tags'
            self.consume(TokenType::LBrace, "Expected '{' after 'tags'")?;
            while !self.check(&TokenType::RBrace) && !self.is_at_end() {
                let k = self.emit_dict_key()?;
                self.consume(TokenType::Colon, "Expected ':' in tags")?;
                let v = match self.peek()?.token_type.clone() {
                    TokenType::String(s) => {
                        self.advance();
                        s
                    }
                    TokenType::Identifier(s) => {
                        self.advance();
                        s
                    }
                    _ => String::new(),
                };
                tags.push((k, v));
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(TokenType::RBrace, "Expected '}' after tags")?;
        }
        let (body_mir, body_wit) = self.emit_block_mir_and_wit("span")?;
        self.emit.emit(MirInst::Span {
            name: name.clone(),
            tags: tags.clone(),
            body: Box::new(body_mir),
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::Span {
                name,
                tags,
                body: Box::new(body_wit),
            },
            span,
        })
    }

    /// 解析 `do? ... end` 块，返回 (MirFunction, witness)。
    /// observe/span 共用（与 emit_section_w 的块处理同构）。
    fn emit_block_mir_and_wit(&mut self, _label: &str) -> Option<(MirFunction, MirWitness)> {
        let span = self.span_of_current();
        let _ = self.match_token_exact(TokenType::Do);
        let _ = self.match_token(&[TokenType::Newline]);
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let mut body_wits = Vec::new();
        let mut last: Option<Reg> = None;
        // v0.104.6 D44：失败时还原父上下文（见 emit_match_arm_w 处的说明）
        let blk_ok = (|| -> Option<()> {
            while !self.check(&TokenType::End) && !self.is_at_end() {
                // v0.104.6 D45：保证进度（见 section 块同处说明）
                let before = self.current;
                match self.emit_statement_expr_w() {
                    Some((r, w)) => {
                        last = Some(r);
                        body_wits.push(w);
                    }
                    // v0.104.6 D45：同 section 块 —— 静默跳过且不消费 token
                    // 会让本循环**死循环**（observe / span 块实测挂死）。
                    None => {
                        eprintln!(
                            "Parse error: unparsable statement in {} block (line {})",
                            _label,
                            self.current_line()
                        );
                        return None;
                    }
                }
                while self.match_token(&[TokenType::Newline]) {}
                if self.current == before {
                    eprintln!(
                        "Parse error: parser made no progress in {} block (line {})",
                        _label,
                        self.current_line()
                    );
                    return None;
                }
            }
            self.consume(TokenType::End, "Expected 'end' after block")?;
            Some(())
        })();
        if blk_ok.is_none() {
            self.emit = parent;
            return None;
        }
        self.emit.emit_tail_return(last);
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        Some((body_mir, Self::block_witness(body_wits, span)))
    }

    /// v0.103: `update(params)` 是否为 TEA 更新函数声明（而非普通调用）。
    ///
    /// 判据（见分派处的守卫注释）：`(` 紧跟当前 token，括号内**全为裸标识符**
    /// （允许逗号与空参表）。含任意表达式实参 → 不是声明。
    fn looks_like_update_decl(&self) -> bool {
        let mut i = self.current + 1; // 指向 '('
        if !matches!(
            self.tokens.get(i).map(|t| &t.token_type),
            Some(TokenType::LParen)
        ) {
            return false;
        }
        i += 1;
        loop {
            match self.tokens.get(i).map(|t| &t.token_type) {
                // 空参表 `update()`
                Some(TokenType::RParen) => return true,
                // 形参：裸标识符
                Some(TokenType::Identifier(_)) => i += 1,
                _ => return false,
            }
            match self.tokens.get(i).map(|t| &t.token_type) {
                Some(TokenType::Comma) => i += 1,
                Some(TokenType::RParen) => return true,
                _ => return false,
            }
        }
    }

    /// v0.103: `parallel ... end` / `worker <name> do ... end`（spec §9.1/§9.2）。
    ///
    /// `parallel` 块内各 `worker` 声明并发执行；`worker` 在块外等价于普通块。
    fn emit_parallel_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'parallel'
        let _ = self.match_token(&[TokenType::Newline]);
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let mut body_wits = Vec::new();
        let mut last: Option<Reg> = None;
        // v0.104.6 D44：失败时还原父上下文（见 emit_match_arm_w 处的说明）
        let par_ok = (|| -> Option<()> {
            while !self.check(&TokenType::End) && !self.is_at_end() {
                // v0.104.6 D45：保证进度（见 section 块同处说明）
                let before = self.current;
                match self.emit_statement_expr_w() {
                    Some((r, w)) => {
                        last = Some(r);
                        body_wits.push(w);
                    }
                    // v0.104.6 D45：同 section 块 —— 静默跳过且不消费 token
                    // 会让本循环**死循环**（parallel 块实测挂死）。
                    None => {
                        eprintln!(
                            "Parse error: unparsable statement in parallel block (line {})",
                            self.current_line()
                        );
                        return None;
                    }
                }
                while self.match_token(&[TokenType::Newline]) {}
                if self.current == before {
                    eprintln!(
                        "Parse error: parser made no progress in parallel block (line {})",
                        self.current_line()
                    );
                    return None;
                }
            }
            self.consume(TokenType::End, "Expected 'end' after parallel block")?;
            Some(())
        })();
        if par_ok.is_none() {
            self.emit = parent;
            return None;
        }
        self.emit.emit_tail_return(last);
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        self.emit.emit(MirInst::Parallel {
            body: Box::new(body_mir),
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
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
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let mut body_wits = Vec::new();
        let mut last: Option<Reg> = None;
        // v0.104.6 D44：失败时还原父上下文（见 emit_match_arm_w 处的说明）
        let wrk_ok = (|| -> Option<()> {
            while !self.check(&TokenType::End) && !self.is_at_end() {
                // v0.104.6 D45：保证进度（见 section 块同处说明）
                let before = self.current;
                match self.emit_statement_expr_w() {
                    Some((r, w)) => {
                        last = Some(r);
                        body_wits.push(w);
                    }
                    // v0.104.6 D45：同 section 块 —— 静默跳过且不消费 token
                    // 会让本循环**死循环**（worker 块实测挂死）。
                    None => {
                        eprintln!(
                            "Parse error: unparsable statement in worker block (line {})",
                            self.current_line()
                        );
                        return None;
                    }
                }
                while self.match_token(&[TokenType::Newline]) {}
                if self.current == before {
                    eprintln!(
                        "Parse error: parser made no progress in worker block (line {})",
                        self.current_line()
                    );
                    return None;
                }
            }
            self.consume(TokenType::End, "Expected 'end' after worker block")?;
            Some(())
        })();
        if wrk_ok.is_none() {
            self.emit = parent;
            return None;
        }
        self.emit.emit_tail_return(last);
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        self.emit.emit(MirInst::Worker {
            name: name.clone(),
            body: Box::new(body_mir),
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::Sequence(body_wits),
            span,
        })
    }

    /// v0.103: `export <声明>` —— 把声明的绑定名标记为模块公开（spec §10.2）。
    ///
    /// 解析内部声明后从其 witness 提取产生的绑定名，包一层
    /// `WitnessKind::Export`（typeck 侧据此计算导入可见性；lower 侧据此 emit
    /// `MirInst::ExportMark`，运行时写入环境导出集）。
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
            TokenType::Rel => self.emit_rel_def_w(),
            TokenType::Prompt => self.emit_section_w(true),
            TokenType::Document => self.emit_section_w(false),
            TokenType::App => self.emit_app_def_w(),
            _ => return None,
        }?;
        let names = Self::exported_names_of(&decl);
        // v0.103: 单遍编译路径必须**在此 emit ExportMark 指令** —— 生产主路径
        // 是 ParserV3::compile（直接产出 MirInst），lower.rs 的 Export 分支只
        // 服务旧的 witness 路径。缺这一步时 typeck 静态可见（读 witness 的
        // export 名集），但运行时环境导出集为空 → import 后调用报
        // "Undefined function or task"。
        for n in &names {
            self.emit.emit(MirInst::ExportMark(n.clone()));
        }
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
            | WitnessKind::TypeAlias { name, .. }
            | WitnessKind::MacroDef { name, .. }
            | WitnessKind::RelDef { name, .. }
            | WitnessKind::PromptSection { name, .. }
            | WitnessKind::DocumentSection { name, .. }
            | WitnessKind::AppDef { name, .. } => vec![name.clone()],
            // Sequence 可含多条声明（`export` 后跟块）—— 收集全部
            WitnessKind::Sequence(items) => {
                items.iter().flat_map(Self::exported_names_of).collect()
            }
            _ => Vec::new(),
        }
    }

    fn emit_with_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'with'
        let mut bindings: Vec<(String, Reg, MirWitness)> = Vec::new();
        loop {
            let name = self.consume_identifier("Expected config key after 'with'")?;
            self.consume(TokenType::Assign, "Expected '=' after config key")?;
            let (v, v_w) = self.emit_expr_w()?;
            bindings.push((name, v, v_w));
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        let _ = self.match_token(&[TokenType::Newline]);
        // 子上下文：body 是独立寄存器空间
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let mut body_wits = Vec::new();
        // v0.104.6 D44：失败时还原父上下文（见 emit_match_arm_w 处的说明）
        let with_ok = (|| -> Option<()> {
            while !self.check(&TokenType::End) && !self.is_at_end() {
                // v0.104.6 D45：保证进度（见 section 块同处说明）
                let before = self.current;
                match self.emit_statement_w() {
                    Some(w) => body_wits.push(w),
                    // v0.104.6 D45：同 section 块 —— 静默跳过且不消费 token
                    // 会让本循环**死循环**（with 块实测挂死）。
                    None => {
                        eprintln!(
                            "Parse error: unparsable statement in with block (line {})",
                            self.current_line()
                        );
                        return None;
                    }
                }
                while self.match_token(&[TokenType::Newline]) {}
                if self.current == before {
                    eprintln!(
                        "Parse error: parser made no progress in with block (line {})",
                        self.current_line()
                    );
                    return None;
                }
            }
            self.consume(TokenType::End, "Expected 'end' after with block")?;
            Some(())
        })();
        if with_ok.is_none() {
            self.emit = parent;
            return None;
        }
        let _ = self.emit.alloc_reg(); // 子 body 的返回值不传播
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        // 分离 binding 三元素组：MIR 指令侧 (name, reg) + witness 侧 (name, witness)
        let binding_pairs: Vec<(String, usize)> =
            bindings.iter().map(|(n, r, _)| (n.clone(), *r)).collect();
        let binding_witnesses: Vec<(String, MirWitness)> =
            bindings.into_iter().map(|(n, _r, w)| (n, w)).collect();
        self.emit.emit(MirInst::WithConfig {
            bindings: binding_pairs,
            body: Box::new(body_mir),
            jit: false,
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::WithConfig {
                bindings: binding_witnesses,
                body: Box::new(Self::block_witness(body_wits, span)),
            },
            span,
        })
    }
}
