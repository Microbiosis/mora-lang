//! v0.54: Parser V3 - Pure MIR expression parser (Phase γ Complete)
//!
//! **Zero AST v2 dependencies** - Direct tokens → MirExpr conversion
//! This is the final parser implementation that completely replaces Parser v2.

use crate::common::{BinaryOp, Literal, Span};
use crate::lexer::{Lexer, Token, TokenType};
use crate::mir::expr::*;
use crate::mir::witness::{MirWitness, WitnessKind, WitnessParam};
use crate::mir::{MirFunction, MirInst, Reg};
use std::collections::HashMap;

///  ParserV3 - Clean-room MIR parser with no AST legacy baggage
pub struct ParserV3 {
    tokens: Vec<Token>,
    current: usize,
    /// v0.75.40: 单遍编译 emit 上下文（阶段 3 完整融合）。
    /// parse 函数在构造语法树的同时 emit MirInst 到此处；compile() 取走
    /// 指令序列。旧路径 parse() 忽略此字段（仅构造 MirExpr）。
    emit: crate::mir::lower::EmitContext,
    /// v0.75.40: 单遍编译并行产出的 witness 树（typeck/LSP 消费面）。
    /// compile() 返回此列表；旧路径 parse() 不填充。
    witnesses: Vec<MirWitness>,
}

