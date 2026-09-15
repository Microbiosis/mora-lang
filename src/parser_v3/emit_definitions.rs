//! v0.92: Definition emit methods extracted from parser_v3/emit.rs (P1.3 split).
//!
//! emit.rs 是 2200+ 行巨型 impl ParserV3 block（P0 时期遗留）。
//! 这里拆出定义类 emit 方法（let/task/return/type/enum/struct/app/closure_mir）。
//!
//! 这些方法全部是 `impl ParserV3` 的固有方法——Rust 允许 impl 块在多个文件中
//! 按模块拆分，只要都在 crate 内声明（见 `parser_v3/mod.rs`）。

use super::*;

impl ParserV3 {
    /// v0.98: 显式 effect 签名声明 —— `effect Name(Hint, ...): Hint`。
    ///
    /// 纯类型层契约：不产任何 MirInst（效果签名只被 typeck 消费 ——
    /// perform 位点校验、handler `__arg0..N` 类型化、结果类型静态化）。
    /// 返回标注用 `:` 风格（与 task 返回标注一致）；缺省 = Any
    /// （仅声明载荷契约，结果多态）。
    pub(super) fn emit_effect_sig_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'effect'
        let name = self.consume_identifier("Expected effect name after 'effect'")?;
        self.consume(TokenType::LParen, "Expected '(' after effect name")?;
        let mut params: Vec<crate::mir::hint::TypeHint> = Vec::new();
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            let ty = self.parse_type_annotation()?;
            params.push(crate::mir::hint::TypeHint::from_type(ty));
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RParen, "Expected ')' after effect params")?;
        let result = if self.match_token_exact(TokenType::Colon) {
            Some(crate::mir::hint::TypeHint::from_type(
                self.parse_type_annotation()?,
            ))
        } else {
            None
        };
        // 与 TypeAlias 同先例：定义语句落一条 Nil 常量 —— 保持"声明即指令"
        // 的不变量（空模块守卫 `func.body.is_empty()` 依赖它；纯签名模块
        // 否则会被 "no executable instructions" 拒绝）。
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));
        Some(MirWitness {
            kind: WitnessKind::EffectSig {
                name,
                params,
                result,
            },
            span,
        })
    }

    pub(super) fn emit_let_w(&mut self) -> Option<MirWitness> {
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

    pub(super) fn emit_fn_def_w(&mut self) -> Option<MirWitness> {
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

    pub(super) fn emit_return_break_continue_w(&mut self) -> Option<MirWitness> {
        let token = self.peek()?.token_type.clone();
        let span = self.span_of_current();
        match token {
            TokenType::Return => {
                self.advance();
                // v0.102 修复：`return <expr>` 必须返回**表达式的求值寄存器**。
                // 此前硬编码 `map(|_| 0)` —— 丢弃 emit_expr_w 的结果寄存器，
                // 一律返回 reg 0。单表达式体（`task f(n) return n*n`）中函数
                // 首参恰好落在 reg 0 而掩盖了缺陷；一旦表达式含多条指令
                // （如 `return n + 100i`，n 在 reg 0、结果在 reg 2），就返回
                // 了错误的值（实为第一个操作数）。与 `break`/`continue` 的
                // 寄存器获取方式对齐：取真实结果寄存器。
                let value = if self.check(&TokenType::Newline)
                    || self.check(&TokenType::RBrace)
                    || self.is_at_end()
                {
                    None
                } else {
                    self.emit_expr_w()
                        .map(|(reg, w)| (reg, Box::new(w)))
                };
                self.emit
                    .emit(MirInst::Return(value.as_ref().map(|(reg, _)| *reg)));
                Some(MirWitness {
                    kind: WitnessKind::Return(value.map(|(_, w)| w)),
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

    pub(super) fn emit_type_alias_w(&mut self) -> Option<MirWitness> {
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

    pub(super) fn emit_enum_def_w(&mut self) -> Option<MirWitness> {
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

    pub(super) fn emit_struct_def_w(&mut self) -> Option<MirWitness> {
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
            } else {
                // v0.90 死循环修复：非标识符 token 前进保证进度
                self.advance();
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
    pub(super) fn emit_app_def_w(&mut self) -> Option<MirWitness> {
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
        // v0.103: update/view 的真实 witness（此前被丢弃 → typeck 只能看到
        // 伪造的零参 Nil 闭包占位，用户写的 update/view 体完全不参与推断）
        let mut update_witness = None;
        let mut view_witness = None;

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
                        if let Some((m, w)) = self.emit_closure_pair() {
                            update_mir = Some(m);
                            update_witness = Some(Box::new(w));
                        }
                    }
                    "view" => {
                        if let Some((m, w)) = self.emit_closure_pair() {
                            view_mir = Some(m);
                            view_witness = Some(Box::new(w));
                        }
                    }
                    _ => {
                        // skip unknown fields
                    }
                }
            } else {
                // v0.90 死循环修复：字段值解析失败残留的非标识符 token
                //（如未支持的元组表达式）—— 前进一 token 保证进度。
                // 此前此处不前进，app 块内任何解析失败都会死循环。
                self.advance();
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
                update_w: update_witness.unwrap_or_else(|| {
                    // 未提供 update 时的显式空闭包（不是占位遮罩 —— 等价于
                    // 用户写 `update: fn() => nil`，类型与效果都可推断）
                    Box::new(MirWitness {
                        kind: WitnessKind::Closure {
                            params: Vec::new(),
                            body: Box::new(MirWitness {
                                kind: WitnessKind::Literal(Literal::Nil(span)),
                                span,
                            }),
                        },
                        span,
                    })
                }),
                view_w: view_witness.unwrap_or_else(|| {
                    Box::new(MirWitness {
                        kind: WitnessKind::Closure {
                            params: Vec::new(),
                            body: Box::new(MirWitness {
                                kind: WitnessKind::Literal(Literal::Nil(span)),
                                span,
                            }),
                        },
                        span,
                    })
                }),
            },
            span,
        })
    }

    /// 解析闭包表达式，同时返回其 body 的 MirFunction 与 witness。
    /// 支持两种语法：
    ///   - fn(params) => body
    ///   - fn(params) block end
    ///
    /// v0.103: 返回 `(MirFunction, MirWitness)` —— 此前只返回 MirFunction 且
    /// 丢弃 witness（`_body_w`），导致 `app` 定义只能把 update/view 的 witness
    /// 伪造为零参、body 为 Nil 的闭包占位。后果是 typeck 看不到用户写的
    /// update/view 体：其形参不参与推断、体内错误不被诊断、效果行不传播。
    pub(super) fn emit_closure_pair(&mut self) -> Option<(MirFunction, MirWitness)> {
        let span = self.span_of_current();
        // 消耗可选的 fn 关键字
        let _ = self.match_token_exact(TokenType::Fn);

        // 子上下文：闭包体是独立寄存器空间
        let parent = std::mem::replace(&mut self.emit, crate::mir::lower::EmitContext::new());

        // 解析参数列表（可选）
        let params: Vec<String> = if self.match_token_exact(TokenType::LParen) {
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
        let (body_reg, body_w) = if self.match_token_exact(TokenType::FatArrow) {
            self.emit_expr_w()?
        } else {
            self.emit_block_w()?
        };

        self.emit.emit(MirInst::Return(Some(body_reg)));
        let body_mir = std::mem::replace(&mut self.emit, parent).finish();
        let wit = MirWitness {
            kind: WitnessKind::Closure {
                params: params
                    .into_iter()
                    .map(|name| WitnessParam {
                        name,
                        type_hint: None,
                        default: None,
                    })
                    .collect(),
                body: Box::new(body_w),
            },
            span,
        };
        Some((body_mir, wit))
    }
}
