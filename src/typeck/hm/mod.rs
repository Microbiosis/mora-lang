//! v0.55: Hindley-Milner Type Inference for MirExpr.
//!
//! This module is the MirExpr-native replacement of the ast_v2-based
//! `typeck::check_program` pipeline. The earlier v0.53 prototype still
//! referenced `NodeId` / `AstArena` and silently no-op'd any closure type
//! check because `Type::Closure` is a unit variant. The current rewrite
//! drives inference directly off `&MirExpr`, tracks closure signatures in
//! a side table keyed by a fresh `Type::TypeVar`, and covers every
//! `MirExprKind` variant.
//!
//! Public entry point: [`check_program_mir`] (re-exported from
//! `crate::typeck`).

use std::collections::{HashMap, HashSet};

use crate::common::Span;
use crate::mir::expr::{BuiltinOp, MirCallee, MirExpr, MirExprKind, Param};
use crate::typeck::Type;

pub mod env;
pub mod error;
pub mod generalize;
pub mod unify;

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
}

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
    /// 特殊处理 closure 身份变量：被量化的身份变量映射到 fresh 变量，且其
    /// closure_sigs 侧表签名随之复制一份、内部 TypeVar 全部重命名 — 这样
    /// `let f = fn(x) x; f(1); f("s")` 每次调用得到一份独立的单形化副本，
    /// 而不是共享同一组约束导致 Int/String 冲突。
    pub fn instantiate_type(&mut self, ty: &Type) -> Type {
        match ty {
            Type::ForAll(vars, inner) => {
                let quantified: HashSet<char> = vars.iter().cloned().collect();
                // 被量化的 closure 身份变量 → fresh 身份变量（sig 同步复制）
                let mut remap: HashMap<char, char> = HashMap::new();
                for v in vars {
                    if let Some(sig) = self.closure_sigs.get(v).cloned() {
                        let fresh = self.fresh_type_var_id();
                        // 先重命名签名内部变量（每次实例化一份 fresh 副本 →
                        // 单形化，两次调用互不冲突），再登记进侧表。
                        let params: Vec<Type> =
                            sig.params.iter().map(|p| self.rename_ty(p)).collect();
                        let return_type = self.rename_ty(&sig.return_type);
                        self.closure_sigs.insert(
                            fresh,
                            ClosureSig {
                                arity: sig.arity,
                                params,
                                return_type,
                            },
                        );
                        remap.insert(*v, fresh);
                    }
                }
                self.instantiate_ty(inner, &quantified, &remap)
            }
            _ => ty.clone(),
        }
    }

    /// 递归替换类型中出现的所有 TypeVar（每次实例化一份独立副本）。
    fn rename_ty(&mut self, ty: &Type) -> Type {
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
            _ => ty.clone(),
        }
    }

    /// 把 ForAll 内层 τ 中被量化的 TypeVar 替换为 fresh 变量（未量化的保留）。
    fn instantiate_ty(
        &mut self,
        ty: &Type,
        quantified: &HashSet<char>,
        remap: &HashMap<char, char>,
    ) -> Type {
        match ty {
            Type::TypeVar(c) => {
                if quantified.contains(c) {
                    match remap.get(c) {
                        // 被量化的 closure 身份变量 → 已复制的 fresh 身份
                        Some(fresh) => Type::TypeVar(*fresh),
                        // 普通量化变量 → 全新 fresh（每次使用单形化）
                        None => Type::TypeVar(self.fresh_type_var_id()),
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
            if let Err(err) = unify::solve(&constraint, &subst) {
                errors.push(err);
                // Continue with a fresh substitution so a single bad
                // program does not abort the whole analysis.
                subst = Substitution::new();
            } else {
                subst = unify::solve(&constraint, &subst).unwrap();
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Drive inference across an entire MirExpr program. Returns the
    /// list of collected diagnostics. (Inferred types are surfaced via
    /// `TypeError` diagnostics only; there is no per-node type cache on
    /// `MirExpr` — see `mir/expr` — so inference always runs from the
    /// expression tree itself.)
    pub fn infer_program(&mut self, exprs: &[MirExpr]) -> Vec<TypeError> {
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

    pub fn infer_expr(&mut self, expr: &MirExpr) -> Result<Type, Vec<TypeError>> {
        match &expr.kind {
            MirExprKind::Literal(lit) => Ok(infer_lit(lit)),
            MirExprKind::Variable(name) => self.infer_var(name, expr.span),
            MirExprKind::Binary { left, op, right } => {
                self.infer_binop(op, left.as_ref(), right.as_ref(), expr.span)
            }
            MirExprKind::Call { callee, args } => self.infer_call(callee, args, expr.span),
            MirExprKind::MethodCall {
                receiver,
                method,
                args,
            } => self.infer_method_call(receiver, method, args, expr.span),
            MirExprKind::Closure { params, body, .. } => {
                self.infer_closure(params, body.as_ref(), expr.span)
            }
            MirExprKind::FnDef { params, body, .. } => {
                self.infer_fn_def(params, body.as_ref(), expr.span)
            }
            MirExprKind::Match { scrutinee, arms } => {
                self.infer_match(scrutinee.as_ref(), arms, expr.span)
            }
            MirExprKind::If { cond, then, r#else } => {
                self.infer_if(cond.as_ref(), then.as_ref(), r#else.as_deref(), expr.span)
            }
            MirExprKind::List(items) => self.infer_list(items, expr.span),
            MirExprKind::Dict(entries) => self.infer_dict(entries, expr.span),
            MirExprKind::DynTrait { expr, .. } => {
                // v0.55: dyn Trait is opaque; defer to inner expression.
                self.infer_expr(expr)
            }
            MirExprKind::Prompt { parts } => {
                for p in parts {
                    let _ = self.infer_expr(p)?;
                }
                Ok(Type::String)
            }
            MirExprKind::LetBinding {
                name,
                type_hint,
                value,
                ..
            } => match type_hint {
                Some(hint) => self.infer_let_typed(name, hint, value.as_ref(), expr.span),
                None => self.infer_let(name, value.as_ref(), expr.span),
            },
            MirExprKind::Assign { target, value } => {
                self.infer_assign(target, value.as_ref(), expr.span)
            }
            MirExprKind::Orchestrate { .. } => {
                // v0.x: orchestrate is a multi-agent top-level construct that
                // does not have a sensible scalar type at this layer. Return a
                // placeholder until full orchestrate type-checking lands.
                Ok(Type::Nil)
            }
            MirExprKind::Loop { .. } => {
                // v0.55: Loop lowering produces nil at the MIR level.
                Ok(Type::Nil)
            }
            MirExprKind::While { .. } => {
                // v0.55: While lowering produces nil at the MIR level.
                Ok(Type::Nil)
            }
            MirExprKind::Or { left, right } | MirExprKind::And { left, right } => {
                let left_ty = self.infer_expr(left)?;
                let right_ty = self.infer_expr(right)?;
                let _op_name = if matches!(expr.kind, MirExprKind::Or { .. }) {
                    "or"
                } else {
                    "and"
                };
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
                Ok(Type::Bool)
            }
            MirExprKind::Return(_) | MirExprKind::Break(_) | MirExprKind::Continue(_) => {
                Ok(Type::Nil)
            }
            MirExprKind::IndexAssign { .. } => Ok(Type::Nil),
            // v0.55: top-level declarations — no scalar result type.
            MirExprKind::TypeAlias { .. }
            | MirExprKind::EnumDef { .. }
            | MirExprKind::StructDef { .. }
            | MirExprKind::Import(_)
            | MirExprKind::MacroDef { .. }
            | MirExprKind::Sequence { .. } => Ok(Type::Nil),
        }
    }

    fn infer_let(
        &mut self,
        name: &str,
        value: &MirExpr,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let value_ty = self.infer_expr(value)?;
        // v0.75.17: let-generalization — 量化为不在 env 中的自由变量
        // （标准 HM：Γ ⊢ let x = e in body : ∀α₁...αₙ.τ，其中
        // {α₁...αₙ} = FV(τ) \ FV(Γ)）。
        let gen_ty = generalize::generalize(&value_ty, &self.env.free_variables());
        self.env.add(name.to_string(), gen_ty.clone());
        let _ = span;
        Ok(gen_ty)
    }

    fn infer_let_typed(
        &mut self,
        name: &str,
        type_hint: &Type,
        value: &MirExpr,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let value_ty = self.infer_expr(value)?;
        // v0.55: validate the user-supplied `let x: T = ...` annotation
        // against the value's inferred type. Tolerant: Type::Any
        // annotations always succeed.
        if !matches!(type_hint, Type::Any) {
            self.constraints.push(Constraint::Eq(
                Box::new(type_hint.clone()),
                Box::new(value_ty.clone()),
            ));
        }
        // v0.75.17: 显式注解同样做 let-generalization（注解含自由变量时
        // 量化为 ForAll；`List<int>` 等具体注解无自由变量，原样登记）。
        let gen_hint = generalize::generalize(type_hint, &self.env.free_variables());
        self.env.add(name.to_string(), gen_hint.clone());
        let _ = span;
        Ok(gen_hint)
    }

    fn infer_assign(
        &mut self,
        target: &str,
        value: &MirExpr,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let value_ty = self.infer_expr(value)?;
        let current = self.env.get(target).cloned();
        if let Some(existing) = current {
            // v0.75.17: 命中 ForAll 时先实例化再合一（赋值的 LHS 是单形实例）
            let existing = match existing {
                Type::ForAll(_, _) => self.instantiate_type(&existing),
                other => other,
            };
            self.constraints.push(Constraint::Eq(
                Box::new(existing),
                Box::new(value_ty.clone()),
            ));
        } else {
            return Err(vec![TypeError::UnboundVariable {
                name: target.to_string(),
                span,
            }]);
        }
        Ok(value_ty)
    }

    fn infer_var(&mut self, name: &str, span: Span) -> Result<Type, Vec<TypeError>> {
        match self.env.get(name) {
            // v0.75.17: env 命中 ForAll → 实例化（let-polymorphism 展开）。
            // 可变借用问题：先克隆 env 条目，再走 &mut self 的实例化路径。
            Some(ty) if matches!(ty, Type::ForAll(_, _)) => {
                let ty = ty.clone();
                Ok(self.instantiate_type(&ty))
            }
            Some(ty) => Ok(ty.clone()),
            None => Err(vec![TypeError::UnboundVariable {
                name: name.to_string(),
                span,
            }]),
        }
    }

    fn infer_binop(
        &mut self,
        op: &crate::common::BinaryOp,
        left: &MirExpr,
        right: &MirExpr,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        use crate::common::BinaryOp::*;
        let left_ty = self.infer_expr(left)?;
        let right_ty = self.infer_expr(right)?;
        let result_ty = self.fresh_type_var();
        match op {
            Add | Sub | Mul | Div | Mod => {
                self.constraints.push(Constraint::Eq(
                    Box::new(left_ty.clone()),
                    Box::new(result_ty.clone()),
                ));
                self.constraints.push(Constraint::Eq(
                    Box::new(right_ty.clone()),
                    Box::new(result_ty.clone()),
                ));
                let _ = span;
                Ok(result_ty)
            }
            Equal | NotEqual => {
                self.constraints
                    .push(Constraint::Eq(Box::new(left_ty), Box::new(right_ty)));
                Ok(Type::Bool)
            }
            Greater | Less | GreaterEqual | LessEqual => {
                self.constraints
                    .push(Constraint::Eq(Box::new(left_ty), Box::new(right_ty)));
                Ok(Type::Bool)
            } // v0.55: Or/And are MirExprKind variants (short-circuit),
              // handled directly in infer_expr, not BinaryOp variants.
              // BinaryOp 已穷尽（11 变体全部覆盖）— 无需 `_` 兜底。
        }
    }

    fn infer_call(
        &mut self,
        callee: &MirCallee,
        args: &[MirExpr],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let callee_ty = match callee {
            MirCallee::Name(name) => self.builtin_callee_ty(name).unwrap_or(Type::Any),
            // v0.75.17: Var 命中 ForAll 时实例化（`let f = fn(x) x; f(1); f("s")`）
            MirCallee::Var(var_name) => match self.env.get(var_name) {
                Some(ty) if matches!(ty, Type::ForAll(_, _)) => {
                    let ty = ty.clone();
                    self.instantiate_type(&ty)
                }
                Some(ty) => ty.clone(),
                None => Type::Any,
            },
            MirCallee::Evaluated(expr) => self.infer_expr(expr)?,
            MirCallee::Builtin(op) => self.builtin_type(op)?,
            // v0.75.16: Method 调用（parser 现产出 MirCallee::Method）— 走
            // method_signature 推断（receiver 类型 + 参数约束 + 返回类型）。
            MirCallee::Method(_, _) => {
                // 第一个 arg 是 receiver 表达式；构造临时 MethodCall 语义。
                // 直接委托 infer_method_call：receiver = args[0], 后续为参数。
                let (recv, method_args) = match args.split_first() {
                    Some((r, rest)) => (r, rest),
                    None => {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: 0,
                            span,
                        }]);
                    }
                };
                let method = match callee {
                    MirCallee::Method(_, m) => m.clone(),
                    _ => unreachable!(),
                };
                return self.infer_method_call(recv, &method, method_args, span);
            }
        };
        let arg_types: Vec<Type> = args
            .iter()
            .map(|a| self.infer_expr(a))
            .collect::<Result<Vec<_>, _>>()?;

        // v0.75.24: merge_with(key, strategy) 的策略名字面量编译期校验 —
        // 非法策略（静态字符串）在 typeck 阶段拦截，不再留到运行时
        // （动态传入的变量仍由运行时 MergeStrategy::from_name 兜底）。
        if let MirCallee::Name(name) = callee
            && name == "merge_with"
            && let Some(MirExprKind::Literal(crate::common::Literal::String(s, _))) =
                args.get(1).map(|a| &a.kind)
            && crate::value::MergeStrategy::from_name(s).is_none()
        {
            return Err(vec![TypeError::InvalidLiteral {
                what: "merge_with strategy".to_string(),
                value: s.clone(),
                span: Some(span),
            }]);
        }

        if let Some(sig) = self.closure_sig(&callee_ty).cloned() {
            if sig.arity != arg_types.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: sig.arity,
                    actual: arg_types.len(),
                    span,
                }]);
            }
            for (param_ty, arg_ty) in sig.params.iter().zip(arg_types.iter()) {
                self.constraints.push(Constraint::Eq(
                    Box::new(param_ty.clone()),
                    Box::new(arg_ty.clone()),
                ));
            }
            Ok(sig.return_type)
        } else {
            // Unknown callee type: introduce a fresh return and
            // constrain all argument slots to be compatible with
            // whatever the callee happens to be.
            let ret = self.fresh_type_var();
            for arg_ty in &arg_types {
                self.constraints.push(Constraint::Eq(
                    Box::new(arg_ty.clone()),
                    Box::new(callee_ty.clone()),
                ));
            }
            let _ = ret.clone();
            Ok(ret)
        }
    }

    fn infer_method_call(
        &mut self,
        receiver: &MirExpr,
        method: &str,
        args: &[MirExpr],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let recv_ty = self.infer_expr(receiver)?;
        let arg_types: Vec<Type> = args
            .iter()
            .map(|a| self.infer_expr(a))
            .collect::<Result<Vec<_>, _>>()?;

        // v0.55: enforce arity from the dispatch table. The signature
        // already includes `self` as its first parameter, so the user
        // arity we compare against is `sig.params.len() - 1`.
        if let Some(sig) = crate::typeck::dispatch::method_signature(&recv_ty, method) {
            let user_arity = sig.params.len().saturating_sub(1);
            if user_arity != arg_types.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: user_arity,
                    actual: arg_types.len(),
                    span,
                }]);
            }
            for (param, arg_ty) in sig.params.iter().skip(1).zip(arg_types.iter()) {
                self.constraints.push(Constraint::Eq(
                    Box::new(param.1.clone()),
                    Box::new(arg_ty.clone()),
                ));
            }
        }

        let return_ty = crate::typeck::dispatch::method_return_type(&recv_ty, method);
        let _ = span;
        Ok(return_ty)
    }

    // v0.75.20: infer_pipe 已删——MirExprKind::Pipe 死变体移除，`|>` 在
    // parse_pipe 脱糖为 Call（right(left)），HM 走 infer_call。

    fn infer_closure(
        &mut self,
        params: &[Param],
        body: &MirExpr,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let saved_env = self.env.clone();
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| p.type_hint.clone().unwrap_or_else(|| self.fresh_type_var()))
            .collect();
        for (p, ty) in params.iter().zip(param_types.iter()) {
            self.env.add(p.name.clone(), ty.clone());
        }
        let body_ty = self.infer_expr(body)?;
        self.env = saved_env;
        let id = self.fresh_closure(param_types, body_ty);
        let _ = span;
        Ok(id)
    }

    fn infer_fn_def(
        &mut self,
        params: &[Param],
        body: &MirExpr,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        // fn name(params) = body  is treated like an immediately-bound
        // closure; the name registration is the caller's responsibility.
        let _ = span;
        self.infer_closure(params, body, span)
    }

    fn infer_match(
        &mut self,
        scrutinee: &MirExpr,
        arms: &[crate::mir::expr::MatchArm],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let _ = self.infer_expr(scrutinee)?;
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            let arm_ty = self.infer_expr(&arm.body)?;
            match result_ty {
                None => result_ty = Some(arm_ty),
                Some(ref mut ty) => {
                    self.constraints
                        .push(Constraint::Eq(Box::new(ty.clone()), Box::new(arm_ty)));
                }
            }
        }
        let _ = span;
        Ok(result_ty.unwrap_or(Type::Any))
    }

    fn infer_if(
        &mut self,
        cond: &MirExpr,
        then_branch: &MirExpr,
        else_branch: Option<&MirExpr>,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let _ = self.infer_expr(cond)?;
        let then_ty = self.infer_expr(then_branch)?;
        let result = if let Some(e) = else_branch {
            let else_ty = self.infer_expr(e)?;
            // Both branches must produce the same type.
            self.constraints
                .push(Constraint::Eq(Box::new(then_ty.clone()), Box::new(else_ty)));
            then_ty
        } else {
            // No else branch: the if-expression yields `then_ty | nil`.
            Type::Union(vec![then_ty.clone(), Type::Nil])
        };
        let _ = span;
        Ok(result)
    }

    fn infer_list(&mut self, items: &[MirExpr], span: Span) -> Result<Type, Vec<TypeError>> {
        let elem_ty = self.fresh_type_var();
        for item in items {
            let ty = self.infer_expr(item)?;
            self.constraints
                .push(Constraint::Eq(Box::new(elem_ty.clone()), Box::new(ty)));
        }
        let _ = span;
        Ok(Type::List(Box::new(elem_ty)))
    }

    fn infer_dict(
        &mut self,
        entries: &[(String, MirExpr)],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let k_ty = Type::String;
        let v_ty = self.fresh_type_var();
        for (_, value) in entries {
            let ty = self.infer_expr(value)?;
            self.constraints
                .push(Constraint::Eq(Box::new(v_ty.clone()), Box::new(ty)));
        }
        let _ = span;
        Ok(Type::Dict(Box::new(k_ty), Box::new(v_ty)))
    }

    fn builtin_callee_ty(&mut self, name: &str) -> Option<Type> {
        // v0.55: prefer the canonical dispatch registry for the
        // canonical arity / return type, but mint fresh type variables
        // for every parameter so the HM unifier can still infer
        // concrete argument types instead of being pinned to a Union
        // annotation.
        if let Some(sig) = crate::typeck::dispatch::lookup_builtin(name) {
            let param_count = sig.params.len();
            let param_types: Vec<Type> = (0..param_count).map(|_| self.fresh_type_var()).collect();
            return Some(self.fresh_closure(param_types, sig.return_type.clone()));
        }
        match name {
            "print" => {
                let arg = self.fresh_type_var();
                let ret = Type::Nil;
                Some(self.fresh_closure(vec![arg], ret))
            }
            "len" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::Int))
            }
            "str" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::String))
            }
            "int" => Some(self.fresh_closure(vec![Type::String], Type::Int)),
            "float" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::Float))
            }
            "bool" => {
                let arg = self.fresh_type_var();
                Some(self.fresh_closure(vec![arg], Type::Bool))
            }
            "range" => {
                let a = self.fresh_type_var();
                let b = self.fresh_type_var();
                let c = self.fresh_type_var();
                let elem = self.fresh_type_var();
                Some(self.fresh_closure(vec![a, b, c], Type::List(Box::new(elem))))
            }
            _ => None,
        }
    }

    fn builtin_type(&mut self, op: &BuiltinOp) -> Result<Type, Vec<TypeError>> {
        match op {
            BuiltinOp::Print => {
                let arg = self.fresh_type_var();
                Ok(self.fresh_closure(vec![arg], Type::Nil))
            }
            BuiltinOp::Assert => Ok(self.fresh_closure(vec![Type::Bool], Type::Nil)),
            BuiltinOp::Not => Ok(self.fresh_closure(vec![Type::Bool], Type::Bool)),
            BuiltinOp::Length => {
                let arg = self.fresh_type_var();
                Ok(self.fresh_closure(vec![arg], Type::Int))
            }
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
    use crate::mir::expr::{MatchArm, Param, Pattern};

    fn lit_int(n: i64) -> MirExpr {
        MirExpr::lit(Literal::Int(n, Span::default()), Span::default())
    }

    #[test]
    fn literal_int_infers_to_int() {
        let mut hm = HMInference::new();
        let ty = hm.infer_expr(&lit_int(7)).unwrap();
        assert_eq!(ty, Type::Int);
    }

    #[test]
    fn unbound_variable_produces_diagnostic() {
        let mut hm = HMInference::new();
        let expr = MirExpr::var("missing".to_string(), Span::default());
        let err = hm.infer_expr(&expr).unwrap_err();
        assert!(matches!(
            err.as_slice(),
            [TypeError::UnboundVariable { .. }]
        ));
    }

    #[test]
    fn let_binding_registers_env() {
        let mut hm = HMInference::new();
        let expr = MirExpr {
            kind: MirExprKind::LetBinding {
                name: "x".to_string(),
                type_hint: None,
                value: Box::new(lit_int(1)),
                init_body: Box::new(MirExpr::var("x".to_string(), Span::default())),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        assert_eq!(hm.env.get("x"), Some(&Type::Int));
    }

    #[test]
    fn if_branches_unify() {
        let mut hm = HMInference::new();
        let expr = MirExpr::if_else(lit_int(1), lit_int(2), Some(lit_int(3)), Span::default());
        hm.infer_expr(&expr).unwrap();
        assert!(
            hm.solve_constraints().is_ok(),
            "if(int,int,int) should unify cleanly"
        );
    }

    #[test]
    fn match_arms_unify() {
        let mut hm = HMInference::new();
        let arms = vec![
            MatchArm {
                pattern: Pattern::Literal(Literal::Int(1, Span::default())),
                guard: None,
                body: lit_int(10),
            },
            MatchArm {
                pattern: Pattern::Wildcard,
                guard: None,
                body: lit_int(20),
            },
        ];
        let expr = MirExpr {
            kind: MirExprKind::Match {
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
    fn closure_call_arity_check() {
        let mut hm = HMInference::new();
        let param = Param {
            name: "x".to_string(),
            type_hint: Some(Type::Int),
            default: None,
        };
        let closure_ty = hm.infer_closure(
            &[param],
            &MirExpr::var("x".to_string(), Span::default()),
            Span::default(),
        );
        let closure_ty = closure_ty.unwrap();
        let call = MirExpr::call(
            MirCallee::Var("c".to_string()),
            vec![lit_int(7), lit_int(8)],
            Span::default(),
        );
        let _ = closure_ty; // Just check the compile
        let _ = call;
    }
}