impl ParserV3 {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            current: 0,
            emit: crate::mir::lower::EmitContext::new(),
            witnesses: Vec::new(),
        }
    }

    /// v0.75.40: 单遍编译入口（阶段 3 完整融合）。
    ///
    /// parse 函数直接 emit MirInst 到内部 EmitContext，并行产出
    /// MirWitness 树，MirExpr 中间层消失（执行路径零 MirExpr）。
    /// 差分测试（tests/compile_differential.rs）锁定与 parse→lower 等价。
    pub fn compile(
        source: &str,
    ) -> Result<
        (
            crate::mir::MirFunction,
            Vec<crate::mir::witness::MirWitness>,
        ),
        String,
    > {
        use crate::lexer::Lexer;
        let tokens = Lexer::new(source).scan_tokens();
        let mut parser = ParserV3::new(tokens);
        parser.emit_program()?;
        let func = parser.emit.finish();
        let witnesses = parser.witnesses;
        Ok((func, witnesses))
    }

    /// v0.75.40: 顶层语句循环 — emit 每条语句 + 顶层 witness。
    fn emit_program(&mut self) -> Result<(), String> {
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
            TokenType::Return | TokenType::Break | TokenType::Continue => {
                self.emit_return_break_continue_w()
            }
            TokenType::Type => self.emit_type_alias_w(),
            TokenType::Enum => self.emit_enum_def_w(),
            TokenType::Struct => self.emit_struct_def_w(),
            TokenType::Import => self.emit_import_w(),
            TokenType::Macro => self.emit_macro_def_w(),
            TokenType::Orchestrate => self.emit_orchestrate_w(),
            _ => {
                // 表达式语句（赋值/字面量/调用等）
                self.emit_expr_w().map(|(_, w)| w)
            }
        }
    }

    /// 表达式入口（witness 嵌套版）— 镜像 parse_assignment（含赋值检测）。
    fn emit_expr_w(&mut self) -> Option<(Reg, MirWitness)> {
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
        self.emit_or_w()
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
            if let Some((r, w)) = self.emit_expr_w() {
                args.push(r);
                wits.push(w);
            }
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
                        type_hint: p.type_hint.clone(),
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
            _ => return None,
        };
        Some((reg, wit))
    }

    /// 旧签名薄包装：只取寄存器（不建嵌套 witness）。
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
        self.emit.emit(MirInst::Define(name.clone(), v));
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
                type_hint,
                value: Box::new(v_w),
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
            let mut last = 0;
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::End) && !self.is_at_end() {
                let (r, w) = self.emit_statement_expr_w()?;
                last = r;
                stmt_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.consume(TokenType::End, "Expected 'end' after task body")?;
            (last, Self::block_witness(stmt_wits, span))
        } else {
            self.emit_expr_w()?
        };
        self.emit.emit(MirInst::Return(Some(body_reg)));
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
            kind: WitnessKind::TypeAlias { name, target },
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
            kind: WitnessKind::StructDef { name, fields },
            span,
        })
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
        // 跳过 macro body 到 end
        while !self.check(&TokenType::End) && !self.is_at_end() {
            self.advance();
        }
        self.consume(TokenType::End, "Expected 'end' after macro")?;
        self.emit.emit(MirInst::MacroDef {
            name: name.clone(),
            params: params.clone(),
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::MacroDef { name, params },
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
            // 换行到 end
            while self.match_token(&[TokenType::Newline]) {}
            while !self.check(&TokenType::End) && !self.is_at_end() {
                let (r, w) = self.emit_statement_expr_w()?;
                last = r;
                stmt_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.consume(TokenType::End, "Expected 'end' after block")?;
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
        self.consume(TokenType::FatArrow, "Expected '=>' in match arm")?;
        // 子上下文：arm body 是独立寄存器空间（镜像 lower Match 分支）
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());
        let (arm_val_reg, body_w) = self.emit_expr_w()?;
        self.emit.emit(MirInst::Return(Some(arm_val_reg)));
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        let pat_str = crate::mir::lower::pattern_to_string(&pattern);
        let witness = crate::mir::witness::WitnessArm {
            pattern: crate::mir::witness::WitnessPattern::from_pattern(&pattern),
            guard: None,
            body: body_w,
        };
        Some(EmittedMatchArm {
            pat_str,
            guard: None,
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

        // Parse kind: sequential | loop | graph | pregel | moa
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
        // v0.75.84: MoA 声明字段（kind == "moa"）
        let mut moa_layers: Option<usize> = None;
        let mut moa_proposers: Vec<String> = Vec::new();
        let mut moa_aggregator: Option<String> = None;
        let mut moa_prompt: Option<MirExpr> = None;
        // v0.75.85: MoE 声明字段（kind == "moe"）
        let mut moe_experts: Vec<crate::mir::expr::MirMoeExpert> = Vec::new();
        let mut moe_router: Option<MirExpr> = None;
        let mut moe_top_k: Option<usize> = None;
        let mut moe_prompt: Option<MirExpr> = None;

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

            // v0.75.84: MoA 字段声明（layers / proposers / aggregator / prompt）
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
                            // v0.38 numeric tower：整数字面量词法为 Float
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

            // v0.75.85: MoE 字段声明（experts / router / top_k / prompt）
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
            // v0.75.80: 命中 `agent` 关键字但解析失败（含 task_body lowering
            // 失败）视为整个 orchestrate 语句失败 —— 不再静默跳过产生残缺图。
            if self.peek_is_identifier("agent") {
                if let Some(agent) = self.parse_agent_def() {
                    agents.push(agent);
                    continue;
                }
                return None;
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
            // v0.75.84: MoA — 声明参数直接构造（展开在 h_orchestrate）。
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
            // v0.75.85: MoE — 声明参数直接构造（执行在 h_orchestrate）。
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

        // v0.75.32: 修复 task_expr → task_body 降级缺失 — 此前 task_body 恒空
        // （pregel 执行报 "lowering missing"）。产出时立即 lower 填入。
        // v0.75.80: lower 失败不再兜底空函数哨兵 —— 具体错误上抛，agent
        // 定义整体失败（调用点据此使整个 orchestrate 语句失败，compile 报错）。
        let lowered_body = match crate::mir::lower::lower_mir_exprs(std::slice::from_ref(&body)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Parse error: orchestrate agent '{name}' task_body lowering failed: {e}");
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

/// emit_match_arm_w 的产出：指令侧 (pat_str/guard/body_mir/val_reg)
/// + witness 侧 WitnessArm。结构体分组避免五元组返回（type_complexity）。
struct EmittedMatchArm {
    pat_str: String,
    guard: Option<Reg>,
    body_mir: Box<MirFunction>,
    val_reg: Reg,
    witness: crate::mir::witness::WitnessArm,
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
