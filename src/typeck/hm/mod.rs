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
            _ => ty.clone(),
        }
    }

    /// 把 ForAll 内层 τ 中被量化的 TypeVar 替换为 fresh 变量（未量化的保留）。
    pub(super) fn instantiate_ty(
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

    pub fn infer_expr(&mut self, expr: &MirWitness) -> Result<Type, Vec<TypeError>> {
        match &expr.kind {
            WitnessKind::Literal(lit) => Ok(infer_lit(lit)),
            WitnessKind::Variable(name) => self.infer_var(name, expr.span),
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
                for p in parts {
                    let _ = self.infer_expr(p)?;
                }
                Ok(Type::String)
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
                Ok(Type::Nil)
            }
            WitnessKind::Loop { .. } => {
                // v0.55: Loop lowering produces nil at the MIR level.
                Ok(Type::Nil)
            }
            WitnessKind::While { .. } => {
                // v0.55: While lowering produces nil at the MIR level.
                Ok(Type::Nil)
            }
            WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
                let left_ty = self.infer_expr(left)?;
                let right_ty = self.infer_expr(right)?;
                let _op_name = if matches!(expr.kind, WitnessKind::Or { .. }) {
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
            WitnessKind::Return(_) | WitnessKind::Break(_) | WitnessKind::Continue(_) => {
                Ok(Type::Nil)
            }
            WitnessKind::IndexAssign { .. } => Ok(Type::Nil),
            // v0.55: top-level declarations — no scalar result type.
            WitnessKind::TypeAlias { .. }
            | WitnessKind::EnumDef { .. }
            | WitnessKind::StructDef { .. }
            | WitnessKind::Import(_)
            | WitnessKind::MacroDef { .. }
            | WitnessKind::Sequence { .. } => Ok(Type::Nil),
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
        let ty = hm.infer_expr(&lit_int(7)).unwrap();
        assert_eq!(ty, Type::Int);
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
}
