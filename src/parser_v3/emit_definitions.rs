//! v0.110.0: `ParserV3` 顶层与局部定义解析子模块（自 `parser_v3/emit.rs` 拆分）。
//!
//! 承载 `let`、`task`、`return`、`type`、`enum`、`struct`（含 `@tag`）、`trait`、`impl` 与闭包定义的语法解析与 Witness 生成。
//!
//! 这些方法全部是 `impl ParserV3` 的固有方法——Rust 允许 impl 块在多个文件中
//! 按模块拆分，只要都在 crate 内声明（见 `parser_v3/mod.rs`）。

use super::*;

impl ParserV3 {
    /// v0.98: 显式 effect 签名声明 —— `effect Name(Hint, ...): Hint`。
    ///
    /// 纯类型层契约：只被 typeck 消费（perform 位点校验、handler `__arg0..N`
    /// 类型化、结果类型静态化）。
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
        // v0.104.6 D353：此前走 `consume_identifier`，而它的兜底分支
        // （`tokens.rs`）会调 `token_to_identifier_name()` 把**关键字 token
        // 映射回标识符名**（`if` / `let` / `fn` / `end` / … 共 20+ 个）。
        //
        // 那个映射的**本意**是别的位置（见 `parser_v3/mod.rs:296` 的注释）：
        // 关键字可以出现在「方法名」或「`.` / `::` 之后的引用」位置。
        // 但 `let` 的**声明位**借用了同一条路径 ⇒ 宽容**泄漏**到声明位，
        // 产出**僵尸绑定**（zombie binding）：
        //
        // ```text
        // let if = 5        → exit 0，**接受**（声明了一个变量）
        // print(if)         → Failed to parse at line 2
        // let zz = 5
        // print(zz)         → 5.0                            ← 正常对照
        // ```
        //
        // 声明**成功**、引用**永远失败** ⇒ 变量不可用，且诊断**指错了行**
        // （问题在 line 1，错误报在 line 2）。这比直接报错更糟：
        // 用户会去查 line 2，而那里完全没问题。
        //
        // 修法：声明位**只认** `TokenType::Identifier`（真正的标识符），
        // 关键字一律报错。**零依赖** —— 实测 `tests/fixtures/**` 与
        // `examples/**` 下 **56 个** `.mora` 文件里，
        // **没有任何一个**用关键字作 `let` 变量名。
        let name = self.consume_plain_identifier("Expected variable name after 'let'")?;
        let type_hint = if self.match_token_exact(TokenType::Colon) {
            // v0.104.6 D111：此前是裸调用 `self.parse_type_annotation()`，
            // 它返回 `None` 时（**不受支持的泛型标注**，
            // 见 `syntax.rs` 的 `unsupported generic type annotation` 分支）
            // 被当成「这一行没有标注」—— **标注被静默丢弃**，程序照常跑、
            // exit 0，且 `let` 绑定的类型就是右值的推断类型。
            //
            // 实测：`let r: result<number, string> = 1` → exit 0、零报错、`r = 1.0`；
            // 而 `let r: string = 1` 会被 typeck 正确拒绝。
            // 同一类缺陷 D46 已在 `emit_struct_def_w` / `emit_enum_def_w` 修过，
            // **此处未同步**。改为报错。
            match self.parse_type_annotation() {
                Some(t) => Some(t),
                None => {
                    eprintln!(
                        "Parse error: unparsable type annotation for let binding \
                         `{name}` (line {})",
                        self.current_line()
                    );
                    return None;
                }
            }
        } else {
            None
        };
        self.consume(TokenType::Assign, "Expected '=' in let binding")?;
        let v_w = self.emit_expr_w()?;
        // v0.85: `let x: dyn Trait = v` — 自动产生 `WitnessKind::DynTrait`
        // coercion，把 plain value 包装为 Value::TraitObject（§3.5 spec 承诺）。
        let final_v_w = if let Some(crate::typeck::Type::TraitObject {
            trait_name,
            generics,
        }) = type_hint.clone()
        {
            MirWitness {
                kind: WitnessKind::DynTrait {
                    expr: Box::new(v_w),
                    trait_name,
                    generics: generics
                        .into_iter()
                        .map(crate::mir::hint::TypeHint::from_type)
                        .collect(),
                },
                span,
            }
        } else {
            v_w
        };
        // init_body = Nil（与 lower LetBinding 一致：Const(Nil) → Assign → Var）
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

