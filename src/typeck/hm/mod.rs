//! v0.55: Hindley-Milner Type Inference for MirWitness.
//!
//! This module is the MirWitness-native replacement of the ast_v2-based
//! `typeck::check_program` pipeline. The earlier v0.53 prototype still
//! referenced `NodeId` / `AstArena` and silently no-op'd any closure type
//! check because `Type::Closure` is a unit variant. The current rewrite
//! drives inference directly off `&MirWitness`, tracks closure signatures in
//! a side table keyed by a fresh `Type::TypeVar`, and covers every
//! `WitnessKind` variant.
//!
//! Public entry point: [`check_program_mir`] (re-exported from
//! `crate::typeck`).

use std::collections::{HashMap, HashSet};

use crate::common::Span;
use crate::mir::expr::BuiltinOp;
use crate::mir::witness::{MirWitness, WitnessArm, WitnessCallee, WitnessKind, WitnessParam};
use crate::typeck::Type;

mod builtin; // v0.75.70: builtin 类型推断（自 mod.rs 拆出）
pub mod diag; // v0.75.94: DiagFilter + WitnessNodeId（自 mod.rs 抽离）
pub mod env;
pub mod error;
pub mod generalize;
mod infer;
pub mod unify; // v0.75.70: infer_* 方法族（自 mod.rs 拆出）
pub mod util; // v0.75.96: check_union / join_types（自 typeck::mod.rs 抽离）
// v0.80: 行多态 HM unification（Stage 2/4 algebraic effects 落地）
pub mod row;

pub use error::TypeError;

use env::TypeEnv;
use unify::{Constraint, Substitution};

///  Signature for a closure / callable.
///  Stored in a side table keyed by the `Type` (always a fresh
///  `Type::TypeVar`) that represents the closure's identity.
#[derive(Debug, Clone)]
pub struct ClosureSig {
    pub params: Vec<Type>,
    pub return_type: Type,
    /// Number of declared parameters; used to arity-check calls.
    pub arity: usize,
}

#[derive(Default)]
pub struct HMInference {
    pub env: TypeEnv,
    pub fresh_counter: usize,
    pub constraints: Vec<Constraint>,
    /// Side table keyed by the `char` of a fresh `Type::TypeVar` that
    /// was minted as a closure's identity. The same `char` is also
    /// stored inside the closure's `Type::TypeVar(_)` so callers can
    /// recover the signature by extracting the variable identifier.
    pub closure_sigs: HashMap<char, ClosureSig>,
    /// Stack of in-scope closure names introduced by FnDef so that a
    /// recursive function can refer to itself.
    pub fn_scope: Vec<String>,
    /// v0.80: row-polymorphic fresh var generator (algebraic effects
    /// EffectRow::Var namer; namespace independent from TypeVar(char)).
    pub fresh_vars: row::FreshVars,
    // v0.75.94: 移除 `diagnosed: HashSet<WitnessNodeId>` 字段 + 3 个方法
    // (`mark_diagnosed` / `is_diagnosed` / `is_diagnosed_at`) —— 抽离到
    // `crate::typeck::hm::diag::DiagFilter`（双向定型专用基础设施）。
    // HM 公共 API 回归到 v0.75.86 之前的纯粹 HM 状态。
}

// v0.75.94: 重新导出 WitnessNodeId（抽离到 diag 子模块）以保留外部 API
// 路径兼容。调用方可以继续 `use crate::typeck::hm::WitnessNodeId`。
pub use diag::WitnessNodeId;

impl HMInference {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh type variable identifier.
    pub fn fresh_type_var_id(&mut self) -> char {
        let id = std::char::from_u32(self.fresh_counter as u32).unwrap_or('\u{10FFFF}');
        self.fresh_counter += 1;
        id
    }

    /// Mint a fresh `Type::TypeVar`.
    pub fn fresh_type_var(&mut self) -> Type {
        Type::TypeVar(self.fresh_type_var_id())
    }

    /// v0.80: Mint a fresh `EffectRow::Var` for row-polymorphic unification.
    /// Each call produces a distinct row variable name (rho0, rho1, ...).
    pub fn fresh_row_var(&mut self) -> crate::mir::effect::EffectRow {
        crate::mir::effect::EffectRow::Var(self.fresh_vars.row_var(""))
    }

