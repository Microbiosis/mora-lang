//! v0.102: 声明式范式（逻辑式/关系式）语法 emit — `rel` 定义与 `solve` 查询。
//!
//! 与 emit.rs / emit_definitions.rs 同属 `impl ParserV3`（Rust 允许 impl 块
//! 跨文件拆分）。生产主路径是 `ParserV3::compile`，本模块直接 emit MirInst
//! 并并行产出 MirWitness。
//!
//! ## 语法与变量规则（Prolog 同源）
//!
//! ```mora
//! rel edge("a", "b")                       -- 事实：头部是项，无体
//! rel path(x, y) edge(x, y) end            -- 规则：体是目标合取（`,` 连接）
//! rel path(x, z) edge(x, y), path(y, z) end
//! solve { path("a", ?to) }                 -- 查询：?to 是投影变量
//! solve 3 { path(?from, ?to) }             -- run 3（前 3 个解）
//! ```
//!
//! **子句内变量**（`rel` 头/体）：裸标识符 = 子句局部逻辑变量，作用域限于
//! 该子句、每次调用 rename 为 fresh 实例。头部未出现的体变量（如上面规则
//! 中的 `y`）同样是子句局部逻辑变量。符号常量写作字符串字面量（`"a"`）。
//! `_` = 匿名变量（每次出现都是独立的 fresh 变量）。
//!
//! **查询变量**（`solve`）：`?name` 是投影变量（解中返回其绑定）；`_` 是
//! 存在性匿名变量（不投影）；字面量是常量。

use super::*;
use crate::rel::{Clause, Goal, Term};

/// 子句局部变量分配器：标识符 → 参数槽。头部初始化，体解析时可增长
/// （体独有变量获得新槽位，rename 时各自成为 fresh 逻辑变量）。
#[derive(Default)]
struct ClauseVars {
    slots: std::collections::HashMap<String, usize>,
    /// 槽位序号 → 名（含体独有变量与匿名变量）。作为 `Clause::params`。
    slot_names: Vec<String>,
}

impl ClauseVars {
    /// 取（或新建）标识符的槽位。
    fn slot_for(&mut self, name: &str) -> usize {
        if let Some(i) = self.slots.get(name) {
            return *i;
        }
        let i = self.slot_names.len();
        self.slots.insert(name.to_string(), i);
        self.slot_names.push(name.to_string());
        i
    }

    /// 新建匿名变量槽位（每次调用一个独立槽位）。
    fn fresh_anon(&mut self) -> (usize, String) {
        let i = self.slot_names.len();
        let name = format!("__anon{}", i);
        self.slots.insert(name.clone(), i);
        self.slot_names.push(name.clone());
        (i, name)
    }
}