    /// v0.104: `assign <name> = <expr>` —— 显式可变更新（spec §2 命令式面、
    /// §14.2 EBNF `assign_stmt = "assign" IDENTIFIER "=" expr`）。
    ///
    /// **缺陷**：`assign` 从未作为**语句前缀**被识别。词法层 `assign` 是普通
    /// 标识符（`TokenType::Identifier("assign")`），分派表里没有它的 arm，
    /// 于是整条语句落回表达式路径 —— 发出 `Var("assign")` 读一个不存在的
    /// 变量，紧接着 `acc = acc + i` 走「裸赋值」分支。后果分三档：
    ///   ① typeck 报 `Unbound variable 'assign'`（顶层、task 体内均如此）；
    ///   ② 仅当该语句位于**循环体**内时，循环体的类型检查此前是 v0.55 桩
    ///      （见 `infer_expr` 的 Loop/While 分支）而被跳过，于是「看似可用」；
    ///   ③ 语义上多读一个不存在的变量，且 witness 树里多一个假的
    ///      `Variable("assign")` 节点（`child_witnesses` 遍历者都会看到）。
    ///
    /// 现在按 spec 形状解析：`assign` → 标识符 → `=` → 表达式，
    /// 产出 `WitnessKind::Assign`（与裸赋值同一契约，目标必须是已绑定名字，
    /// 由 `infer_assign` 校验）。
    pub(super) fn emit_assign_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'assign'
        let name = self.consume_identifier("Expected variable name after 'assign'")?;
        self.consume(TokenType::Assign, "Expected '=' in assign statement")?;
        let v_w = self.emit_expr_w()?;
        Some(MirWitness {
            kind: WitnessKind::Assign {
                target: name,
                value: Box::new(v_w),
            },
            span,
        })
    }

    /// Shared parameter list parser for `task`, `fn` closures, `trait` methods, and `impl` methods.
    /// Supports optional `: Type` annotations and Python-style `= default_expr` defaults,
    /// enforcing that no required parameter may follow a defaulted parameter.
    pub(super) fn parse_witness_params(
        &mut self,
        allow_self: bool,
        strict_names: bool,
    ) -> Option<(Vec<WitnessParam>, bool)> {
        let mut params: Vec<WitnessParam> = Vec::new();
        let mut has_self = false;
        let mut seen_default = false;
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            let pname = if allow_self
                && params.is_empty()
                && matches!(
                    self.peek().map(|t| &t.token_type),
                    Some(TokenType::Identifier(s)) if s == "self"
                ) {
                self.advance();
                has_self = true;
                "self".to_string()
            } else if strict_names {
                self.consume_plain_identifier("Expected parameter name")?
            } else {
                self.consume_identifier("Expected parameter name")?
            };

            if params.iter().any(|p| p.name == pname) {
                let msg = format!(
                    "Parse error: duplicate parameter '{}' at line {}",
                    pname,
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                return None;
            }

            let type_hint = if self.match_token_exact(TokenType::Colon) {
                Some(crate::mir::hint::TypeHint::from_type(
                    self.parse_type_annotation()?,
                ))
            } else {
                None
            };

            let default = if self.match_token_exact(TokenType::Assign) {
                if pname == "self" {
                    let msg = format!(
                        "Parse error: 'self' parameter cannot have a default value at line {}",
                        self.current_line()
                    );
                    eprintln!("{msg}");
                    self.diag = Some(msg);
                    return None;
                }
                seen_default = true;
                Some(self.emit_expr_w()?)
            } else {
                if seen_default {
                    let msg = format!(
                        "Parse error: non-default parameter '{}' follows default parameter at line {}",
                        pname,
                        self.current_line()
                    );
                    eprintln!("{msg}");
                    self.diag = Some(msg);
                    return None;
                }
                None
            };

            params.push(WitnessParam {
                name: pname,
                type_hint,
                default,
            });
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        Some((params, has_self))
    }

    pub(super) fn emit_fn_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'task'
        let name = self.consume_identifier("Expected task name")?;
        let saved_generic = std::mem::take(&mut self.generic_params);
        let type_params = match self.parse_generic_clause() {
            Some(tp) => tp,
            None => {
                self.generic_params = saved_generic;
                return None;
            }
        };
        if self
            .consume(TokenType::LParen, "Expected '(' after task name")
            .is_none()
        {
            self.generic_params = saved_generic;
            return None;
        }
        let w_params = match self.parse_witness_params(false, false) {
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
        let return_type = if self.match_token_exact(TokenType::Colon) {
            match self.parse_type_annotation() {
                Some(t) => Some(crate::mir::hint::TypeHint::from_type(t)),
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            }
        } else {
            None
        };
        let body_res = self.in_nested_scope(|this| {
            let body_w = if this.match_token_exact(TokenType::Newline) {
                let mut stmt_wits = Vec::new();
                while this.match_token(&[TokenType::Newline]) {}
                while !this.check(&TokenType::End) && !this.is_at_end() {
                    let w = this.emit_statement_expr_w()?;
                    stmt_wits.push(w);
                    while this.match_token(&[TokenType::Newline]) {}
                }
                this.consume(TokenType::End, "Expected 'end' after task body")?;
                Self::block_witness(stmt_wits, span)
            } else if this.check(&TokenType::End) {
                MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(span)),
                    span,
                }
            } else {
                this.emit_expr_w()?
            };
            Some(body_w)
        });
        self.generic_params = saved_generic;
        let body_w = body_res?;
        Some(MirWitness {
            kind: WitnessKind::FnDef {
                name,
                type_params,
                params: w_params,
                return_type,
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
                // v0.104.6 D42：`return` 只在函数体里有意义。判据用
                // `is_program_top`（程序顶层是唯一不属于任何函数体的作用域），
                // 与下方 `break`/`continue` 的 `parse_loop_depth` 守卫同款。
                //
                // 修前的实测（真实 CLI `mora run`，**全部 exit 0、零提示**）：
                //
                // ```mora
                // print(111)   → 111.0
                // return 1
                // print(999)   → 从不执行
                // ```
                //
                // `if` / `for` 的体同样内联在程序顶层，故
                // `if c == 1 then return 7 end` + `print(888)` 也会把 888 吞掉。
                // 用户写「提前返回」却看到程序「成功结束」，与 D35
                // （`let r = handle …` 让其后所有语句静默消失）同族。
                //
                // 为什么**不**保留「顶层 return = 结束程序」：那与
                // 「正常跑完」在退出码和输出上完全不可区分。
                //
                // v0.104.6 D474：此守卫一度被 parser 手术连带删掉 ——
                // 顶层 `return` 于是又能被接受，又会**静默终止整个程序**
                // （D42 的原始缺陷原地复活）。故在此复原。
                if self.scope.is_program_top {
                    eprintln!(
                        "Parse error: `return` is only valid inside a `task` / closure \
                         body (line {}) — at the top level it silently terminated the \
                         whole program, discarding every following statement",
                        self.current_line()
                    );
                    return None;
                }
                self.advance();
                // `return <expr>` 的值 = 该表达式的求值结果（witness 侧直接带上
                // 该表达式，不硬编码任何常量 —— 硬编码会在多操作数表达式上
                // 返回错误的值）。
                let value = if self.check(&TokenType::Newline)
                    || self.check(&TokenType::RBrace)
                    || self.is_at_end()
                {
                    None
                } else {
                    self.emit_expr_w().map(Box::new)
                };
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
                // v0.104.6 D474：判据从「IR 的 `loop_stack`」改成「解析期的
                // 循环深度」（`ParserScope::parse_loop_depth`）。
                //
                // 「循环外 break 是错误」是**解析语义** —— 判的是源码位置，
                // 与 IR 的跳转标签无关。此前它借 `loop_stack`（IR 标签栈）当
                // 「是否在循环内」的标记；parser 不再发射指令后那个栈在
                // parser 侧永远为空，合法写法里的 `break` 一律被拒。
                //
                // 深度记在 `ParserScope` 里 ⇒ 嵌套的函数体（task / closure /
                // macro / match arm …）深度归零，函数体里的 `break` 跳不出外层循环。
                if self.scope.parse_loop_depth == 0 {
                    eprintln!(
                        "Parse error: `break` is only valid inside a loop body \
                         (line {})",
                        self.current_line()
                    );
                    return None;
                }
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
                // v0.104.6 D474：同上，判据改为解析期循环深度。
                if self.scope.parse_loop_depth == 0 {
                    eprintln!(
                        "Parse error: `continue` is only valid inside a loop body \
                         (line {})",
                        self.current_line()
                    );
                    return None;
                }
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
        self.declared_type_names.insert(name.clone());
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
        self.declared_type_names.insert(name.clone());
        let saved_generic = std::mem::take(&mut self.generic_params);
        let type_params = match self.parse_generic_clause() {
            Some(tp) => tp,
            None => {
                self.generic_params = saved_generic;
                return None;
            }
        };
        let mut variants: Vec<crate::mir::witness::WitnessEnumVariant> = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            let before = self.current;
            let vspan = self.span_of_current();
            let vname = match self.consume_identifier("Expected variant name") {
                Some(v) => v,
                None => {
                    self.generic_params = saved_generic;
                    eprintln!(
                        "Parse error: unparsable enum variant (line {})",
                        self.current_line()
                    );
                    return None;
                }
            };
            if variants.iter().any(|v| v.name == vname) {
                let msg = format!(
                    "Parse error: duplicate enum variant '{}' in '{}' (line {})",
                    vname,
                    name,
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                self.generic_params = saved_generic;
                return None;
            }
            let mut payload_types = Vec::new();
            if self.match_token_exact(TokenType::LParen) {
                while !self.check(&TokenType::RParen) && !self.is_at_end() {
                    match self.parse_type_annotation() {
                        Some(ty) => payload_types.push(crate::mir::hint::TypeHint::from_type(ty)),
                        None => {
                            self.generic_params = saved_generic;
                            return None;
                        }
                    }
                    if !self.match_token(&[TokenType::Comma]) {
                        break;
                    }
                }
                if self
                    .consume(TokenType::RParen, "Expected ')' after enum variant payload")
                    .is_none()
                {
                    self.generic_params = saved_generic;
                    return None;
                }
            }
            let _ = self.match_token(&[TokenType::Comma]);
            variants.push(crate::mir::witness::WitnessEnumVariant {
                name: vname,
                payload_types,
                span: vspan,
            });
            if self.current == before {
                self.generic_params = saved_generic;
                eprintln!(
                    "Parse error: parser made no progress in enum block (line {})",
                    self.current_line()
                );
                return None;
            }
        }
        self.generic_params = saved_generic;
        self.consume(TokenType::End, "Expected 'end' after enum")?;
        Some(MirWitness {
            kind: WitnessKind::EnumDef {
                name,
                type_params,
                variants,
            },
            span,
        })
    }

    /// Parse compile-time field tag metadata `@tag(key="val", flag, ...)` inside a `struct` field.
    fn parse_struct_field_tags(&mut self, tags: &mut Vec<(String, String)>) -> Option<()> {
        self.advance(); // '@'
        match self.peek().map(|t| &t.token_type) {
            Some(TokenType::Identifier(s)) if s == "tag" => {
                self.advance();
            }
            _ => {
                let msg = format!(
                    "Parse error: expected '@tag(...)' metadata on struct field at line {}",
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                return None;
            }
        }
        self.consume(TokenType::LParen, "Expected '(' after '@tag'")?;
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            let key = self.consume_identifier("Expected tag key name inside '@tag(...)'")?;
            if key.trim().is_empty() {
                let msg = format!(
                    "Parse error: empty tag key inside '@tag(...)' at line {}",
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                return None;
            }
            let value = if self.match_token_exact(TokenType::Assign) {
                match self.peek().cloned() {
                    Some(Token {
                        token_type: TokenType::String(s),
                        ..
                    }) => {
                        self.advance();
                        s
                    }
                    Some(Token {
                        token_type: TokenType::Int(n),
                        ..
                    }) => {
                        self.advance();
                        n.to_string()
                    }
                    Some(Token {
                        token_type: TokenType::Float(f),
                        ..
                    }) => {
                        self.advance();
                        f.to_string()
                    }
                    Some(Token {
                        token_type: TokenType::True,
                        ..
                    }) => {
                        self.advance();
                        "true".to_string()
                    }
                    Some(Token {
                        token_type: TokenType::False,
                        ..
                    }) => {
                        self.advance();
                        "false".to_string()
                    }
                    Some(Token {
                        token_type: TokenType::Identifier(id),
                        ..
                    }) => {
                        self.advance();
                        id
                    }
                    _ => {
                        let msg = format!(
                            "Parse error: expected literal value for tag '{}' at line {}",
                            key,
                            self.current_line()
                        );
                        eprintln!("{msg}");
                        self.diag = Some(msg);
                        return None;
                    }
                }
            } else {
                "true".to_string()
            };
            if tags.iter().any(|(k, _)| k == &key) {
                let msg = format!(
                    "Parse error: duplicate @tag key '{}' on struct field at line {}",
                    key,
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                return None;
            }
            tags.push((key, value));
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RParen, "Expected ')' after '@tag(...)'")?;
        Some(())
    }

    pub(super) fn emit_struct_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'struct'
        let name = self.consume_identifier("Expected struct name")?;
        self.declared_type_names.insert(name.clone());
        let saved_generic = std::mem::take(&mut self.generic_params);
        let type_params = match self.parse_generic_clause() {
            Some(tp) => tp,
            None => {
                self.generic_params = saved_generic;
                return None;
            }
        };
        let mut fields: Vec<crate::mir::witness::WitnessStructField> = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            let before = self.current;
            let fspan = self.span_of_current();
            let fname = match self.consume_identifier("Expected field name") {
                Some(n) => n,
                None => {
                    self.generic_params = saved_generic;
                    eprintln!(
                        "Parse error: unparsable field name in struct block (line {})",
                        self.current_line()
                    );
                    return None;
                }
            };
            if fields.iter().any(|f| f.name == fname) {
                let msg = format!(
                    "Parse error: duplicate field '{}' in struct '{}' (line {})",
                    fname,
                    name,
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                self.generic_params = saved_generic;
                return None;
            }
            if self
                .consume(TokenType::Colon, "Expected ':' after field name")
                .is_none()
            {
                self.generic_params = saved_generic;
                return None;
            }
            let ftype = match self.parse_type_annotation() {
                Some(t) => t,
                None => {
                    self.generic_params = saved_generic;
                    eprintln!(
                        "Parse error: unparsable type annotation for field \
                         `{fname}` (line {})",
                        self.current_line()
                    );
                    return None;
                }
            };
            let mut default_expr: Option<MirWitness> = None;
            let mut tags: Vec<(String, String)> = Vec::new();
            // Accept optional `= default_expr` and `@tag(...)` in either order.
            loop {
                if self.check(&TokenType::At) {
                    if self.parse_struct_field_tags(&mut tags).is_none() {
                        self.generic_params = saved_generic;
                        return None;
                    }
                } else if self.match_token_exact(TokenType::Assign) {
                    if default_expr.is_some() {
                        let msg = format!(
                            "Parse error: duplicate default value for field '{}' at line {}",
                            fname,
                            self.current_line()
                        );
                        eprintln!("{msg}");
                        self.diag = Some(msg);
                        self.generic_params = saved_generic;
                        return None;
                    }
                    let prev_flag = self.in_struct_field_default;
                    self.in_struct_field_default = true;
                    let expr_res = self.emit_expr_w();
                    self.in_struct_field_default = prev_flag;
                    match expr_res {
                        Some(e) => default_expr = Some(e),
                        None => {
                            self.generic_params = saved_generic;
                            return None;
                        }
                    }
                } else {
                    break;
                }
            }
            let _ = self.match_token(&[TokenType::Comma]);
            fields.push(crate::mir::witness::WitnessStructField {
                name: fname,
                type_hint: crate::mir::hint::TypeHint::from_type(ftype),
                default: default_expr,
                tags,
                span: fspan,
            });
            if self.current == before {
                self.generic_params = saved_generic;
                eprintln!(
                    "Parse error: parser made no progress in struct block (line {})",
                    self.current_line()
                );
                return None;
            }
        }
        self.generic_params = saved_generic;
        self.consume(TokenType::End, "Expected 'end' after struct")?;
        Some(MirWitness {
            kind: WitnessKind::StructDef {
                name,
                type_params,
                fields,
            },
            span,
        })
    }

    /// Stage 3C: Parse `trait Name<T>: Parent1, Parent2 ... end`.
    pub(super) fn emit_trait_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'trait'
        let name = self.consume_identifier("Expected trait name after 'trait'")?;
        self.declared_type_names.insert(name.clone());
        let saved_generic = std::mem::take(&mut self.generic_params);
        let type_params = match self.parse_generic_clause() {
            Some(tp) => tp,
            None => {
                self.generic_params = saved_generic;
                return None;
            }
        };
        let mut parents = Vec::new();
        if self.match_token_exact(TokenType::Colon) {
            loop {
                match self.consume_identifier("Expected parent trait name after ':'") {
                    Some(p) => parents.push(p),
                    None => {
                        self.generic_params = saved_generic;
                        return None;
                    }
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        let mut methods: Vec<crate::mir::witness::WitnessTraitMethod> = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            let before = self.current;
            let mspan = self.span_of_current();
            if !self.match_token(&[TokenType::Fn, TokenType::Task]) {
                let msg = format!(
                    "Parse error: expected 'fn' or 'task' method declaration inside trait '{}' at line {}",
                    name,
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                self.generic_params = saved_generic;
                return None;
            }
            let mname = match self.consume_identifier("Expected method name in trait") {
                Some(n) => n,
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            };
            let outer_generics = self.generic_params.clone();
            let method_type_params = match self.parse_generic_clause() {
                Some(mtp) => {
                    let mut combined = outer_generics.clone();
                    for tp in &mtp {
                        if !combined.contains(tp) {
                            combined.push(tp.clone());
                        }
                    }
                    self.generic_params = combined;
                    mtp
                }
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            };
            if self
                .consume(TokenType::LParen, "Expected '(' after trait method name")
                .is_none()
            {
                self.generic_params = saved_generic;
                return None;
            }
            let (params, has_self) = match self.parse_witness_params(true, false) {
                Some(v) => v,
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            };
            if self
                .consume(TokenType::RParen, "Expected ')' after trait method params")
                .is_none()
            {
                self.generic_params = saved_generic;
                return None;
            }
            let return_type = if self.match_token(&[TokenType::Arrow, TokenType::Colon]) {
                match self.parse_type_annotation() {
                    Some(t) => Some(crate::mir::hint::TypeHint::from_type(t)),
                    None => {
                        self.generic_params = saved_generic;
                        return None;
                    }
                }
            } else {
                None
            };
            while self.match_token(&[TokenType::Newline]) {}
            let default_body = if self.check(&TokenType::Fn)
                || self.check(&TokenType::Task)
                || self.check(&TokenType::End)
            {
                None
            } else {
                let body_res = self.in_nested_scope(|this| {
                    let mut stmts = Vec::new();
                    while !this.check(&TokenType::End) && !this.is_at_end() {
                        let w = this.emit_statement_expr_w()?;
                        stmts.push(w);
                        while this.match_token(&[TokenType::Newline]) {}
                    }
                    this.consume(
                        TokenType::End,
                        "Expected 'end' after trait default method body",
                    )?;
                    Some(Self::block_witness(stmts, mspan))
                });
                match body_res {
                    Some(b) => Some(b),
                    None => {
                        self.generic_params = saved_generic;
                        return None;
                    }
                }
            };
            self.generic_params = outer_generics;
            methods.push(crate::mir::witness::WitnessTraitMethod {
                name: mname,
                type_params: method_type_params,
                params,
                return_type,
                default_body,
                has_self,
                span: mspan,
            });
            if self.current == before {
                self.generic_params = saved_generic;
                return None;
            }
        }
        self.generic_params = saved_generic;
        self.consume(TokenType::End, "Expected 'end' after trait definition")?;
        Some(MirWitness {
            kind: WitnessKind::TraitDef {
                name,
                type_params,
                parents,
                methods,
            },
            span,
        })
    }

    /// Parse a type header `Name<Arg1, Arg2>` inside `impl ...`, collecting implicit generic
    /// type parameters (such as `T` in `impl Container<T> for list<T>`) into `self.generic_params`.
    fn parse_impl_header_type(
        &mut self,
        collected_type_params: &mut Vec<String>,
    ) -> Option<(String, Vec<crate::mir::hint::TypeHint>)> {
        let name = self.consume_identifier("Expected type or trait name in 'impl'")?;
        let mut generics = Vec::new();
        if self.match_token(&[TokenType::Less]) {
            loop {
                // Allow introducing implicit single-identifier generic vars like `T` in `impl Container<T> for list<T>`.
                if let Some(Token {
                    token_type: TokenType::Identifier(id),
                    ..
                }) = self.peek().cloned()
                {
                    let lower = id.to_lowercase();
                    let is_builtin = matches!(
                        lower.as_str(),
                        "int"
                            | "float"
                            | "number"
                            | "string"
                            | "char"
                            | "bool"
                            | "nil"
                            | "any"
                            | "unknown"
                            | "context"
                            | "channel"
                            | "task_handle"
                            | "taskhandle"
                            | "tensor"
                            | "list"
                            | "dict"
                    ) || crate::common::DType::from_name(&lower).is_some()
                        || self.declared_type_names.contains(id.as_str());
                    let is_followed_by_less = matches!(
                        self.tokens.get(self.current + 1).map(|t| &t.token_type),
                        Some(TokenType::Less)
                    );
                    if !is_builtin && !is_followed_by_less {
                        if !self.generic_params.contains(&id) {
                            self.generic_params.push(id.clone());
                        }
                        if !collected_type_params.contains(&id) {
                            collected_type_params.push(id.clone());
                        }
                    }
                }
                let ty = self.parse_type_annotation()?;
                generics.push(crate::mir::hint::TypeHint::from_type(ty));
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(
                TokenType::Greater,
                "Expected '>' in impl header type arguments",
            )?;
        }
        Some((name, generics))
    }

    /// Stage 3C: Parse `impl<T> Trait<T> for Target<T> ... end` or inherent `impl Target<T> ... end`.
    pub(super) fn emit_impl_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'impl'
        let saved_generic = std::mem::take(&mut self.generic_params);
        let mut type_params = match self.parse_generic_clause() {
            Some(tp) => tp,
            None => {
                self.generic_params = saved_generic;
                return None;
            }
        };
        let (first_name, first_generics) = match self.parse_impl_header_type(&mut type_params) {
            Some(v) => v,
            None => {
                self.generic_params = saved_generic;
                return None;
            }
        };
        let (trait_name, trait_generics, target_type, target_generics) =
            if self.match_token_exact(TokenType::For) {
                let (tgt_name, tgt_gens) = match self.parse_impl_header_type(&mut type_params) {
                    Some(v) => v,
                    None => {
                        self.generic_params = saved_generic;
                        return None;
                    }
                };
                (Some(first_name), first_generics, tgt_name, tgt_gens)
            } else {
                (None, Vec::new(), first_name, first_generics)
            };

        let mut methods: Vec<crate::mir::witness::WitnessImplMethod> = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            let before = self.current;
            let mspan = self.span_of_current();
            if !self.match_token(&[TokenType::Fn, TokenType::Task]) {
                let msg = format!(
                    "Parse error: expected 'fn' or 'task' method implementation inside impl block at line {}",
                    self.current_line()
                );
                eprintln!("{msg}");
                self.diag = Some(msg);
                self.generic_params = saved_generic;
                return None;
            }
            let mname = match self.consume_identifier("Expected method name in impl block") {
                Some(n) => n,
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            };
            let outer_generics = self.generic_params.clone();
            let method_type_params = match self.parse_generic_clause() {
                Some(mtp) => {
                    let mut combined = outer_generics.clone();
                    for tp in &mtp {
                        if !combined.contains(tp) {
                            combined.push(tp.clone());
                        }
                    }
                    self.generic_params = combined;
                    mtp
                }
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            };
            if self
                .consume(TokenType::LParen, "Expected '(' after impl method name")
                .is_none()
            {
                self.generic_params = saved_generic;
                return None;
            }
            let (params, has_self) = match self.parse_witness_params(true, false) {
                Some(v) => v,
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            };
            if self
                .consume(TokenType::RParen, "Expected ')' after impl method params")
                .is_none()
            {
                self.generic_params = saved_generic;
                return None;
            }
            let return_type = if self.match_token(&[TokenType::Arrow, TokenType::Colon]) {
                match self.parse_type_annotation() {
                    Some(t) => Some(crate::mir::hint::TypeHint::from_type(t)),
                    None => {
                        self.generic_params = saved_generic;
                        return None;
                    }
                }
            } else {
                None
            };
            let body_res = self.in_nested_scope(|this| {
                let body_w = if this.match_token_exact(TokenType::Newline) {
                    let mut stmts = Vec::new();
                    while this.match_token(&[TokenType::Newline]) {}
                    while !this.check(&TokenType::End) && !this.is_at_end() {
                        let w = this.emit_statement_expr_w()?;
                        stmts.push(w);
                        while this.match_token(&[TokenType::Newline]) {}
                    }
                    this.consume(TokenType::End, "Expected 'end' after impl method body")?;
                    Self::block_witness(stmts, mspan)
                } else if this.check(&TokenType::End) {
                    this.advance();
                    MirWitness {
                        kind: WitnessKind::Literal(Literal::Nil(mspan)),
                        span: mspan,
                    }
                } else {
                    let expr = this.emit_expr_w()?;
                    this.match_token_exact(TokenType::End);
                    expr
                };
                Some(body_w)
            });
            self.generic_params = outer_generics;
            let body = match body_res {
                Some(b) => b,
                None => {
                    self.generic_params = saved_generic;
                    return None;
                }
            };
            methods.push(crate::mir::witness::WitnessImplMethod {
                name: mname,
                type_params: method_type_params,
                params,
                return_type,
                body,
                has_self,
                span: mspan,
            });
            if self.current == before {
                self.generic_params = saved_generic;
                return None;
            }
        }
        self.generic_params = saved_generic;
        self.consume(TokenType::End, "Expected 'end' after impl block")?;
        Some(MirWitness {
            kind: WitnessKind::ImplDef {
                type_params,
                trait_name,
                trait_generics,
                target_type,
                target_generics,
                methods,
            },
            span,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::parser_v3::ParserV3;

    fn compiles(src: &str) -> bool {
        ParserV3::compile(src).is_ok()
    }

    /// D353（声明位这一侧）：`let` 的绑定名必须是**真正的标识符**。
    ///
    /// `tokens.rs` 侧已钉住「两条 API 对关键字给出相反答案」；本测试钉住
    /// **调用点确实用了严格的那条**。此前 `emit_let_w` 借用了宽容的
    /// `consume_identifier`，产出**僵尸绑定**：`let if = 5` exit 0（声明成功）
    /// 而 `print(if)` 永远解析失败 —— 用户拿到「声明成功、引用失败、
    /// 诊断还指错行」的三重坑，比直接报错更糟。
    #[test]
    fn d353_let_binding_rejects_keyword_names() {
        for name in ["if", "let", "fn", "end", "match"] {
            assert!(
                !compiles(&format!("let {name} = 5\n")),
                "`let {name}` 是僵尸绑定：声明成功但永远引用不到，必须编译期报错"
            );
        }
        // 对照：真标识符照常工作
        assert!(compiles("let zz = 5\nprint(zz)\n"));
    }

    /// 各类定义声明都必须能编译（这些 emitter 集中在本文件）。
    #[test]
    fn definition_declarations_compile() {
        let cases: &[(&str, &str)] = &[
            ("let", "let x = 1 + 2\nprint(x)\n"),
            ("let + 类型标注", "let x: int = 7\nprint(x)\n"),
            ("let + union 标注", "let x: string | int = 7\nprint(x)\n"),
            // 注意：具名 `fn` 声明在本语言**不存在**（`fn` 只是闭包关键字，
            // 见 CHANGELOG 23067 行一节）。函数声明走 `task`。
            (
                "task",
                "task add(a, b)\n  return a + b\nend\nprint(add(1, 2))\n",
            ),
            ("struct", "struct P\n  x: number\nend\nprint(1)\n"),
            ("enum", "enum E\n  A\n  B\nend\nprint(1)\n"),
        ];
        let mut failures = Vec::new();
        for (name, src) in cases {
            if !compiles(src) {
                failures.push(*name);
            }
        }
        assert!(failures.is_empty(), "以下定义形态应可编译：{failures:?}");
    }

    /// D46：enum / struct 的成员循环必须**保证进度**。
    ///
    /// 此前变体名解析失败时**既不 push 也不 advance**，循环条件恒真 ⇒
    /// **死循环**（不是报错，是挂死）。
    ///
    /// 修复端有**两道彼此独立**的守卫：`None => return None`（显式报错）
    /// 与 `if self.current == before`（进度校验）。实测**任一道单独存在
    /// 就足以终止**；把两道都拿掉，本测试会在 45s 后被 timeout 杀掉
    /// （exit 143），把两道都保留则秒过。故这条测试能跑完本身就是护栏。
    #[test]
    fn d46_malformed_enum_and_struct_terminate_instead_of_looping() {
        for src in [
            "enum E\n  @@@\nend\n",
            "enum E\n  A\n  @@@\n  B\nend\n",
            "struct P\n  @@@\nend\n",
            "struct P\n  x\nend\n", // 缺 `:`
            "enum E\n",             // 缺 end
        ] {
            // 只断言「能返回」——返回后编译成功或失败都算正确行为。
            // 修复前这里会永不返回，测试二进制被杀。
            let _ = compiles(src);
        }
    }

    /// struct 字段的类型标注解析失败必须**报错**，不得静默丢字段。
    ///
    /// 此前是 `if let Some(..)` 落空即什么都不做，用户以为 `x: T` 声明
    /// 成功了，而该字段根本没进定义。
    #[test]
    fn struct_field_with_bad_type_annotation_is_rejected() {
        assert!(
            !compiles("struct P\n  x: @@@\nend\n"),
            "字段类型标注解析失败必须报错，不得静默丢字段"
        );
        assert!(compiles("struct P\n  x: number\nend\nprint(1)\n"));
    }

    /// 形参与返回类型标注（v0.104.6 起）必须仍被接受。
    ///
    /// 与 README 的用户承诺对应：`task f(a: number): T` 此前无法展示，
    /// 如今必须照常工作。
    ///
    /// 顺带钉住一条易被误加的形态：**具名 `fn` 声明在本语言不存在**
    /// （`fn` 只是闭包关键字，见 CHANGELOG 23067 行一节）。若有人日后
    /// 「顺手」给 `TokenType::Fn` 也派发到 `emit_fn_def_w`，那是新增语法，
    /// 不是本测试该放行的东西 —— 故显式断言它仍被拒。
    #[test]
    fn typed_parameters_and_return_annotations_still_compile() {
        assert!(compiles(
            "task double(a: number): number\n  return a * 2\nend\nprint(double(21))\n"
        ));
        assert!(compiles(
            "task go(a: number): number\n  a + 1\nend\nprint(go(1))\n"
        ));
        assert!(
            !compiles("fn f(a: number): number\n  return a\nend\n"),
            "具名 `fn` 声明不存在 —— `fn` 仅作闭包关键字"
        );
    }
}