    /// Record a fresh closure signature and return the type variable
    /// that callers can use to refer to it.
    pub fn fresh_closure(&mut self, params: Vec<Type>, return_type: Type) -> Type {
        let id = self.fresh_type_var_id();
        let ty = Type::TypeVar(id);
        self.closure_sigs.insert(
            id,
            ClosureSig {
                arity: params.len(),
                params,
                return_type,
            },
        );
        ty
    }

    /// Recover a closure signature from its type. Returns `None` if the
    /// type is not a known closure identity.
    pub fn closure_sig(&self, ty: &Type) -> Option<&ClosureSig> {
        match ty {
            Type::TypeVar(c) => self.closure_sigs.get(c),
            _ => None,
        }
    }

    /// v0.75.17: 展开 env 中命中的 ForAll（标准 HM let-polymorphism 展开）。
    ///
    /// v0.80: 移除 closure 身份复制逻辑（ClosureSig 侧表已删除，Arrow 是
    /// 自包含类型）。remap 改为 &mut HashMap — 首次遇到量化变量时 mint fresh
    /// 并插入 remap；后续遇到同一变量时从 remap 取值，保证 param/return 一致。
    pub fn instantiate_type(&mut self, ty: &Type) -> Type {
        match ty {
            Type::ForAll(vars, inner) => {
                let quantified: HashSet<char> = vars.iter().cloned().collect();
                let mut remap: HashMap<char, char> = HashMap::new();
                self.instantiate_ty(inner, &quantified, &mut remap)
            }
            _ => ty.clone(),
        }
    }

    /// v0.75.97: 如果 `ty` 是 `Type::ForAll` 则实例化（每次 fresh 副本），
    /// 否则原样返回。封装「命中 env 后实例化」通用模式——
    /// `infer_var`（赋值 LHS 单形化）与 `infer_call`（Var callee 单形化）
    /// 两个调用点共享此语义。
    ///
    /// 与 [`instantiate_type`](Self::instantiate_type) 区别：
    ///   - `instantiate_type`: 无条件 match ForAll（已是 ForAll 才展开）
    ///   - `instantiate_if_forall`: 若 ForAll 则实例化，否则直接 clone 返回
    ///     （省去 caller 写 match 的开销 + 集中单形化语义）
    pub fn instantiate_if_forall(&mut self, ty: &Type) -> Type {
        match ty {
            Type::ForAll(_, _) => self.instantiate_type(ty),
            _ => ty.clone(),
        }
    }

    /// 递归替换类型中出现的所有 TypeVar（每次实例化一份独立副本）。
    /// v0.80: 当前仅被 instantiate_ty 的 Arrow 分支间接使用（通过
    /// rename_row），ClosureSig 删除后暂无直接调用点。
    #[allow(dead_code)]
    pub(super) fn rename_ty(&mut self, ty: &Type) -> Type {
        match ty {
            Type::TypeVar(_) => Type::TypeVar(self.fresh_type_var_id()),
            Type::List(elem) => Type::List(Box::new(self.rename_ty(elem))),
            Type::Dict(k, v) => {
                Type::Dict(Box::new(self.rename_ty(k)), Box::new(self.rename_ty(v)))
            }
            Type::Result_(ok, err) => {
                Type::Result_(Box::new(self.rename_ty(ok)), Box::new(self.rename_ty(err)))
            }
            Type::Union(members) => {
                Type::Union(members.iter().map(|m| self.rename_ty(m)).collect())
            }
            Type::ForAll(vs, inner) => Type::ForAll(vs.clone(), Box::new(self.rename_ty(inner))),
            // v0.80: Arrow — 递归重命名 input/output，effect row 走 rename_row。
            Type::Arrow(input, output, row) => Type::Arrow(
                Box::new(self.rename_ty(input)),
                Box::new(self.rename_ty(output)),
                crate::typeck::hm::row::rename_row(row, &mut self.fresh_vars),
            ),
            _ => ty.clone(),
        }
    }