impl ParserV3 {
    /// `rel` 关系定义。头部是项列表；体是可选的目标合取。
    pub(super) fn emit_rel_def_w(&mut self) -> Option<MirWitness> {
        let span = self.span_of_current();
        self.advance(); // 'rel'
        let name = self.consume_identifier("Expected relation name")?;
        self.consume(TokenType::LParen, "Expected '(' after relation name")?;
        while self.match_token(&[TokenType::Newline]) {}

        let mut vars = ClauseVars::default();
        let mut head: Vec<Term> = Vec::new();
        let mut head_wits: Vec<MirWitness> = Vec::new();
        while !self.check(&TokenType::RParen) && !self.is_at_end() {
            let (t, w) = self.emit_clause_term_w(&mut vars)?;
            head.push(t);
            head_wits.push(w);
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
            while self.match_token(&[TokenType::Newline]) {}
        }
        self.consume(TokenType::RParen, "Expected ')' after relation parameters")?;

        // 事实 vs 规则：头部 `)` 之后**同一逻辑行**还有 token 才是体。
        let mut body_goals: Vec<Goal> = Vec::new();
        let mut body_wits: Vec<MirWitness> = Vec::new();
        let has_body = !matches!(
            self.peek().map(|t| &t.token_type),
            None | Some(TokenType::Newline) | Some(TokenType::End) | Some(TokenType::EOF)
        );
        if has_body {
            loop {
                let (g, w) = self.emit_clause_goal_w(&mut vars)?;
                body_goals.push(g);
                body_wits.push(w);
                while self.match_token(&[TokenType::Newline]) {}
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
                while self.match_token(&[TokenType::Newline]) {}
            }
            self.consume(TokenType::End, "Expected 'end' after relation body")?;
        } else if self.check(&TokenType::End) {
            self.advance(); // 显式空体 `rel foo() end`
        }

        let (body_goal, body_wit) = if body_goals.is_empty() {
            (
                Goal::Succeed,
                MirWitness {
                    kind: WitnessKind::Call {
                        callee: crate::mir::witness::WitnessCallee::Name("succeed".to_string()),
                        args: Vec::new(),
                    },
                    span,
                },
            )
        } else if body_goals.len() == 1 {
            (
                body_goals.pop().expect("len 1"),
                body_wits.pop().expect("len 1"),
            )
        } else {
            let wit = MirWitness {
                kind: WitnessKind::Sequence(body_wits),
                span,
            };
            (Goal::Conj(body_goals), wit)
        };

        let clause = Clause::rule(vars.slot_names.clone(), head, body_goal);
        let clause_wit = crate::mir::witness::RelClauseWit {
            head: head_wits,
            body: Box::new(body_wit),
        };

        self.emit.emit(MirInst::RelDef {
            name: name.clone(),
            clauses: vec![clause.clone()],
        });
        let dst = self.emit.alloc_reg();
        self.emit
            .emit(MirInst::Const(dst, crate::value::Value::Nil));

        Some(MirWitness {
            kind: WitnessKind::RelDef {
                name,
                clauses: vec![clause],
                clause_wits: vec![clause_wit],
            },
            span,
        })
    }

    /// `solve` 查询：`solve { goal }` 或 `solve N { goal }`（run N）。
    /// 返回 (结果寄存器, witness) —— 可作语句亦可作表达式。
    pub(super) fn emit_solve_w(&mut self) -> Option<(Reg, MirWitness)> {
        let span = self.span_of_current();
        self.advance(); // 'solve'
        // 上界 N。注意：无后缀数字字面量在词法层是 Float（Mora 词法约定），
        // 故同时接受 Int 与整数值 Float。
        let limit = match self.peek().map(|t| t.token_type.clone()) {
            Some(TokenType::Int(n)) => {
                self.advance();
                Some(n.max(0) as usize)
            }
            Some(TokenType::Float(f)) if f >= 0.0 && f.fract() == 0.0 => {
                self.advance();
                Some(f as usize)
            }
            _ => None,
        };

        let mut query_vars: Vec<String> = Vec::new();
        let mut anon_vars: Vec<String> = Vec::new();
        self.consume(TokenType::LBrace, "Expected '{' after solve")?;
        while self.match_token(&[TokenType::Newline]) {}
        let mut body_goals: Vec<Goal> = Vec::new();
        let mut body_wits: Vec<MirWitness> = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            let (g, w) = self.emit_query_goal_w(&mut query_vars, &mut anon_vars)?;
            body_goals.push(g);
            body_wits.push(w);
            while self.match_token(&[TokenType::Newline]) {}
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
            while self.match_token(&[TokenType::Newline]) {}
        }
        self.consume(TokenType::RBrace, "Expected '}' after solve goal")?;

        let goal_wit = if body_wits.len() == 1 {
            body_wits.pop().expect("len 1")
        } else {
            MirWitness {
                kind: WitnessKind::Sequence(body_wits),
                span,
            }
        };
        let goal_mir = crate::mir::lower::lower_block_witness_to_mir(&goal_wit);

        let dst = self.emit.alloc_reg();
        self.emit.emit(MirInst::Solve {
            dst,
            limit,
            query_vars: query_vars.clone(),
            anon_vars: anon_vars.clone(),
            goal: Box::new(goal_mir),
        });

