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
                    return Err(format!("Failed to parse at line {}", self.current_line()));
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
            // v0.75.81: 事务家族 + eval 断言（顶层同嵌套分发）
            TokenType::Identifier(ref s)
                if s == "transaction"
                    || s == "commit"
                    || s == "rollback"
                    || s == "eval"
                    || s == "aggregate" =>
            {
                self.emit_statement_expr_w().map(|(_, w)| w)
            }
            // v0.80: algebraic effects 完整语法（Stage 2/4 落地）
            //   handle Effect { body } { handler } end
            //   perform Effect(args)
            // 不引入新 TokenType（不抢旧 identifier），与 transaction/eval 同模式。
            TokenType::Identifier(ref s) if s == "handle" => self.emit_handle_w(),
            TokenType::Identifier(ref s) if s == "perform" => self.emit_perform_w(),
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
    fn emit_expr_w(&mut self) -> Option<(Reg, MirWitness)> {
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
        // 显式将 expression 包装为 Value::TraitObject，与 legacy parse 路径
        // MirExprKind::DynTrait 对齐。
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

    fn emit_pipe_w(&mut self) -> Option<(Reg, MirWitness)> {
        let (mut left, mut left_w) = self.emit_comparison_w()?;
        while self.match_token_exact(TokenType::Pipe) {
            let (rhs, rhs_w) = self.emit_comparison_w()?;
            let dst = self.emit.alloc_reg();
            self.emit.emit(MirInst::Pipe(dst, left, rhs));
            let span = self.span_of_current();
            left_w = MirWitness {
                kind: WitnessKind::Call {
                    callee: crate::mir::witness::WitnessCallee::Name("|>".to_string()),
                    args: vec![left_w, rhs_w],
                },
                span,
            };
            left = dst;
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
                // p"..." 拆分为 parts 再 emit Prompt（parts 是纯树，非 token 流）
                let parts = parse_prompt_parts(s, span);
                let mut part_regs = Vec::new();
                let mut part_wits = Vec::new();
                for part in &parts {
                    let (r, w) = self.emit_expr_witness_w(part)?;
                    part_regs.push(r);
                    part_wits.push(w);
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
                // 子上下文：闭包体是独立寄存器空间（镜像 lower Closure 分支）
                let parent =
                    std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
                // v0.75.78: 非 FatArrow 闭包体走 emit_block_w（镜像 parse 侧
                // parse_block_body）— 支持多语句与嵌套构造（if/for/match/let）。
                // 修复前用 emit_expr_w：`fn(n) if n<=1 {..} else {..} end` 解析失败。
                // emit_block_w 已消费 End，无需外部 consume。
                let (body_reg, _body_w) = if self.match_token_exact(TokenType::FatArrow) {
                    self.emit_expr_w()?
                } else {
                    self.emit_block_w()?
                };
                self.emit.emit(MirInst::Return(Some(body_reg)));
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
                        body: Box::new(MirWitness {
                            kind: WitnessKind::Sequence(vec![]),
                            span,
                        }),
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
        self.emit.emit(MirInst::Call(
            dst,
            "quote".to_string(),
            vec![str_reg],
        ));

        let w = MirWitness {
            kind: WitnessKind::Call {
                callee: crate::mir::witness::WitnessCallee::Name("quote".to_string()),
                args: vec![MirWitness {
                    kind: WitnessKind::Literal(Literal::String(
                        quoted_text.to_string(),
                        span,
                    )),
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

                            let end = self.source_byte_at(tok.line, tok.column);
                            if end > capture_start {
                                let text = &self.source[capture_start..end];
                                if !text.is_empty() {
                                    segments
                                        .push(crate::mir::QuasiquoteSegment::Quote(
                                            text.to_string(),
                                        ));
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
                                    segments
                                        .push(crate::mir::QuasiquoteSegment::UnquoteSplice(reg));
                                } else {
                                    segments
                                        .push(crate::mir::QuasiquoteSegment::Unquote(reg));
                                }
                                witness_segments.push(w);
                                if let Some(next) = self.peek() {
                                    capture_start =
                                        self.source_byte_at(next.line, next.column);
                                }
                            } else {
                                return None;
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

        // Capture trailing static text up to the terminating token (Newline/EOF)
        if let Some(tok) = self.peek() {
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
        }

        let dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::Quasiquote { dst, segments });

        let w = MirWitness {
            kind: WitnessKind::Quasiquote { segments: witness_segments },
            span: start_span,
        };
        Some((dst, w))
    }

    /// 辅助：把已构造的 MirExpr（prompt parts：Literal/String 或 Variable）
    /// emit 成指令 + 返回嵌套 witness。
    fn emit_expr_witness_w(&mut self, expr: &MirExpr) -> Option<(Reg, MirWitness)> {
        let wit = MirWitness::from_expr(expr);
        match &expr.kind {
            MirExprKind::Literal(Literal::String(s, _)) => {
                let dst = self.emit.alloc_reg();
                self.emit
                    .emit(MirInst::Const(dst, crate::value::Value::String(s.clone())));
                Some((dst, wit))
            }
            MirExprKind::Variable(name) => {
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::Var(dst, name.clone()));
                Some((dst, wit))
            }
            _ => None,
        }
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
        let span = self.span_of_current();
        // dict key：Identifier 或 String 字面量
        let tok = self.peek().cloned()?;
        match tok.token_type {
            TokenType::Identifier(name) => {
                self.advance();
                Some(name)
            }
            TokenType::String(s) => {
                self.advance();
                let _ = span;
                Some(s)
            }
            _ => None,
        }
    }

    // ── 语句 emit ──

    fn emit_let_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'let'
        let name = self.consume_identifier("Expected variable name after 'let'")?;
        let type_hint = if self.match_token_exact(TokenType::Colon) {
            self.parse_type_annotation()
        } else {
            None
        };
        self.consume(TokenType::Assign, "Expected '=' in let binding")?;
        let (v, v_w) = self.emit_expr_w()?;
        // v0.85: `let x: dyn Trait = v` — 自动 emit MirInst::DynTrait
        // coercion，将 plain value 包装为 Value::TraitObject（§3.5 spec 承诺）。
        let (final_v, final_v_w) = if let Some(crate::typeck::Type::TraitObject {
            trait_name,
            ..
        }) = type_hint.clone()
        {
            let dst = self.emit.alloc_reg();
            self.emit.emit(MirInst::DynTrait {
                dst,
                src: v,
                trait_generics: Vec::new(),
                trait_name: trait_name.clone(),
            });
            (dst, MirWitness {
                kind: WitnessKind::DynTrait {
                    expr: Box::new(v_w),
                    trait_name,
                    generics: Vec::new(),
                },
                span,
            })
        } else {
            (v, v_w)
        };
        self.emit.emit(MirInst::Define(name.clone(), final_v));
        // init_body = Nil（与 lower LetBinding 一致：Const(Nil) → Assign → Var）
        let b_dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(b_dst, crate::value::Value::Nil));
        self.emit
            .emit(MirInst::Assign("__let_result".to_string(), b_dst));
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Var(dst, "__let_result".to_string()));
        let nil_w = MirWitness {
            kind: WitnessKind::Literal(Literal::Nil(span)),
            span,
        };
        Some(MirWitness {
            kind: WitnessKind::LetBinding {
                name,
                type_hint: type_hint.map(crate::mir::hint::TypeHint::from_type),
                value: Box::new(final_v_w),
                init_body: Box::new(nil_w),
            },
            span,
        })
    }

    fn emit_fn_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'task'
        let name = self.consume_identifier("Expected task name")?;
        self.consume(TokenType::LParen, "Expected '(' after task name")?;
        let mut params = Vec::new();
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            if let Some(p) = self.consume_identifier("Expected parameter name") {
                params.push(p);
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RParen, "Expected ')' after parameters")?;
        // 子上下文：函数体是独立寄存器空间（镜像 lower FnDef 分支）
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let (body_reg, body_w) = if self.match_token_exact(TokenType::Newline) {
            let mut stmt_wits = Vec::new();
            let mut last: Option<Reg> = None;
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::End) && !self.is_at_end() {
                let (r, w) = self.emit_statement_expr_w()?;
                last = Some(r);
                stmt_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.consume(TokenType::End, "Expected 'end' after task body")?;
            (last, Self::block_witness(stmt_wits, span))
        } else if self.check(&TokenType::End) {
            // Empty body: `task main() end`
            let nil_reg = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::Const(nil_reg, crate::value::Value::Nil));
            let nil_w = MirWitness {
                kind: WitnessKind::Literal(Literal::Nil(span)),
                span,
            };
            (Some(nil_reg), nil_w)
        } else {
            let (r, w) = self.emit_expr_w()?;
            (Some(r), w)
        };
        self.emit.emit(MirInst::Return(body_reg));
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        self.emit.emit(MirInst::TaskDef {
            name: name.clone(),
            params: params.clone(),
            body: Box::new(body_mir),
        });
        let w_params = params
            .iter()
            .map(|p| WitnessParam {
                name: p.clone(),
                type_hint: None,
                default: None,
            })
            .collect();
        Some(MirWitness {
            kind: WitnessKind::FnDef {
                name,
                params: w_params,
                return_type: None,
                body: Box::new(body_w),
            },
            span,
        })
    }

    fn emit_return_break_continue_w(&mut self) -> Option<MirWitness> {
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
                    self.emit_expr_w().map(|(_, w)| Box::new(w))
                };
                self.emit.emit(MirInst::Return(value.as_ref().map(|_| 0)));
                Some(MirWitness {
                    kind: WitnessKind::Return(value),
                    span,
                })
            }
            TokenType::Break => {
                self.advance();
                let label = if matches!(self.peek()?.token_type, TokenType::Identifier(_)) {
                    self.consume_identifier("Expected label after 'break'")?
                } else {
                    String::new()
                };
                let (_, brk) = self
                    .emit
                    .loop_stack
                    .last()
                    .copied()
                    .ok_or("Break outside loop")
                    .ok()?;
                self.emit.emit(MirInst::Break(brk));
                Some(MirWitness {
                    kind: WitnessKind::Break(label),
                    span,
                })
            }
            TokenType::Continue => {
                self.advance();
                let label = if matches!(self.peek()?.token_type, TokenType::Identifier(_)) {
                    self.consume_identifier("Expected label after 'continue'")?
                } else {
                    String::new()
                };
                let (cont, _) = self
                    .emit
                    .loop_stack
                    .last()
                    .copied()
                    .ok_or("Continue outside loop")
                    .ok()?;
                self.emit.emit(MirInst::Continue(cont));
                Some(MirWitness {
                    kind: WitnessKind::Continue(label),
                    span,
                })
            }
            _ => None,
        }
    }

    fn emit_type_alias_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'type'
        let name = self.consume_identifier("Expected type name")?;
        self.consume(TokenType::Assign, "Expected '=' in type alias")?;
        let target = self.parse_type_annotation()?;
        self.emit.emit(MirInst::TypeAlias {
            name: name.clone(),
            target: target.name(),
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::TypeAlias {
                name,
                target: crate::mir::hint::TypeHint::from_type(target),
            },
            span,
        })
    }

    fn emit_enum_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'enum'
        let name = self.consume_identifier("Expected enum name")?;
        let mut variants = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            if let Some(v) = self.consume_identifier("Expected variant name") {
                variants.push(v);
            }
        }
        self.consume(TokenType::End, "Expected 'end' after enum")?;
        let evs: Vec<crate::common::EnumVariant> = variants
            .iter()
            .map(|v| crate::common::EnumVariant {
                name: v.clone(),
                data: None,
            })
            .collect();
        self.emit.emit(MirInst::EnumDef {
            name: name.clone(),
            variants: evs,
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::EnumDef { name, variants },
            span,
        })
    }

    fn emit_struct_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'struct'
        let name = self.consume_identifier("Expected struct name")?;
        let mut fields = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            if let Some(fname) = self.consume_identifier("Expected field name") {
                self.consume(TokenType::Colon, "Expected ':' after field name")?;
                if let Some(ftype) = self.parse_type_annotation() {
                    fields.push((fname, ftype));
                }
            }
        }
        self.consume(TokenType::End, "Expected 'end' after struct")?;
        let sfs: Vec<crate::common::StructField> = fields
            .iter()
            .map(|(fname, ftype)| crate::common::StructField {
                name: fname.clone(),
                type_hint: ftype.name(),
            })
            .collect();
        self.emit.emit(MirInst::StructDef {
            name: name.clone(),
            fields: sfs,
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::StructDef {
                name,
                fields: fields
                    .into_iter()
                    .map(|(n, t)| (n, crate::mir::hint::TypeHint::from_type(t)))
                    .collect(),
            },
            span,
        })
    }

    /// v0.88: TEA app 定义 — `app Counter ... end` 完整解析。
    ///
    /// syntax:
    /// ```mora
    /// app Counter
    ///   model: Counter
    ///   msg: CounterMsg
    ///   init: <expr>
    ///   update: fn(model, msg) => ...
    ///   view: fn(model) => ...
    /// end
    /// ```
    ///
    /// 镜像 h_app_def 语义：model/msg 为 Identifier 绑定到当前 env；
    /// init/update/view 均作为闭包编译（子 EmitContext），emit MirInst::AppDef。
    fn emit_app_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'app'
        let name = self.consume_identifier("Expected app name")?;

        // 子上下文：app 块内部是独立寄存器空间
        let parent = std::mem::replace(
            &mut self.emit,
            crate::mir::lower::EmitContext::new(),
        );

        let mut model_name = String::new();
        let mut msg_name = String::new();
        let mut init_witness = None;
        let mut update_mir = None;
        let mut view_mir = None;

        while self.match_token(&[TokenType::Newline]) {}
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if let Some(field) = self.consume_identifier("Expected field name") {
                self.consume(TokenType::Colon, "Expected ':' after field name")?;
                match field.as_str() {
                    "model" => {
                        model_name = self
                            .consume_identifier("Expected model name")?
                            .clone();
                    }
                    "msg" => {
                        msg_name = self
                            .consume_identifier("Expected msg name")?
                            .clone();
                    }
                    "init" => {
                        init_witness = Some(Box::new(self.emit_expr_w().unwrap_or((0, {
                            let s = self.span_of_current();
                            MirWitness {
                                kind: WitnessKind::Literal(Literal::Nil(s)),
                                span: s,
                            }
                        })).1));
                    }
                    "update" => {
                        update_mir = self.emit_closure_mir("update");
                    }
                    "view" => {
                        view_mir = self.emit_closure_mir("view");
                    }
                    _ => {
                        // skip unknown fields
                    }
                }
            }
            while self.match_token(&[TokenType::Newline]) {}
        }
        self.consume(TokenType::End, "Expected 'end' after app block")?;

        // 将 init 表达式编译为 MirFunction（必须带 Return）
        let init_mir = init_witness
            .as_ref()
            .map(|b| {
                let mut mir = crate::mir::lower::lower_block_witness_to_mir(b);
                if mir.body.is_empty() || !matches!(mir.body.last(), Some(MirInst::Return(_))) {
                    let result_reg = mir.n_regs.saturating_sub(1);
                    mir.body.push(MirInst::Return(Some(result_reg)));
                }
                mir
            })
            .unwrap_or_default();

        // 恢复父上下文（app 块内部指令丢弃，仅保留 init/update/view 的 MirFunction）
        let _ = std::mem::replace(&mut self.emit, parent).finish();

        self.emit.emit(MirInst::AppDef {
            name: name.clone(),
            model_name: model_name.clone(),
            msg_name: msg_name.clone(),
            init_mir: Box::new(init_mir),
            update_mir: Box::new(update_mir.unwrap_or_default()),
            view_mir: Box::new(view_mir.unwrap_or_default()),
        });

        Some(MirWitness {
            kind: WitnessKind::AppDef {
                name: name.clone(),
                model_name,
                msg_name,
                init_w: init_witness.unwrap_or_else(|| {
                    Box::new(MirWitness {
                        kind: WitnessKind::Literal(Literal::Nil(span)),
                        span,
                    })
                }),
                update_w: Box::new(MirWitness {
                    kind: WitnessKind::Closure {
                        params: Vec::new(),
                        body: Box::new(MirWitness {
                            kind: WitnessKind::Literal(Literal::Nil(span)),
                            span,
                        }),
                    },
                    span,
                }),
                view_w: Box::new(MirWitness {
                    kind: WitnessKind::Closure {
                        params: Vec::new(),
                        body: Box::new(MirWitness {
                            kind: WitnessKind::Literal(Literal::Nil(span)),
                            span,
                        }),
                    },
                    span,
                }),
            },
            span,
        })
    }

    /// 解析闭包表达式并直接返回其 body 的 MirFunction（用于 app 的 update/view 字段）。
    /// 支持两种语法：
    ///   - fn(params) => body
    ///   - fn(params) block end
    fn emit_closure_mir(&mut self, _label: &str) -> Option<MirFunction> {
        // 消耗可选的 fn 关键字
        let _ = self.match_token_exact(TokenType::Fn);
        
        // 子上下文：闭包体是独立寄存器空间
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());

        // 解析参数列表（可选）
        let _params: Vec<String> = if self.match_token_exact(TokenType::LParen) {
            let mut params = Vec::new();
            while !self.check(&TokenType::RParen) && !self.is_at_end() {
                if let Some(p) = self.consume_identifier("Expected parameter") {
                    params.push(p);
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(TokenType::RParen, "Expected ')' after params")?;
            params
        } else {
            Vec::new()
        };

        // 解析闭包体
        let (body_reg, _body_w) = if self.match_token_exact(TokenType::FatArrow) {
            self.emit_expr_w()?
        } else {
            self.emit_block_w()?
        };

        self.emit.emit(MirInst::Return(Some(body_reg)));
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        Some(body_mir)
    }

    fn emit_import_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'import'
        let path = self.consume_identifier("Expected import path")?;
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
    fn emit_handle_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'handle'
        let effect = self.consume_identifier("Expected effect name after 'handle'")?;
        // body 块（必须花括号形式）
        self.consume(TokenType::LBrace, "Expected '{' after effect name")?;
        let (_, body_w) = self.emit_brace_block_w()?;
        self.consume(TokenType::RBrace, "Expected '}' after handle body")?;
        // handler 块（必须花括号形式）
        self.consume(TokenType::LBrace, "Expected '{' for handler block")?;
        let (_, handler_w) = self.emit_brace_block_w()?;
        self.consume(TokenType::RBrace, "Expected '}' after handler block")?;
        // v0.80 Stage 2.0: 直接在 parser 单遍编译中 emit MirInst::Handle。
        // (parser → EmitContext 单遍，绕开 lower.rs::lower_expr 的 Handle 分支
        // —— 那个分支是给旧 parse 路径用的，主路径走 emit_program 直接 emit)
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
                if let Some(p) = self.consume_identifier("Expected parameter name") {
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
        let parent = std::mem::replace(
            &mut self.emit,
            crate::mir::lower::EmitContext::new(),
        );
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
            (0, MirWitness {
                kind: WitnessKind::Sequence(Vec::new()),
                span,
            })
        };
        self.emit.emit(MirInst::Return(Some(body_reg)));
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
        let (then_reg, then_w) = self.emit_block_w()?;
        let dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::Copy(dst, then_reg));
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
            let (else_reg, w) = self.emit_block_w()?;
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

    /// emit 一个块（{} 或换行到 end），返回 (最后结果寄存器, 块 witness)。
    /// 块 witness：单条语句取该语句的 witness；多条语句嵌套为 Sequence。
    fn emit_block_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        let mut stmt_wits = Vec::new();
        let mut last = 0;
        if self.match_token_exact(TokenType::LBrace) {
            // v0.75.78: 与 parse_block_body/else 分支对称 —— 块内语句间允许
            // 换行（`if c {\n  stmt\n}`）。修复前 `{` 后不跳换行，多行
            // brace 块在 compile 主路径解析失败（旧 parse 路径可解析，
            // 差分测试只覆盖单行 if，未暴露）。
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::RBrace) && !self.is_at_end() {
                let (r, w) = self.emit_statement_expr_w()?;
                last = r;
                stmt_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.consume(TokenType::RBrace, "Expected '}' after block")?;
        } else {
            // 换行到 end（v0.87：或 EOF / 右括号 / 逗号 终止）
            while self.match_token(&[TokenType::Newline]) {}
            let is_block_end = |p: &ParserV3| -> bool {
                p.peek().map(|t| matches!(
                    &t.token_type,
                    TokenType::End
                        | TokenType::RParen
                        | TokenType::RBrace
                        | TokenType::Comma
                        | TokenType::EOF
                )).unwrap_or(true) // EOF 也是终止符
            };
            while !is_block_end(self) {
                let (r, w) = self.emit_statement_expr_w()?;
                last = r;
                stmt_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            // v0.87: end 可选——EOF 或右括号终止的闭包体（如 fn(x) x + 1）
            // 不要求显式 end；match_token_exact 在 token 不匹配时静默返回 false。
            self.match_token_exact(TokenType::End);
        }
        Some((last, Self::block_witness(stmt_wits, span)))
    }

    /// 块/函数体的语句列表 → 单条则直出，多条嵌套为 Sequence。
    fn block_witness(stmt_wits: Vec<MirWitness>, span: Span) -> MirWitness {
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
    fn emit_statement_expr_w(&mut self) -> Option<(Reg, MirWitness)> {
        if self.check(&TokenType::Let) {
            return self.emit_let_w().map(|w| (0, w));
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
            // v0.85: `with` 配置块（可嵌套在 task body 内）
            TokenType::With => self.emit_with_w().map(|w| (0, w)),
            TokenType::App => self.emit_app_def_w().map(|w| (0, w)),
            TokenType::Identifier(n) if n == "commit" => {
                let span = self.span_of_current();
                self.advance(); // 'commit'
                self.emit.emit(MirInst::Commit);
                let w = MirWitness {
                    kind: WitnessKind::Sequence(vec![]),
                    span,
                };
                Some((0, w))
            }
            TokenType::Identifier(n) if n == "rollback" => {
                let span = self.span_of_current();
                self.advance(); // 'rollback'
                self.emit.emit(MirInst::Rollback);
                let w = MirWitness {
                    kind: WitnessKind::Sequence(vec![]),
                    span,
                };
                Some((0, w))
            }
            _ => self.emit_expr_w(),
        }
    }

    fn emit_loop_w(&mut self) -> Option<(Reg, MirWitness)> {
        // 'for' var 'in' iterable newline body 'end'（镜像 lower Loop）
        let span = self.span_of_current();
        self.advance(); // 'for'
        let var = self.consume_identifier("Expected loop variable")?;
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

        let body_start = self.emit.insts.len();
        self.emit.loop_stack.push((loop_label, 0));
        let (_, body_w) = self.emit_block_w()?;
        self.emit.loop_stack.pop();
        let body_end = self.emit.insts.len();

        self.emit
            .emit(MirInst::BinaryOp(i_reg, i_reg, BinaryOp::Add, one_reg));
        self.emit.emit(MirInst::Jump(loop_label));
        let end_label = self.emit.insts.len();
        self.emit.patch_label_at(exit_jump_idx, end_label);
        for i in body_start..body_end {
            match &mut self.emit.insts[i] {
                MirInst::Break(lbl) => *lbl = end_label,
                MirInst::Continue(lbl) => *lbl = loop_label,
                _ => {}
            }
        }
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

        let body_start = self.emit.insts.len();
        self.emit.loop_stack.push((loop_label, 0));
        let (_, body_w) = self.emit_block_w()?;
        self.emit.loop_stack.pop();
        let body_end = self.emit.insts.len();

        self.emit.emit(MirInst::Jump(loop_label));
        let end_label = self.emit.insts.len();
        self.emit.patch_label_at(exit_jump_idx, end_label);
        for i in body_start..body_end {
            match &mut self.emit.insts[i] {
                MirInst::Break(lbl) => *lbl = end_label,
                MirInst::Continue(lbl) => *lbl = loop_label,
                _ => {}
            }
        }
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
        let mut last = 0;
        while self.match_token(&[TokenType::Newline]) {}
        while !self.check(&TokenType::End)
            && !self.peek_is_identifier("compensation")
            && !self.is_at_end()
        {
            let (r, w) = self.emit_statement_expr_w()?;
            last = r;
            body_wits.push(w);
            while self.match_token(&[TokenType::Newline]) {}
        }
        self.emit.emit(MirInst::Return(Some(last)));
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();

        // compensation 段（可选）：`compensation` 后语句循环到 `end`
        let comp_mir = if self.peek_is_identifier("compensation") {
            self.advance(); // 'compensation'
            let parent2 = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
            let mut comp_wits = Vec::new();
            let mut comp_last = 0;
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::End) && !self.is_at_end() {
                let (r, w) = self.emit_statement_expr_w()?;
                comp_last = r;
                comp_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.emit.emit(MirInst::Return(Some(comp_last)));
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
        Some((0, w))
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
        self.consume(TokenType::LBrace, "Expected '{' after match subject")?;
        let mut arms = Vec::new();
        let mut arm_wits = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            if let Some(arm) = self.emit_match_arm_w() {
                arms.push((arm.pat_str, arm.guard, arm.body_mir, arm.val_reg));
                arm_wits.push(arm.witness);
                let _ = self.match_token(&[TokenType::Comma]);
            } else {
                self.advance();
            }
        }
        self.consume(TokenType::RBrace, "Expected '}' after match arms")?;
        let dst = self.emit.alloc_reg();
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
        // v0.87: Detect optional "when <guard_expr>" before FatArrow
        let guard_reg = if let TokenType::Identifier(ref name) = self.peek()?.token_type
            && name == "when"
        {
            self.advance(); // consume "when"
            // Parse guard expression in a sub-context (isolated registers)
            let guard_parent =
                std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
            let (guard_val_reg, guard_w) = self.emit_expr_w()?;
            self.emit.emit(MirInst::Return(Some(guard_val_reg)));
            let _guard_mir =
                std::mem::replace(&mut self.emit, guard_parent).finish();
            self.consume(TokenType::FatArrow, "Expected '=>' after guard")?;
            Some((guard_val_reg, guard_w))
        } else {
            self.consume(TokenType::FatArrow, "Expected '=>' in match arm")?;
            None
        };
        // 子上下文：arm body 是独立寄存器空间（镜像 lower Match 分支）
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let (arm_val_reg, body_w) = self.emit_expr_w()?;
        self.emit.emit(MirInst::Return(Some(arm_val_reg)));
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        let pat_str = crate::mir::lower::pattern_to_string(&pattern);
        let witness = crate::mir::witness::WitnessArm {
            pattern: crate::mir::witness::WitnessPattern::from_pattern(&pattern),
            guard: guard_reg.as_ref().map(|(_, w)| w.clone()),
            body: body_w,
        };
        Some(EmittedMatchArm {
            pat_str,
            guard: guard_reg.map(|(r, _)| r),
            body_mir: Box::new(body_mir),
            val_reg: arm_val_reg,
            witness,
        })
    }

    fn emit_pattern(&mut self) -> Option<crate::mir::expr::Pattern> {
        // 复用完整 pattern 解析器（通配/字面量/变量/元组/列表/dict/类型标注）
        self.parse_pattern()
    }

    fn emit_orchestrate_w(&mut self) -> Option<MirWitness> {
        // 复用旧解析器构造 kind（含预 lower 的 task_body）— orchestrate 是
        // 数据构造指令，运行时引擎执行，不参与递归 emit（与 lower 的
        // MirExprKind::Orchestrate 分支一致：emit inst + Const(Nil)）。
        let span = self.span_of_current();
        let expr = self.parse_orchestrate_statement()?;
        if let MirExprKind::Orchestrate {
            input_var,
            result_var,
            kind,
        } = expr.kind
        {
            self.emit.emit(MirInst::Orchestrate {
                input_var: input_var.clone(),
                result_var: result_var.clone(),
                kind: kind.clone(),
            });
            let dst = self.emit.alloc_reg();
            self.emit
                .emit(MirInst::Const(dst, crate::value::Value::Nil));
            // witness：从构造的 MirExpr 转换（含嵌套 agent 树）
            Some(MirWitness {
                kind: WitnessKind::Orchestrate {
                    input_var,
                    result_var,
                    kind: Box::new(crate::mir::witness::WitnessOrchestrateKind::from_kind(
                        &kind,
                    )),
                },
                span,
            })
        } else {
            None
        }
    }

    /// v0.85: `as dyn Trait` coercion（§3.5 spec 承诺）。
    /// 检测当前 token 序列为 `as dyn <TraitName>`，emit MirInst::DynTrait
    /// 指令，将 src_reg 的 plain value 包装为 Value::TraitObject。
    /// 未检测到则原样返回。
    fn emit_dyn_coercion(
        &mut self,
        src_reg: Reg,
        src_w: MirWitness,
    ) -> Option<(Reg, MirWitness)> {
        if self.check(&TokenType::As) {
            self.advance(); // 'as'
            if self.check(&TokenType::Dyn) {
                self.advance(); // 'dyn'
                let trait_name =
                    self.consume_identifier("Expected trait name after 'dyn'")?;
                let dst = self.emit.alloc_reg();
                self.emit.emit(MirInst::DynTrait {
                    dst,
                    src: src_reg,
                    trait_generics: Vec::new(),
                    trait_name: trait_name.clone(),
                });
                let span = src_w.span;
                return Some((dst, MirWitness {
                    kind: WitnessKind::DynTrait {
                        expr: Box::new(src_w),
                        trait_name,
                        generics: Vec::new(),
                    },
                    span,
                }));
            }
        }
        Some((src_reg, src_w))
    }

    /// v0.85: `with key = value, key2 = value2` body end — 配置桥接块（§19.4 spec 承诺）。
    /// bindings 是 (name, value_reg) 列表，body 是嵌套 MirFunction。
    /// MirInst::WithConfig 经解释器保存到 current_ai_config 中（mock_llm/mock_responses）。
    /// 与 handle/perform 同模式（Identifier 派生），不引入新 TokenType。
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
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if let Some(w) = self.emit_statement_w() {
                body_wits.push(w);
            }
            while self.match_token(&[TokenType::Newline]) {}
        }
        self.consume(TokenType::End, "Expected 'end' after with block")?;
        let _ = self.emit.alloc_reg(); // 子 body 的返回值不传播
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        // 分离 binding 三元素组：MIR 指令侧 (name, reg) + witness 侧 (name, witness)
        let binding_pairs: Vec<(String, usize)> = bindings
            .iter()
            .map(|(n, r, _)| (n.clone(), *r))
            .collect();
        let binding_witnesses: Vec<(String, MirWitness)> = bindings
            .into_iter()
            .map(|(n, _r, w)| (n, w))
            .collect();
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