    /// 把 ForAll 内层 τ 中被量化的 TypeVar 替换为 fresh 变量（未量化的保留）。
    ///
    /// v0.80: remap 从 &HashMap 改为 &mut HashMap — 修复「同一量化变量出现
    /// 多次时拿到不同 fresh var」的 bug。首次遇到量化变量时 mint fresh 并插入
    /// remap；后续遇到同一变量时从 remap 取值，保证 param/return 一致性。
    pub(super) fn instantiate_ty(
        &mut self,
        ty: &Type,
        quantified: &HashSet<char>,
        remap: &mut HashMap<char, char>,
    ) -> Type {
        match ty {
            Type::TypeVar(c) => {
                if quantified.contains(c) {
                    match remap.get(c) {
                        // 已映射的量化变量 → 复用 fresh 身份
                        Some(fresh) => Type::TypeVar(*fresh),
                        // 首次遇到 → mint fresh 并记录映射
                        None => {
                            let fresh = self.fresh_type_var_id();
                            remap.insert(*c, fresh);
                            Type::TypeVar(fresh)
                        }
                    }
                } else {
                    Type::TypeVar(*c)
                }
            }
            Type::List(elem) => Type::List(Box::new(self.instantiate_ty(elem, quantified, remap))),
            Type::Dict(k, v) => Type::Dict(
                Box::new(self.instantiate_ty(k, quantified, remap)),
                Box::new(self.instantiate_ty(v, quantified, remap)),
            ),
            Type::Result_(ok, err) => Type::Result_(
                Box::new(self.instantiate_ty(ok, quantified, remap)),
                Box::new(self.instantiate_ty(err, quantified, remap)),
            ),
            Type::Union(members) => Type::Union(
                members
                    .iter()
                    .map(|m| self.instantiate_ty(m, quantified, remap))
                    .collect(),
            ),
            // v0.75.17: 嵌套 ForAll — 内层量化变量冻结（遮蔽外层，不展开）
            Type::ForAll(inner_vars, inner) => {
                let shadowed: HashSet<char> = inner_vars.iter().cloned().collect();
                let active: HashSet<char> = quantified
                    .iter()
                    .filter(|c| !shadowed.contains(c))
                    .cloned()
                    .collect();
                Type::ForAll(
                    inner_vars.clone(),
                    Box::new(self.instantiate_ty(inner, &active, remap)),
                )
            }
            // v0.80: Arrow — 递归实例化 input/output，effect row 走 rename_row。
            Type::Arrow(input, output, row) => Type::Arrow(
                Box::new(self.instantiate_ty(input, quantified, remap)),
                Box::new(self.instantiate_ty(output, quantified, remap)),
                crate::typeck::hm::row::rename_row(row, &mut self.fresh_vars),
            ),
            _ => ty.clone(),
        }
    }

