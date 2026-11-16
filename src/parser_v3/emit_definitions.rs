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
        // v0.104.6 D474：原先是 (reg, witness) 二元组（reg 供
        // `MirInst::DynTrait` 发射）；parser 只产 witness 后降为单值。
        let final_v_w =
            if let Some(crate::typeck::Type::TraitObject { trait_name, .. }) = type_hint.clone() {
                MirWitness {
                    kind: WitnessKind::DynTrait {
                        expr: Box::new(v_w),
                        trait_name,
                        generics: Vec::new(),
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
    ///      （见 `infer_expr` 的 Loop/While 分支）而被跳过，于是「看似可用」，
    ///      实际靠运行期 DAG 按名字写环境掩盖；
    ///   ③ 语义上多读一个不存在的变量，且 witness 树里多一个假的
    ///      `Variable("assign")` 节点（`child_witnesses` 遍历者都会看到）。
    ///
    /// 现在按 spec 形状解析：`assign` → 标识符 → `=` → 表达式，
    /// 产出 `MirInst::Assign` + `WitnessKind::Assign`（与裸赋值同一 IR 契约，
    /// 目标必须是已绑定名字，由 `infer_assign` 校验）。
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

    pub(super) fn emit_fn_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'task'
        let name = self.consume_identifier("Expected task name")?;
        // v0.104.6：进入泛型作用域前先存旧值（供退出时还原）。
        let saved_generic = std::mem::take(&mut self.generic_params);
        // v0.104.6：泛型声明子句 `task f<T, U>(…)`。它把类型参数写入
        // `self.generic_params`，随后形参/返回标注据此把裸名认作 `Type::TypeVar`。
        let type_params = self.parse_generic_clause()?;
        self.consume(TokenType::LParen, "Expected '(' after task name")?;
        // v0.104.6：形参类型标注 —— `task f(a: number, b: string): Ret`。
        //
        // 此前形参循环只收裸标识符，遇 `:` 直接 break ⇒ `Expected ')' after
        // parameters`（README「Type checking」节的示例曾因此整块无法解析）。
        // 承载位与消费端**本就存在**：`WitnessParam.type_hint`（witness.rs:446）
        // 与 `bidirectional.rs:193-202`（Phase A' 把形参 type_hint 绑进 HM 环境
        // 并做 subtype 检查）—— 缺的只是 parser 把标注填进 `type_hint` 这一步。
        //
        // 裸形参 `task f(x)` 行为不变（`type_hint: None`，与此前等价）。
        let mut params: Vec<(String, Option<crate::mir::hint::TypeHint>)> = Vec::new();
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            if let Some(p) = self.consume_identifier("Expected parameter name") {
                let hint = if self.match_token_exact(TokenType::Colon) {
                    Some(crate::mir::hint::TypeHint::from_type(
                        self.parse_type_annotation()?,
                    ))
                } else {
                    None
                };
                params.push((p, hint));
            }
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(TokenType::RParen, "Expected ')' after parameters")?;
        // v0.104.6：返回类型标注 `): Ret` —— `FnDef.return_type` 位本就存在，
        // 但此前写死 None（`emit_fn_def_w` 不消费尾随标注）。无 `:` 时保持 None，
        // 行为不变。
        let return_type = if self.match_token_exact(TokenType::Colon) {
            Some(crate::mir::hint::TypeHint::from_type(
                self.parse_type_annotation()?,
            ))
        } else {
            None
        };
        // 子上下文：函数体是独立作用域（`is_program_top = false`, `parse_loop_depth = 0`）
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
                // Empty body: `task main() end`
                MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(span)),
                    span,
                }
            } else {
                this.emit_expr_w()?
            };
            Some(body_w)
        });
        // 退出泛型作用域 —— 必须在体解析**之后**、且**无论成败**都执行，
        // 否则 `generic_params` 泄漏到后续无关标注，把同名误当类型变量。
        self.generic_params = saved_generic;
        let body_w = body_res?;
        let w_params = params
            .iter()
            .map(|(n, hint)| WitnessParam {
                name: n.clone(),
                type_hint: hint.clone(),
                default: None,
            })
            .collect();
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
                // `is_program_top`（程序顶层是唯一不属于任何函数体的寄存器
                // 空间），与下方 `break`/`continue` 的 `loop_stack` 守卫同款。
                //
                // 修前的实测（真实 CLI `mora run`，**全部 exit 0、零提示**）：
                //
                // ```mora
                // print(111)   → 111.0
                // return 1
                // print(999)   → 从不执行
                // ```
                //
                // `if` / `for` / `with` 的体同样内联在程序顶层，故
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
                // 循环深度」（`EmitContext::parse_loop_depth`）。
                //
                // 「循环外 break 是错误」是**解析语义** —— 判的是源码位置，
                // 与 IR 的跳转标签无关。此前它借 `loop_stack`（IR 标签栈）当
                // 「是否在循环内」的标记；parser 不再发射 MirInst 后那个栈在
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
        let mut variants = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            // v0.104.6 D46：保证进度（见 struct 块同处说明）
            let before = self.current;
            match self.consume_identifier("Expected variant name") {
                Some(v) => variants.push(v),
                // v0.104.6 D46：**此前根本没有 else 分支** —— 变体名解析失败时
                // 既不 push 也不 advance，循环条件恒真 → **死循环**，且
                // `consume_identifier` 每次都重打 "Expected variant name" 刷屏。
                // 实测 `enum E / A / @@@ / B / end` 挂死 >8s。
                None => {
                    eprintln!(
                        "Parse error: unparsable enum variant (line {})",
                        self.current_line()
                    );
                    return None;
                }
            }
            if self.current == before {
                eprintln!(
                    "Parse error: parser made no progress in enum block (line {})",
                    self.current_line()
                );
                return None;
            }
        }
        self.consume(TokenType::End, "Expected 'end' after enum")?;
        let _evs: Vec<crate::common::EnumVariant> = variants
            .iter()
            .map(|v| crate::common::EnumVariant {
                name: v.clone(),
                data: None,
            })
            .collect();
        Some(MirWitness {
            kind: WitnessKind::EnumDef { name, variants },
            span,
        })
    }

    pub(super) fn emit_struct_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'struct'
        let name = self.consume_identifier("Expected struct name")?;
        self.declared_type_names.insert(name.clone());
        let mut fields = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            while self.match_token(&[TokenType::Newline]) {}
            if self.check(&TokenType::End) {
                break;
            }
            // v0.104.6 D46：保证进度（见 enum 块同处说明）
            let before = self.current;
            match self.consume_identifier("Expected field name") {
                Some(fname) => {
                    self.consume(TokenType::Colon, "Expected ':' after field name")?;
                    // 类型标注解析失败时此前是**静默丢字段**（`if let Some(..)`
                    // 落空即什么都不做），用户以为 `x: T` 声明成功了。
                    // 改为报错。
                    match self.parse_type_annotation() {
                        Some(ftype) => fields.push((fname, ftype)),
                        None => {
                            eprintln!(
                                "Parse error: unparsable type annotation for field \
                                 `{fname}` (line {})",
                                self.current_line()
                            );
                            return None;
                        }
                    }
                }
                // v0.104.6 D46：此前是 `self.advance()` —— 静默跳过整行，
                // 程序继续跑并 exit 0（实测 `struct S / @@@ / x: Int / end`
                // 正常执行、退出码 0）。且错误被重复打印 3~4 遍（循环每次重试）。
                None => {
                    eprintln!(
                        "Parse error: unparsable field name in struct block (line {})",
                        self.current_line()
                    );
                    return None;
                }
            }
            if self.current == before {
                eprintln!(
                    "Parse error: parser made no progress in struct block (line {})",
                    self.current_line()
                );
                return None;
            }
        }
        self.consume(TokenType::End, "Expected 'end' after struct")?;
        let _sfs: Vec<crate::common::StructField> = fields
            .iter()
            .map(|(fname, ftype)| crate::common::StructField {
                name: fname.clone(),
                type_hint: ftype.name(),
            })
            .collect();
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