        Some((
            dst,
            MirWitness {
                kind: WitnessKind::Solve {
                    limit,
                    query_vars,
                    anon_vars,
                    goal: Box::new(goal_wit),
                },
                span,
            },
        ))
    }

    // ── 子句（rel 头/体）目标与项 ──

    /// 子句体单个目标。
    fn emit_clause_goal_w(&mut self, vars: &mut ClauseVars) -> Option<(Goal, MirWitness)> {
        let span = self.span_of_current();
        // 目标可以带 `?` 前缀（与查询变量书写风格统一，语义同局部变量）
        let _ = self.match_token_exact(TokenType::Question);
        let name = self.consume_identifier("Expected goal name")?;

        match name.as_str() {
            "fail" => Some((Goal::Fail, Self::goal_const_wit("fail", span))),
            "succeed" => Some((Goal::Succeed, Self::goal_const_wit("succeed", span))),
            "unify" => {
                self.consume(TokenType::LParen, "Expected '(' after unify")?;
                let (a, aw) = self.emit_clause_term_w(vars)?;
                self.consume(TokenType::Comma, "Expected ',' in unify")?;
                let (b, bw) = self.emit_clause_term_w(vars)?;
                self.consume(TokenType::RParen, "Expected ')' after unify")?;
                Some((
                    Goal::Unify(a, b),
                    Self::goal_call_wit("unify", vec![aw, bw], span),
                ))
            }
            "project" => {
                self.consume(TokenType::LParen, "Expected '(' after project")?;
                let f = self.consume_identifier("Expected function in project")?;
                let mut args: Vec<Term> = Vec::new();
                let mut wits: Vec<MirWitness> = Vec::new();
                while self.match_token(&[TokenType::Comma]) {
                    let (t, w) = self.emit_clause_term_w(vars)?;
                    args.push(t);
                    wits.push(w);
                }
                self.consume(TokenType::RParen, "Expected ')' after project")?;
                let result = args.pop()?;
                wits.pop();
                let mut call_args = vec![MirWitness {
                    kind: WitnessKind::Variable(f.clone()),
                    span,
                }];
                call_args.extend(wits);
                Some((
                    Goal::Project {
                        func: crate::rel::ProjectFn::Name(f),
                        args,
                        result,
                    },
                    Self::goal_call_wit("project", call_args, span),
                ))
            }
            _ => {
                // 关系调用
                self.consume(TokenType::LParen, "Expected '(' after relation name")?;
                let mut args: Vec<Term> = Vec::new();
                let mut wits: Vec<MirWitness> = Vec::new();
                while !self.check(&TokenType::RParen) && !self.is_at_end() {
                    let (t, w) = self.emit_clause_term_w(vars)?;
                    args.push(t);
                    wits.push(w);
                    if !self.match_token(&[TokenType::Comma]) {
                        break;
                    }
                }
                self.consume(TokenType::RParen, "Expected ')' after relation arguments")?;
                Some((
                    Goal::Invoke {
                        name: name.clone(),
                        clauses: None,
                        args,
                    },
                    Self::goal_call_wit(&name, wits, span),
                ))
            }
        }
    }

    /// 子句项：裸标识符 = 子句局部变量；`_` = 匿名变量；`nil`/`cons`/字面量
    /// 为常量构造。
    fn emit_clause_term_w(&mut self, vars: &mut ClauseVars) -> Option<(Term, MirWitness)> {
        let span = self.span_of_current();
        // `?name` 与裸 `name` 同义（子句局部变量）
        let _ = self.match_token_exact(TokenType::Question);

        match self.peek()?.token_type.clone() {
            TokenType::Identifier(id) if id == "_" => {
                self.advance();
                // 匿名变量：每次出现独立槽位
                let (idx, wit_name) = vars.fresh_anon();
                Some((
                    Term::Param(idx),
                    MirWitness {
                        kind: WitnessKind::Variable(wit_name),
                        span,
                    },
                ))
            }
            TokenType::Identifier(id) if id == "nil" => {
                self.advance();
                Some((
                    Term::Val(crate::value::Value::Nil),
                    MirWitness {
                        kind: WitnessKind::Literal(Literal::Nil(span)),
                        span,
                    },
                ))
            }
            TokenType::Identifier(id) if id == "cons" && self.next_is_lparen() => {
                self.advance();
                self.consume(TokenType::LParen, "Expected '(' after cons")?;
                let (h, hw) = self.emit_clause_term_w(vars)?;
                self.consume(TokenType::Comma, "Expected ',' in cons")?;
                let (t, tw) = self.emit_clause_term_w(vars)?;
                self.consume(TokenType::RParen, "Expected ')' after cons")?;
                Some((
                    Term::Cons(Box::new(h), Box::new(t)),
                    Self::goal_call_wit("cons", vec![hw, tw], span),
                ))
            }
            TokenType::Identifier(id) if self.next_is_lparen() => {
                // 头部/项位置的调用除 cons 外无意义
                self.advance();
                self.consume(TokenType::LParen, "Expected '(' after term constructor")?;
                Some((
                    Term::Val(crate::value::Value::String(id)),
                    MirWitness {
                        kind: WitnessKind::Literal(Literal::String(String::new(), span)),
                        span,
                    },
                ))
            }
            TokenType::Identifier(id) => {
                self.advance();
                let idx = vars.slot_for(&id);
                Some((
                    Term::Param(idx),
                    MirWitness {
                        kind: WitnessKind::Variable(id),
                        span,
                    },
                ))
            }
            other => self.emit_literal_term_w(other, span),
        }
    }

    // ── 查询（solve）目标与项 ──

    /// 查询体单个目标。
    fn emit_query_goal_w(
        &mut self,
        query_vars: &mut Vec<String>,
        anon_vars: &mut Vec<String>,
    ) -> Option<(Goal, MirWitness)> {
        let span = self.span_of_current();
        let name = self.consume_identifier("Expected goal name")?;
        match name.as_str() {
            "fail" => Some((Goal::Fail, Self::goal_const_wit("fail", span))),
            "succeed" => Some((Goal::Succeed, Self::goal_const_wit("succeed", span))),
            "unify" => {
                self.consume(TokenType::LParen, "Expected '(' after unify")?;
                let (a, aw) = self.emit_query_term_w(query_vars, anon_vars)?;
                self.consume(TokenType::Comma, "Expected ',' in unify")?;
                let (b, bw) = self.emit_query_term_w(query_vars, anon_vars)?;
                self.consume(TokenType::RParen, "Expected ')' after unify")?;
                Some((
                    Goal::Unify(a, b),
                    Self::goal_call_wit("unify", vec![aw, bw], span),
                ))
            }
            "project" => {
                self.consume(TokenType::LParen, "Expected '(' after project")?;
                let f = self.consume_identifier("Expected function in project")?;
                let mut args: Vec<Term> = Vec::new();
                let mut wits: Vec<MirWitness> = Vec::new();
                while self.match_token(&[TokenType::Comma]) {
                    let (t, w) = self.emit_query_term_w(query_vars, anon_vars)?;
                    args.push(t);
                    wits.push(w);
                }
                self.consume(TokenType::RParen, "Expected ')' after project")?;
                let result = args.pop()?;
                wits.pop();
                let mut call_args = vec![MirWitness {
                    kind: WitnessKind::Variable(f.clone()),
                    span,
                }];
                call_args.extend(wits);
                Some((
                    Goal::Project {
                        func: crate::rel::ProjectFn::Name(f),
                        args,
                        result,
                    },
                    Self::goal_call_wit("project", call_args, span),
                ))
            }
            _ => {
                self.consume(TokenType::LParen, "Expected '(' after relation name")?;
                let mut args: Vec<Term> = Vec::new();
                let mut wits: Vec<MirWitness> = Vec::new();
                while !self.check(&TokenType::RParen) && !self.is_at_end() {
                    let (t, w) = self.emit_query_term_w(query_vars, anon_vars)?;
                    args.push(t);
                    wits.push(w);
                    if !self.match_token(&[TokenType::Comma]) {
                        break;
                    }
                }
                self.consume(TokenType::RParen, "Expected ')' after relation arguments")?;
                Some((
                    Goal::Invoke {
                        name: name.clone(),
                        clauses: None,
                        args,
                    },
                    Self::goal_call_wit(&name, wits, span),
                ))
            }
        }
    }

    /// 查询项：`?name` = 投影变量；`_` = 存在性匿名变量；字面量/nil/cons 为项。
    fn emit_query_term_w(
        &mut self,
        query_vars: &mut Vec<String>,
        anon_vars: &mut Vec<String>,
    ) -> Option<(Term, MirWitness)> {
        let span = self.span_of_current();

        // `?name` → 投影查询变量（运行期经构建环境解析为 Value::LogicVar(i)）
        if self.match_token_exact(TokenType::Question) {
            let n = self.consume_identifier("Expected variable name after '?'")?;
            if !query_vars.contains(&n) {
                query_vars.push(n.clone());
            }
            return Some((
                Term::Val(crate::value::Value::String(n.clone())),
                MirWitness {
                    kind: WitnessKind::Variable(n),
                    span,
                },
            ));
        }

        match self.peek()?.token_type.clone() {
            TokenType::Identifier(id) if id == "_" => {
                self.advance();
                let anon = format!("__anon{}", anon_vars.len());
                anon_vars.push(anon.clone());
                Some((
                    Term::Val(crate::value::Value::String(anon.clone())),
                    MirWitness {
                        kind: WitnessKind::Variable(anon),
                        span,
                    },
                ))
            }
            TokenType::Identifier(id) if id == "nil" => {
                self.advance();
                Some((
                    Term::Val(crate::value::Value::Nil),
                    MirWitness {
                        kind: WitnessKind::Literal(Literal::Nil(span)),
                        span,
                    },
                ))
            }
            TokenType::Identifier(id) if id == "cons" && self.next_is_lparen() => {
                self.advance();
                self.consume(TokenType::LParen, "Expected '(' after cons")?;
                let (h, hw) = self.emit_query_term_w(query_vars, anon_vars)?;
                self.consume(TokenType::Comma, "Expected ',' in cons")?;
                let (t, tw) = self.emit_query_term_w(query_vars, anon_vars)?;
                self.consume(TokenType::RParen, "Expected ')' after cons")?;
                Some((
                    Term::Cons(Box::new(h), Box::new(t)),
                    Self::goal_call_wit("cons", vec![hw, tw], span),
                ))
            }
            TokenType::Identifier(id) => {
                // 查询中的裸标识符 = 符号常量（Prolog atom 惯例）：
                // 变量必须写 `?name`，常量写 `"str"` 或裸 atom
                self.advance();
                Some((
                    Term::Val(crate::value::Value::String(id.clone())),
                    MirWitness {
                        kind: WitnessKind::Literal(Literal::String(id, span)),
                        span,
                    },
                ))
            }
            other => self.emit_literal_term_w(other, span),
        }
    }

    /// 字面量项解析（Int/Float/String/Char/BigInt/Bool/Nil）。
    fn emit_literal_term_w(&mut self, tok: TokenType, span: Span) -> Option<(Term, MirWitness)> {
        let (t, lit) = match tok {
            TokenType::Int(n) => (
                Term::Val(crate::value::Value::Int(n)),
                Literal::Int(n, span),
            ),
            TokenType::Float(f) => (
                Term::Val(crate::value::Value::Float(f)),
                Literal::Float(f, span),
            ),
            TokenType::String(s) => (
                Term::Val(crate::value::Value::String(s.clone())),
                Literal::String(s, span),
            ),
            TokenType::Char(c) => (
                Term::Val(crate::value::Value::Char(c)),
                Literal::Char(c, span),
            ),
            TokenType::BigInt(n) => (
                Term::Val(crate::value::Value::BigInt(n.clone())),
                Literal::BigInt(n, span),
            ),
            TokenType::True => (
                Term::Val(crate::value::Value::Bool(true)),
                Literal::Bool(true, span),
            ),
            TokenType::False => (
                Term::Val(crate::value::Value::Bool(false)),
                Literal::Bool(false, span),
            ),
            TokenType::Nil => (Term::Val(crate::value::Value::Nil), Literal::Nil(span)),
            _ => return None,
        };
        self.advance();
        Some((
            t,
            MirWitness {
                kind: WitnessKind::Literal(lit),
                span,
            },
        ))
    }

    /// 下一个 token 是否是 `(`（区分裸变量与项构造调用）。
    fn next_is_lparen(&self) -> bool {
        matches!(
            self.tokens.get(self.current + 1).map(|t| &t.token_type),
            Some(TokenType::LParen)
        )
    }

    fn goal_const_wit(name: &str, span: Span) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Call {
                callee: crate::mir::witness::WitnessCallee::Name(name.to_string()),
                args: Vec::new(),
            },
            span,
        }
    }

    fn goal_call_wit(name: &str, args: Vec<MirWitness>, span: Span) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Call {
                callee: crate::mir::witness::WitnessCallee::Name(name.to_string()),
                args,
            },
            span,
        }
    }
}