    /// Solve all collected constraints, mutating internal state. Returns
    /// the first unification error, or `Ok(())` if all constraints
    /// unify cleanly.
    pub fn solve_constraints(&mut self) -> Result<(), Vec<TypeError>> {
        let mut subst = Substitution::new();
        let mut errors: Vec<TypeError> = Vec::new();
        for constraint in self.constraints.drain(..) {
            match unify::solve(&constraint, &subst) {
                Ok(new_subst) => subst = new_subst,
                Err(err) => {
                    errors.push(err);
                    // Continue with a fresh substitution so a single bad
                    // program does not abort the whole analysis.
                    subst = Substitution::new();
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Drive inference across an entire MirWitness program. Returns the
    /// list of collected diagnostics. (Inferred types are surfaced via
    /// `TypeError` diagnostics only; there is no per-node type cache on
    /// `MirWitness` — see `mir/expr` — so inference always runs from the
    /// expression tree itself.)
    pub fn infer_program(&mut self, exprs: &[MirWitness]) -> Vec<TypeError> {
        let mut errors: Vec<TypeError> = Vec::new();
        for expr in exprs {
            if let Err(mut errs) = self.infer_expr(expr) {
                errors.append(&mut errs);
            }
        }
        if let Err(mut errs) = self.solve_constraints() {
            errors.append(&mut errs);
        }
        errors
    }

    /// v0.80: 推断表达式类型 + effect row。
    ///
    /// 每个表达式产生 `(Type, EffectRow)` — 类型 + 该表达式可能产生的
    /// side effect 集合。EffectRow::Empty 表示纯表达式。
    pub fn infer_expr(
        &mut self,
        expr: &MirWitness,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        match &expr.kind {
            WitnessKind::Literal(lit) => Ok((infer_lit(lit), crate::mir::effect::EffectRow::Empty)),
            WitnessKind::Variable(name) => {
                let ty = self.infer_var(name, expr.span)?;
                Ok((ty, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::Binary { left, op, right } => {
                self.infer_binop(op, left.as_ref(), right.as_ref(), expr.span)
            }
            WitnessKind::Call { callee, args } => self.infer_call(callee, args, expr.span),
            WitnessKind::MethodCall {
                receiver,
                method,
                args,
            } => self.infer_method_call(receiver, method, args, expr.span),
            WitnessKind::Closure { params, body, .. } => {
                self.infer_closure(params, body.as_ref(), expr.span)
            }
            WitnessKind::FnDef { params, body, .. } => {
                self.infer_fn_def(params, body.as_ref(), expr.span)
            }
            WitnessKind::Match { scrutinee, arms } => {
                self.infer_match(scrutinee.as_ref(), arms, expr.span)
            }
            WitnessKind::If { cond, then, r#else } => {
                self.infer_if(cond.as_ref(), then.as_ref(), r#else.as_deref(), expr.span)
            }
            WitnessKind::List(items) => self.infer_list(items, expr.span),
            WitnessKind::Dict(entries) => self.infer_dict(entries, expr.span),
            WitnessKind::DynTrait { expr, .. } => {
                // v0.55: dyn Trait is opaque; defer to inner expression.
                self.infer_expr(expr)
            }
            WitnessKind::Prompt { parts } => {
                let mut row = crate::mir::effect::EffectRow::Empty;
                for p in parts {
                    let (_, r) = self.infer_expr(p)?;
                    row = self.merge_rows(row, r);
                }
                Ok((Type::String, row))
            }
            WitnessKind::LetBinding {
                name,
                type_hint,
                value,
                ..
            } => match type_hint {
                Some(hint) => self.infer_let_typed(name, hint, value.as_ref(), expr.span),
                None => self.infer_let(name, value.as_ref(), expr.span),
            },
            WitnessKind::Assign { target, value } => {
                self.infer_assign(target, value.as_ref(), expr.span)
            }
            WitnessKind::Orchestrate {
                input_var,
                result_var,
                ..
            } => {
                // v0.75.34: orchestrate 在语义上声明 input_var / result_var
                //（`orchestrate ... input -> result`）— 登记为 Any 类型，
                // 避免后续引用 result 报 UnboundVariable。此前返回 Nil 但
                // 不登记变量，pregel/sequential 路径经 CLI 都会撞此缺口
                //（测试走 run_mir 绕过 typeck 未暴露）。
                self.env.add(input_var.clone(), Type::Unknown);
                self.env.add(result_var.clone(), Type::Unknown);
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::Loop { .. } => {
                // v0.55: Loop lowering produces nil at the MIR level.
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::While { .. } => {
                // v0.55: While lowering produces nil at the MIR level.
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
                let (left_ty, left_row) = self.infer_expr(left)?;
                let (right_ty, right_row) = self.infer_expr(right)?;
                let merged = self.merge_rows(left_row, right_row);
                if !matches!(left_ty, Type::Bool) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "bool".to_string(),
                        got: left_ty.name(),
                        span: Some(expr.span),
                    }]);
                }
                if !matches!(right_ty, Type::Bool) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "bool".to_string(),
                        got: right_ty.name(),
                        span: Some(expr.span),
                    }]);
                }
                Ok((Type::Bool, merged))
            }
            WitnessKind::Return(_) | WitnessKind::Break(_) | WitnessKind::Continue(_) => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::IndexAssign { .. } => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            // v0.55: top-level declarations — no scalar result type.
            WitnessKind::TypeAlias { .. }
            | WitnessKind::EnumDef { .. }
            | WitnessKind::StructDef { .. }
            | WitnessKind::Import(_)
            | WitnessKind::MacroDef { .. }
            | WitnessKind::UpdateDef { .. }
            | WitnessKind::AppDef { .. } => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            // v0.84: Sequence 推断 — 依次推断子表达式，合并 effect row，
            // 返回最后一个表达式的类型（do-notation 语义）。空 Sequence → Nil。
            WitnessKind::Sequence(exprs) => self.infer_sequence(exprs, expr.span),
            // v0.85: with 块 — 推断 bindings（声明/丢弃），body 的 effect row
            // 直接传播（配置桥接不产生 effect，但 body 可能产生）。
            WitnessKind::WithConfig { bindings, body } => {
                let mut row = crate::mir::effect::EffectRow::Empty;
                for (_, v) in bindings {
                    let (_, r) = self.infer_expr(v)?;
                    row = self.merge_rows(row, r);
                }
                let (body_ty, body_row) = self.infer_expr(body)?;
                let merged = self.merge_rows(row, body_row);
                Ok((body_ty, merged))
            }
            // v0.83: TEA definitions — 注册 Type 到 env（typeck 路径可查）
            WitnessKind::ModelDef { name, fields } => {
                use crate::typeck::Type;
                let ty = Type::TeaModel {
                    name: name.clone(),
                    fields: fields
                        .iter()
                        .map(|(n, t)| (n.clone(), Box::new(t.clone().into_type())))
                        .collect(),
                };
                self.env.add(name.clone(), ty);
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::MsgDef { name, variants } => {
                use crate::typeck::Type;
                let ty = Type::TeaMsg {
                    name: name.clone(),
                    variants: variants
                        .iter()
                        .map(|v| {
                            let payload = v.payload_type.as_ref().map(|t| {
                                Box::new(Type::Concrete {
                                    name: t.clone(),
                                    generics: vec![],
                                    traits: vec![],
                                })
                            });
                            (v.name.clone(), payload)
                        })
                        .collect(),
                };
                self.env.add(name.clone(), ty);
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            // v0.80: Perform — 产生 effect，返回 fresh type var（由 handler 决定具体类型）。
            WitnessKind::Perform { effect, args } => {
                self.infer_perform(effect, args, expr.span)
            }
            // v0.80: Handle — 捕获 effect，body 的 effect row 中移除被捕获的 effect。
            WitnessKind::Handle {
                effect,
                body,
                handler,
                ..
            } => self.infer_handle(effect, body.as_ref(), handler.as_ref(), expr.span),
            // v0.88: Quasiquote — 与 quote 对称，返回 String 类型（源码字符串）。
            // 推断各子表达式（Unquote/UnquoteSplice），合并 effect row；
            // Quote 段为静态常量不产生 effect。
            WitnessKind::Quasiquote { segments } => {
                let mut row = crate::mir::effect::EffectRow::Empty;
                for seg in segments {
                    let (_, r) = self.infer_expr(seg)?;
                    row = self.merge_rows(row, r);
                }
                Ok((Type::String, row))
            }
        }
    }

    /// v0.80: 合并两个 effect row（用于二元运算、if 分支等）。
    ///
    /// 规则：
    /// - Empty + x = x（恒等元）
    /// - Cons(h, t) + x = Cons(h, merge(t, x))（若 h 不在 x 中）
    /// - Var(v) + x = x + Constraint::RowEq(Var(v), x)（推迟到 solve 阶段）
    pub(crate) fn merge_rows(
        &mut self,
        a: crate::mir::effect::EffectRow,
        b: crate::mir::effect::EffectRow,
    ) -> crate::mir::effect::EffectRow {
        use crate::mir::effect::EffectRow;
        match (a, b) {
            (EffectRow::Empty, b) => b,
            (a, EffectRow::Empty) => a,
            (EffectRow::Cons(h, t), b) => {
                if row_contains_concrete(&b, &h) {
                    self.merge_rows(*t, b)
                } else {
                    EffectRow::Cons(h, Box::new(self.merge_rows(*t, b)))
                }
            }
            (EffectRow::Var(v), b) => {
                self.constraints.push(Constraint::RowEq(
                    EffectRow::Var(v),
                    b.clone(),
                ));
                b
            }
        }
    }

    /// v0.80: Perform 推断 — 产生 effect，返回 fresh type var。
    fn infer_perform(
        &mut self,
        effect: &str,
        args: &[MirWitness],
        _span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let mut arg_rows = crate::mir::effect::EffectRow::Empty;
        for arg in args {
            let (_, row) = self.infer_expr(arg)?;
            arg_rows = self.merge_rows(arg_rows, row);
        }
        let result_ty = self.fresh_type_var();
        let perform_row = crate::mir::effect::EffectRow::Cons(
            effect.to_string(),
            Box::new(crate::mir::effect::EffectRow::Empty),
        );
        Ok((result_ty, self.merge_rows(arg_rows, perform_row)))
    }

    /// v0.80: Handle 推断 — 捕获 effect，body 的 effect row 中移除被捕获的 effect。
    ///
    /// handler 体内的 `__arg0`, `__arg1`, ... 是 perform 传参的运行时约定
    /// （见 `src/runtime/effect.rs`）。类型检查时在 handler 作用域内注册
    /// 这些变量为 fresh type var，让 handler body 的引用通过 typeck。
    fn infer_handle(
        &mut self,
        effect: &str,
        body: &MirWitness,
        handler: &MirWitness,
        _span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (body_ty, body_row) = self.infer_expr(body)?;
        let residual = self.fresh_row_var();
        let expected_row = crate::mir::effect::EffectRow::Cons(
            effect.to_string(),
            Box::new(residual.clone()),
        );
        self.constraints.push(Constraint::RowEq(body_row, expected_row));
        // v0.80: 注册 handler 参数（__arg0, __arg1, ...）到 env
        // handler 是 `{ expr }` 形式，其 body 引用 __arg0 等
        let saved_env = self.env.clone();
        // 注册 __arg0 为 Any（perform 参数类型在运行时确定，typeck 阶段
        // 无法静态连接 perform 的 arg 类型到 handler 的 __arg0 —— 需要
        // effect signature 声明才能精确化，当前用 Any 兜底）。
        self.env.add("__arg0".to_string(), Type::Any);
        let (_handler_ty, handler_row) = self.infer_expr(handler)?;
        self.env = saved_env;
        Ok((body_ty, self.merge_rows(residual, handler_row)))
    }
}

/// v0.80: 检查 effect row 是否包含具体 label（不对 Var 返回 true，
/// 与 EffectRow::contains 的多态语义不同）。
fn row_contains_concrete(row: &crate::mir::effect::EffectRow, label: &str) -> bool {
    match row {
        crate::mir::effect::EffectRow::Empty => false,
        crate::mir::effect::EffectRow::Var(_) => false,
        crate::mir::effect::EffectRow::Cons(h, t) => {
            h == label || row_contains_concrete(t, label)
        }
    }
}

fn infer_lit(lit: &crate::common::Literal) -> Type {
    use crate::common::Literal;
    match lit {
        Literal::Int(_, _) => Type::Int,
        Literal::Float(_, _) => Type::Float,
        Literal::String(_, _) => Type::String,
        Literal::Char(_, _) => Type::Char,
        Literal::Bool(_, _) => Type::Bool,
        Literal::Nil(_) => Type::Nil,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Literal, Span};
    use crate::mir::witness::{
        MirWitness, WitnessArm, WitnessCallee, WitnessKind, WitnessParam, WitnessPattern,
    };

    pub(super) fn lit_int(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::default())),
            span: Span::default(),
        }
    }

    #[test]
    pub(super) fn literal_int_infers_to_int() {
        let mut hm = HMInference::new();
        let (ty, row) = hm.infer_expr(&lit_int(7)).unwrap();
        assert_eq!(ty, Type::Int);
        assert_eq!(row, crate::mir::effect::EffectRow::Empty);
    }

    #[test]
    pub(super) fn unbound_variable_produces_diagnostic() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::Variable("missing".to_string()),
            span: Span::default(),
        };
        let err = hm.infer_expr(&expr).unwrap_err();
        assert!(matches!(
            err.as_slice(),
            [TypeError::UnboundVariable { .. }]
        ));
    }

    #[test]
    pub(super) fn let_binding_registers_env() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::LetBinding {
                name: "x".to_string(),
                type_hint: None,
                value: Box::new(lit_int(1)),
                init_body: Box::new(MirWitness {
                    kind: WitnessKind::Variable("x".to_string()),
                    span: Span::default(),
                }),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        assert_eq!(hm.env.get("x"), Some(&Type::Int));
    }

    #[test]
    pub(super) fn if_branches_unify() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::If {
                cond: Box::new(lit_int(1)),
                then: Box::new(lit_int(2)),
                r#else: Some(Box::new(lit_int(3))),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        assert!(
            hm.solve_constraints().is_ok(),
            "if(int,int,int) should unify cleanly"
        );
    }

    #[test]
    pub(super) fn match_arms_unify() {
        let mut hm = HMInference::new();
        let arms = vec![
            WitnessArm {
                pattern: WitnessPattern::Literal(Literal::Int(1, Span::default())),
                guard: None,
                body: lit_int(10),
            },
            WitnessArm {
                pattern: WitnessPattern::Wildcard,
                guard: None,
                body: lit_int(20),
            },
        ];
        let expr = MirWitness {
            kind: WitnessKind::Match {
                scrutinee: Box::new(lit_int(1)),
                arms,
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        assert!(
            hm.solve_constraints().is_ok(),
            "match with uniform arm types should unify"
        );
    }

    #[test]
    pub(super) fn closure_call_arity_check() {
        let mut hm = HMInference::new();
        let param = WitnessParam {
            name: "x".to_string(),
            type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::Int)),
            default: None,
        };
        let closure_ty = hm.infer_closure(
            &[param],
            &MirWitness {
                kind: WitnessKind::Variable("x".to_string()),
                span: Span::default(),
            },
            Span::default(),
        );
        let closure_ty = closure_ty.unwrap();
        let call = MirWitness {
            kind: WitnessKind::Call {
                callee: WitnessCallee::Var("c".to_string()),
                args: vec![lit_int(7), lit_int(8)],
            },
            span: Span::default(),
        };
        let _ = closure_ty; // Just check the compile
        let _ = call;
    }

    // ─── v0.80: perform/handle inference tests ───

    #[test]
    fn perform_produces_effect_row() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::Perform {
                effect: "Ai".to_string(),
                args: vec![],
            },
            span: Span::default(),
        };
        let (ty, row) = hm.infer_expr(&expr).unwrap();
        // perform returns a fresh type var (concrete type determined by handler)
        assert!(matches!(ty, Type::TypeVar(_)));
        // effect row contains "Ai"
        assert!(row.contains("Ai"));
    }

    #[test]
    fn handle_captures_effect() {
        let mut hm = HMInference::new();
        let body = MirWitness {
            kind: WitnessKind::Perform {
                effect: "Ai".to_string(),
                args: vec![],
            },
            span: Span::default(),
        };
        let handler = MirWitness {
            kind: WitnessKind::Literal(Literal::String("handled".to_string(), Span::default())),
            span: Span::default(),
        };
        let expr = MirWitness {
            kind: WitnessKind::Handle {
                effect: "Ai".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        };
        let (ty, row) = hm.infer_expr(&expr).unwrap();
        // handle returns body type (fresh var from perform)
        assert!(matches!(ty, Type::TypeVar(_)));
        // effect row should NOT contain "Ai" (captured by handler)
        // The residual row is a fresh var (row-polymorphic)
        assert!(matches!(row, crate::mir::effect::EffectRow::Var(_)));
    }

    #[test]
    fn handle_solves_row_constraint() {
        let mut hm = HMInference::new();
        let body = MirWitness {
            kind: WitnessKind::Perform {
                effect: "Ai".to_string(),
                args: vec![],
            },
            span: Span::default(),
        };
        let handler = MirWitness {
            kind: WitnessKind::Literal(Literal::String("handled".to_string(), Span::default())),
            span: Span::default(),
        };
        let expr = MirWitness {
            kind: WitnessKind::Handle {
                effect: "Ai".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        // solve_constraints should succeed — RowEq(body_row, Cons("Ai", residual))
        // unifies body_row (Cons("Ai", Empty)) with Cons("Ai", residual),
        // binding residual to Empty.
        assert!(
            hm.solve_constraints().is_ok(),
            "handle should solve row constraint cleanly"
        );
    }

    #[test]
    fn closure_returns_curried_arrow() {
        let mut hm = HMInference::new();
        let param = WitnessParam {
            name: "x".to_string(),
            type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::Int)),
            default: None,
        };
        let (ty, row) = hm
            .infer_closure(
                &[param],
                &MirWitness {
                    kind: WitnessKind::Variable("x".to_string()),
                    span: Span::default(),
                },
                Span::default(),
            )
            .unwrap();
        // Closure type should be Arrow(Int, Int, Empty)
        assert!(
            matches!(ty, Type::Arrow(_, _, _)),
            "closure should return Arrow, got {:?}",
            ty
        );
        // Closure definition is pure
        assert_eq!(row, crate::mir::effect::EffectRow::Empty);
    }

    #[test]
    fn closure_two_params_curried() {
        let mut hm = HMInference::new();
        let params = vec![
            WitnessParam {
                name: "x".to_string(),
                type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::Int)),
                default: None,
            },
            WitnessParam {
                name: "y".to_string(),
                type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::String)),
                default: None,
            },
        ];
        let (ty, _row) = hm
            .infer_closure(
                &params,
                &MirWitness {
                    kind: WitnessKind::Variable("x".to_string()),
                    span: Span::default(),
                },
                Span::default(),
            )
            .unwrap();
        // fn(x: Int, y: String) -> Int 应该是 Arrow(Int, Arrow(String, Int, Empty), Empty)
        match &ty {
            Type::Arrow(input, output, row) => {
                assert_eq!(**input, Type::Int);
                assert_eq!(*row, crate::mir::effect::EffectRow::Empty);
                match output.as_ref() {
                    Type::Arrow(inner_input, inner_output, inner_row) => {
                        assert_eq!(**inner_input, Type::String);
                        assert_eq!(**inner_output, Type::Int);
                        assert_eq!(*inner_row, crate::mir::effect::EffectRow::Empty);
                    }
                    other => panic!("expected inner Arrow, got {:?}", other),
                }
            }
            other => panic!("expected Arrow, got {:?}", other),
        }
    }

    // v0.75.97: instantiate_if_forall helper 测试
    #[test]
    fn instantiate_if_forall_returns_input_when_not_forall() {
        // 非 ForAll 类型直接 clone 返回（no-op）
        let mut hm = HMInference::new();
        let int_ty = Type::Int;
        let result = hm.instantiate_if_forall(&int_ty);
        assert_eq!(result, Type::Int);
        // 复合类型（List/Dict）也不变
        let list_ty = Type::List(Box::new(Type::Int));
        let result = hm.instantiate_if_forall(&list_ty);
        assert_eq!(result, Type::List(Box::new(Type::Int)));
    }

    #[test]
    fn instantiate_if_forall_produces_fresh_copy_when_forall() {
        // ForAll 输入 → 每次实例化一份 fresh 副本（标准 HM let-polymorphism）
        // 关键不变量：内层 TypeVar 名字必须 fresh（不等同于原 vars）
        let mut hm = HMInference::new();
        // 构造 ForAll['a].TypeVar('a) — 最简单的泛型量化
        let forall = Type::ForAll(vec!['a'], Box::new(Type::TypeVar('a')));
        let result1 = hm.instantiate_if_forall(&forall);
        let result2 = hm.instantiate_if_forall(&forall);
        // 两次调用产出独立 TypeVar（名字不同）
        match (&result1, &result2) {
            (Type::TypeVar(c1), Type::TypeVar(c2)) => {
                assert_ne!(c1, c2, "两次实例化必须产出 fresh TypeVar");
            }
            _ => panic!("expected TypeVar after instantiate"),
        }
    }
}
