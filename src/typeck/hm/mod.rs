//! v0.110.0: 基于 MirWitness 的 Hindley-Milner 类型推断引擎。
//!
//! 推断直接消费 `&[MirWitness]`（parse 层产出的 witness 树），并覆盖
//! `WitnessKind` 的每个变体。闭包与函数以柯里化的 `Type::Arrow` 表示；
//! 闭包的参数规格与效果行按定义 span 登记在 `closure_param_specs` /
//! `closure_rows`。
//!
//! 入口：[`crate::typeck::check_mir::check_program_witnesses`]。

use std::collections::{HashMap, HashSet};

use crate::common::Span;
use crate::mir::witness::{
    BuiltinOp, MirWitness, WitnessArm, WitnessCallee, WitnessKind, WitnessParam,
};
use crate::typeck::{Type, TypeVarId};

mod builtin;
pub mod env;
pub mod error;
pub mod generalize;
mod infer;
pub(crate) mod kernel_bounds;
pub(crate) mod omega;
pub mod row;
mod stage3c;
pub mod unify;
pub mod util;

pub use error::TypeError;

use env::TypeEnv;
use unify::{Constraint, SpannedConstraint, Substitution};

/// v0.97: handle 推断帧 —— 一层 handle body 推断期间收集的 perform 位点。
#[derive(Debug)]
pub struct HandleFrame {
    pub effect: String,
    pub sites: Vec<PerformSite>,
}

/// v0.97: 一个 perform 位点的静态类型连接材料。
#[derive(Debug, Clone)]
pub struct PerformSite {
    /// perform 的结果类型（fresh var —— 与 handler 返回类型 Eq 统一）
    pub result_ty: Type,
    /// 各实参的推断类型（派生 handler 的 __arg0..N）
    pub arg_tys: Vec<Type>,
}

/// v0.98: 显式 effect 签名 —— 一个 effect 标签的静态契约。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectSignature {
    pub params: Vec<Type>,
    pub result: Type,
}

/// Wave 3 Stage 3C: Nominal struct field type & compile-time tag descriptor.
#[derive(Debug, Clone)]
pub struct StructFieldTypeDef {
    pub name: String,
    pub ty: Type,
    pub has_default: bool,
    pub tags: Vec<(String, String)>,
}

/// Wave 3 Stage 3C: Nominal generic struct type definition.
#[derive(Debug, Clone)]
pub struct StructTypeDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub const_params: Vec<String>,
    pub fields: Vec<StructFieldTypeDef>,
}

/// Wave 3 Stage 3C: Nominal generic enum type definition with payload variants.
#[derive(Debug, Clone)]
pub struct EnumTypeDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub const_params: Vec<String>,
    pub variants: Vec<(String, Vec<Type>)>,
}

/// Wave 3 Stage 3C: Trait method signature (excluding implicit receiver `self` in `params`).
#[derive(Debug, Clone)]
pub struct TraitMethodTypeSig {
    pub name: String,
    pub type_params: Vec<String>,
    pub params: Vec<(String, Type, bool)>,
    pub return_type: Type,
    pub has_self: bool,
    pub has_default: bool,
    /// Optional explicit effect row constraint (`! pure` / `! io, async`).
    pub effect_row: Option<crate::mir::effect::EffectRow>,
}

/// Wave 3 Stage 3C: Source-level trait declaration descriptor.
#[derive(Debug, Clone)]
pub struct TraitTypeDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub parents: Vec<String>,
    pub methods: Vec<TraitMethodTypeSig>,
}

/// Wave 3 Stage 3C: Compiled inherent or trait `impl` method signature (excluding `self` in `params`).
#[derive(Debug, Clone)]
pub struct ImplMethodTypeSig {
    pub name: String,
    pub type_params: Vec<String>,
    pub target_type_params: Vec<String>,
    pub params: Vec<(String, Type, bool)>,
    pub return_type: Type,
    pub has_self: bool,
    pub effect_row: crate::mir::effect::EffectRow,
}

/// Wave 3 Stage 3C: Positional/keyword/default parameter contract for a function or closure.
#[derive(Debug, Clone)]
pub struct FnParamSpec {
    pub names: Vec<String>,
    pub has_default: Vec<bool>,
    pub min_arity: usize,
}

#[derive(Default)]
pub struct HMInference {
    pub env: TypeEnv,
    pub fresh_counter: u32,
    pub constraints: Vec<SpannedConstraint>,
    /// Persistent unification substitution accumulated across solves.
    pub subst: Substitution,
    /// Diagnostics accumulated during inference (supports error recovery).
    pub errors: Vec<TypeError>,
    /// Qualified deferred constraints attached to generalized polymorphic bindings.
    pub let_schemes: HashMap<String, Vec<SpannedConstraint>>,
    /// Pre-registered `(param_types, return_type)` placeholders for `FnDef`/`MacroDef`.
    pub fn_placeholders: HashMap<String, (Vec<Type>, Type)>,
    /// Named `FnDef` / `MacroDef` / let-bound closure arities.
    pub fn_arities: HashMap<String, usize>,
    /// Stage 3C: Parameter names & default-value arity specs for named functions/closures.
    pub fn_param_specs: HashMap<String, FnParamSpec>,
    /// Stage 3C: Parameter specs for anonymous closures keyed by definition span.
    pub closure_param_specs: HashMap<Span, FnParamSpec>,
    /// Stack of in-scope closure names introduced by FnDef for recursion.
    pub fn_scope: Vec<String>,
    /// Row-polymorphic fresh var generator.
    pub fresh_vars: row::FreshVars,
    /// Handle inference frames (perform sites collected inside active handle bodies).
    pub handle_stack: Vec<HandleFrame>,
    /// Closure nesting depth (> 0 suppresses concrete handle-site recording).
    pub closure_depth: usize,
    /// Explicit and ambient effect signatures keyed by effect label.
    pub effect_signatures: HashMap<String, EffectSignature>,
    /// Precomputed effect rows for named functions/tasks.
    pub fn_effect_rows: HashMap<String, crate::mir::effect::EffectRow>,
    /// Effect rows for closures/functions keyed by definition span.
    pub closure_rows: HashMap<Span, crate::mir::effect::EffectRow>,
    /// Expected return type stack for active `FnDef`/`Closure` bodies (`return` validation).
    pub return_targets: Vec<Type>,
    /// Pre-registered user struct, enum, trait, impl, and type alias declarations.
    pub struct_defs: HashMap<String, StructTypeDef>,
    pub enum_defs: HashMap<String, EnumTypeDef>,
    pub trait_defs: HashMap<String, TraitTypeDef>,
    pub inherent_impls: HashMap<(String, String), ImplMethodTypeSig>,
    pub trait_impl_methods: HashMap<(String, String, String), ImplMethodTypeSig>,
    pub trait_impl_pairs: HashSet<(String, String)>,
    pub type_aliases: HashMap<String, Type>,
    /// Span -> raw inferred type captured during `infer_expr`.
    pub inferred_spans: HashMap<Span, Type>,
    /// Lexical bindings `(span, name, raw_type)` recorded in declaration order.
    pub recorded_bindings: Vec<(Span, String, Type)>,
    /// Stage 3B: Tensors statically known to have `requires_grad = true` in the active lexical scope.
    pub grad_tracked_tensors: HashSet<String>,
    /// Stage 3B: Tensors statically known to be zero-copy strided views (`transpose`/`reshape`/slice) in the active lexical scope.
    pub view_aliased_tensors: HashSet<String>,
    /// Stage 3B: Source tensor variables whose underlying `TensorStorage` has been aliased by a view.
    pub aliased_source_tensors: HashSet<String>,
    /// Stage 3B: Inter-procedural map `fn_name -> [param_indices]` mutated in-place via `fill_`/`add_`/`mul_`.
    pub fn_mutates_tensor_params: HashMap<String, Vec<usize>>,
    /// Stage 3B: Stack of active function parameter tables `(param_names, mutated_indices)` for tracking in-place mutations inside `FnDef`.
    pub active_fn_mut_stack: Vec<(Vec<String>, HashSet<usize>)>,
    /// Stage 3C: Active `no_grad ... end` nesting depth.
    pub no_grad_depth: usize,
    /// Static byte quotas tracked for `let pool = memory_pool(max_bytes, ...)` variables.
    pub pool_byte_quotas: HashMap<String, usize>,
    /// P6-3: Active const-generic parameter names in the current `impl`/`task` body scope.
    pub active_const_params: HashSet<String>,
    /// P6-3B: Compiled GPU kernel IRs keyed by kernel name for launch-time static shared-bounds verification.
    pub kernel_programs: HashMap<String, crate::common::kernel_ir::KernelProgram>,
}

impl HMInference {
    pub fn new() -> Self {
        Self::default()
    }

    pub(super) fn record_span_type(&mut self, span: Span, ty: &Type) {
        if span.line > 0 && !matches!(ty, Type::Error) {
            self.inferred_spans
                .entry(span)
                .or_insert_with(|| ty.clone());
        }
    }

    pub(super) fn record_binding(&mut self, span: Span, name: &str, ty: &Type) {
        if !matches!(ty, Type::Error) {
            self.recorded_bindings
                .push((span, name.to_string(), ty.clone()));
        }
    }

    /// Resolve `ty` under the solved unification substitution.
    pub fn resolve_type(&self, ty: &Type) -> Type {
        self.subst.apply(ty)
    }

    /// Resolve a builtin or module object's HM type representation (for LSP hover/completion).
    pub fn resolve_builtin_hover_type(&mut self, name: &str) -> Option<Type> {
        if name == "random" {
            return Some(Type::RandomModule);
        }
        if crate::flow::is_builtin_object(name) {
            return Some(Type::BuiltinModule(name.to_string()));
        }
        let spec = self.builtin_callee_sig(name)?;
        if spec.variadic {
            return Some(Type::Arrow(
                Box::new(Type::Any),
                Box::new(spec.return_type),
                crate::mir::effect::EffectRow::Empty,
            ));
        }
        Some(Type::curried_arrow(
            &spec.params,
            spec.return_type,
            &crate::mir::effect::EffectRow::Empty,
        ))
    }

    /// Mint a fresh HM meta-variable identifier.
    pub fn fresh_type_var_id(&mut self) -> TypeVarId {
        let id = TypeVarId::Meta(self.fresh_counter);
        self.fresh_counter += 1;
        id
    }

    /// Mint a fresh `Type::TypeVar`.
    pub fn fresh_type_var(&mut self) -> Type {
        Type::TypeVar(self.fresh_type_var_id())
    }

    /// Mint a fresh `EffectRow::Var` for row-polymorphic unification.
    pub fn fresh_row_var(&mut self) -> crate::mir::effect::EffectRow {
        crate::mir::effect::EffectRow::Var(self.fresh_vars.row_var(""))
    }

    /// Instantiate `∀α₁...αₙ. τ` into fresh variables, returning the type and var map.
    pub fn instantiate_type_with_mapping(
        &mut self,
        ty: &Type,
    ) -> (Type, HashMap<TypeVarId, TypeVarId>) {
        match ty {
            Type::ForAll(vars, inner) => {
                let quantified: HashSet<TypeVarId> = vars.iter().cloned().collect();
                let mut remap: HashMap<TypeVarId, TypeVarId> = HashMap::new();
                let inst = self.instantiate_ty(inner, &quantified, &mut remap);
                (inst, remap)
            }
            _ => (ty.clone(), HashMap::new()),
        }
    }

    /// Expand `ForAll` into fresh type variables (preserving shared var identity).
    pub fn instantiate_type(&mut self, ty: &Type) -> Type {
        self.instantiate_type_with_mapping(ty).0
    }

    /// Expand if `ForAll`, otherwise clone `ty` unchanged.
    pub fn instantiate_if_forall(&mut self, ty: &Type) -> Type {
        match ty {
            Type::ForAll(_, _) => self.instantiate_type(ty),
            _ => ty.clone(),
        }
    }

    /// Instantiate a named polymorphic binding together with any qualified `let_schemes` constraints.
    pub(crate) fn instantiate_named_binding(
        &mut self,
        name: &str,
        ty: &Type,
        use_span: Span,
    ) -> Type {
        if !matches!(ty, Type::ForAll(_, _)) {
            return ty.clone();
        }
        let (inst, mapping) = self.instantiate_type_with_mapping(ty);
        if let Some(scheme_constraints) = self.let_schemes.get(name).cloned() {
            for sc in scheme_constraints {
                self.constraints
                    .push(unify::remap_constraint(&sc, &mapping, Some(use_span)));
            }
        }
        inst
    }

    pub(super) fn instantiate_ty(
        &mut self,
        ty: &Type,
        quantified: &HashSet<TypeVarId>,
        remap: &mut HashMap<TypeVarId, TypeVarId>,
    ) -> Type {
        match ty {
            Type::TypeVar(id) => {
                if quantified.contains(id) {
                    match remap.get(id) {
                        Some(fresh) => Type::TypeVar(fresh.clone()),
                        None => {
                            let fresh = self.fresh_type_var_id();
                            remap.insert(id.clone(), fresh.clone());
                            Type::TypeVar(fresh)
                        }
                    }
                } else {
                    Type::TypeVar(id.clone())
                }
            }
            Type::List(elem) => Type::List(Box::new(self.instantiate_ty(elem, quantified, remap))),
            Type::Channel(elem) => {
                Type::Channel(Box::new(self.instantiate_ty(elem, quantified, remap)))
            }
            Type::TaskHandle(elem) => {
                Type::TaskHandle(Box::new(self.instantiate_ty(elem, quantified, remap)))
            }
            Type::Dict(k, v) => Type::Dict(
                Box::new(self.instantiate_ty(k, quantified, remap)),
                Box::new(self.instantiate_ty(v, quantified, remap)),
            ),
            Type::Result_(ok, err) => Type::Result_(
                Box::new(self.instantiate_ty(ok, quantified, remap)),
                Box::new(self.instantiate_ty(err, quantified, remap)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| Box::new(self.instantiate_ty(e, quantified, remap)))
                    .collect(),
            ),
            Type::Cons(h, t) => Type::Cons(
                Box::new(self.instantiate_ty(h, quantified, remap)),
                Box::new(self.instantiate_ty(t, quantified, remap)),
            ),
            Type::Union(members) => Type::Union(
                members
                    .iter()
                    .map(|m| self.instantiate_ty(m, quantified, remap))
                    .collect(),
            ),
            Type::Trait { name, generics } => Type::Trait {
                name: name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.instantiate_ty(g, quantified, remap))
                    .collect(),
            },
            Type::Concrete {
                name,
                generics,
                traits,
            } => Type::Concrete {
                name: name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.instantiate_ty(g, quantified, remap))
                    .collect(),
                traits: traits
                    .iter()
                    .map(|t| self.instantiate_ty(t, quantified, remap))
                    .collect(),
            },
            Type::TraitObject {
                trait_name,
                generics,
            } => Type::TraitObject {
                trait_name: trait_name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.instantiate_ty(g, quantified, remap))
                    .collect(),
            },
            Type::ForAll(inner_vars, inner) => {
                let shadowed: HashSet<TypeVarId> = inner_vars.iter().cloned().collect();
                let active: HashSet<TypeVarId> = quantified
                    .iter()
                    .filter(|c| !shadowed.contains(c))
                    .cloned()
                    .collect();
                Type::ForAll(
                    inner_vars.clone(),
                    Box::new(self.instantiate_ty(inner, &active, remap)),
                )
            }
            Type::Arrow(input, output, row) => Type::Arrow(
                Box::new(self.instantiate_ty(input, quantified, remap)),
                Box::new(self.instantiate_ty(output, quantified, remap)),
                crate::typeck::hm::row::rename_row(row, &mut self.fresh_vars),
            ),
            Type::Tensor {
                dtype,
                shape,
                device,
            } => {
                let new_dtype = Box::new(self.instantiate_ty(dtype, quantified, remap));
                let new_shape = match shape {
                    crate::typeck::ShapeType::Unranked(Some(id)) => {
                        if quantified.contains(id) {
                            let fresh = match remap.get(id) {
                                Some(f) => f.clone(),
                                None => {
                                    let f = self.fresh_type_var_id();
                                    remap.insert(id.clone(), f.clone());
                                    f
                                }
                            };
                            crate::typeck::ShapeType::Unranked(Some(fresh))
                        } else {
                            crate::typeck::ShapeType::Unranked(Some(id.clone()))
                        }
                    }
                    crate::typeck::ShapeType::Unranked(None) => {
                        crate::typeck::ShapeType::Unranked(None)
                    }
                    crate::typeck::ShapeType::Ranked(dims) => {
                        let mut new_dims = Vec::with_capacity(dims.len());
                        for d in dims {
                            let nd = d.map_leaves(&mut |leaf| match leaf {
                                crate::typeck::DimExpr::Known(n) => {
                                    crate::typeck::DimExpr::Known(*n)
                                }
                                crate::typeck::DimExpr::Var(id) => {
                                    let vid = TypeVarId::Meta(*id);
                                    if quantified.contains(&vid) {
                                        let fresh = match remap.get(&vid) {
                                            Some(f) => f.clone(),
                                            None => {
                                                let f = self.fresh_type_var_id();
                                                remap.insert(vid, f.clone());
                                                f
                                            }
                                        };
                                        match fresh {
                                            TypeVarId::Meta(nid) => {
                                                crate::typeck::DimExpr::Var(nid)
                                            }
                                            TypeVarId::Rigid(s) => {
                                                crate::typeck::DimExpr::Symbolic(s)
                                            }
                                        }
                                    } else {
                                        crate::typeck::DimExpr::Var(*id)
                                    }
                                }
                                crate::typeck::DimExpr::Symbolic(sym) => {
                                    if sym == "_" {
                                        match self.fresh_type_var_id() {
                                            TypeVarId::Meta(nid) => {
                                                crate::typeck::DimExpr::Var(nid)
                                            }
                                            TypeVarId::Rigid(s) => {
                                                crate::typeck::DimExpr::Symbolic(s)
                                            }
                                        }
                                    } else {
                                        let vid = TypeVarId::Rigid(sym.clone());
                                        if quantified.contains(&vid) {
                                            let fresh = match remap.get(&vid) {
                                                Some(f) => f.clone(),
                                                None => {
                                                    let f = self.fresh_type_var_id();
                                                    remap.insert(vid, f.clone());
                                                    f
                                                }
                                            };
                                            match fresh {
                                                TypeVarId::Meta(nid) => {
                                                    crate::typeck::DimExpr::Var(nid)
                                                }
                                                TypeVarId::Rigid(s) => {
                                                    crate::typeck::DimExpr::Symbolic(s)
                                                }
                                            }
                                        } else {
                                            crate::typeck::DimExpr::Symbolic(sym.clone())
                                        }
                                    }
                                }
                                crate::typeck::DimExpr::BinOp(..) => leaf.clone(),
                            });
                            new_dims.push(nd);
                        }
                        crate::typeck::ShapeType::Ranked(new_dims)
                    }
                };
                let new_device = match device {
                    crate::typeck::DeviceType::Var(id) => {
                        if quantified.contains(id) {
                            let fresh = match remap.get(id) {
                                Some(f) => f.clone(),
                                None => {
                                    let f = self.fresh_type_var_id();
                                    remap.insert(id.clone(), f.clone());
                                    f
                                }
                            };
                            crate::typeck::DeviceType::Var(fresh)
                        } else {
                            crate::typeck::DeviceType::Var(id.clone())
                        }
                    }
                    other => other.clone(),
                };
                Type::Tensor {
                    dtype: new_dtype,
                    shape: new_shape,
                    device: new_device,
                }
            }
            _ => ty.clone(),
        }
    }

    /// Replace `_` wildcard dimensions inside a type annotation with fresh dimension inference variables.
    pub(super) fn instantiate_wildcards(&mut self, ty: &Type) -> Type {
        match ty {
            Type::Tensor {
                dtype,
                shape,
                device,
            } => {
                let new_dtype = Box::new(self.instantiate_wildcards(dtype));
                let new_shape = match shape {
                    crate::typeck::ShapeType::Ranked(dims) => {
                        let mut out = Vec::with_capacity(dims.len());
                        for d in dims {
                            if matches!(d, crate::typeck::DimExpr::Symbolic(s) if s == "_") {
                                if let TypeVarId::Meta(nid) = self.fresh_type_var_id() {
                                    out.push(crate::typeck::DimExpr::Var(nid));
                                } else {
                                    out.push(d.clone());
                                }
                            } else {
                                out.push(d.clone());
                            }
                        }
                        crate::typeck::ShapeType::Ranked(out)
                    }
                    other => other.clone(),
                };
                Type::Tensor {
                    dtype: new_dtype,
                    shape: new_shape,
                    device: device.clone(),
                }
            }
            Type::List(e) => Type::List(Box::new(self.instantiate_wildcards(e))),
            Type::Channel(e) => Type::Channel(Box::new(self.instantiate_wildcards(e))),
            Type::TaskHandle(e) => Type::TaskHandle(Box::new(self.instantiate_wildcards(e))),
            Type::Dict(k, v) => Type::Dict(
                Box::new(self.instantiate_wildcards(k)),
                Box::new(self.instantiate_wildcards(v)),
            ),
            Type::Union(ms) => {
                Type::Union(ms.iter().map(|m| self.instantiate_wildcards(m)).collect())
            }
            other => other.clone(),
        }
    }

    /// Solve all collected constraints persistently, never wiping prior unifications on error.
    pub fn solve_constraints(&mut self) -> (Substitution, Vec<TypeError>) {
        let mut subst = self.subst.clone();
        let mut errors: Vec<TypeError> = Vec::new();
        let all: Vec<SpannedConstraint> = std::mem::take(&mut self.constraints);

        // Pass 1: Solve all non-pinning structural constraints (Eq, Subtype, RowEq).
        let mut numerics: Vec<SpannedConstraint> = Vec::new();
        for sc in all {
            match &sc.kind {
                Constraint::Numeric(_) => numerics.push(sc),
                _ => match unify::solve_spanned(&sc, &subst) {
                    Ok(new_subst) => subst = new_subst,
                    Err(err) => errors.push(err),
                },
            }
        }

        // Pass 2: Solve concrete Numeric constraints first so resolved types propagate before guessing.
        let mut deferred: Vec<SpannedConstraint> = Vec::new();
        for sc in numerics {
            if let Constraint::Numeric(ref bc) = sc.kind
                && unify::numeric_has_unbound_meta(bc, &subst)
            {
                deferred.push(sc);
                continue;
            }
            match unify::solve_spanned(&sc, &subst) {
                Ok(new_subst) => subst = new_subst,
                Err(err) => errors.push(err),
            }
        }

        // Pass 3: Solve remaining deferred Numeric constraints iteratively.
        for sc in deferred {
            match unify::solve_spanned(&sc, &subst) {
                Ok(new_subst) => subst = new_subst,
                Err(err) => errors.push(err),
            }
        }

        self.subst = subst.clone();
        (subst, errors)
    }

    pub fn infer_program(&mut self, exprs: &[MirWitness]) -> Vec<TypeError> {
        self.errors.clear();
        self.inferred_spans.clear();
        self.recorded_bindings.clear();
        self.grad_tracked_tensors.clear();
        self.view_aliased_tensors.clear();
        self.aliased_source_tensors.clear();
        self.fn_mutates_tensor_params.clear();
        self.active_fn_mut_stack.clear();
        let mut errors: Vec<TypeError> = Vec::new();
        self.seed_ambient_effect_signatures();
        self.precompute_type_defs(exprs);
        self.precompute_effect_signatures(exprs, &mut errors);
        self.precompute_fn_arities(exprs);
        self.precompute_fn_effect_rows(exprs);
        self.precompute_fn_signatures(exprs);
        for expr in exprs {
            match self.infer_expr(expr) {
                Err(mut errs) => errors.append(&mut errs),
                Ok((_, row)) => {
                    if let Some(label) = row
                        .labels()
                        .into_iter()
                        .find(|l| !crate::mir::effect::ambient::is_ambient_label(l))
                        .map(|s| s.to_string())
                    {
                        errors.push(self.localize_unhandled_effect(expr, &label));
                    }
                }
            }
        }
        errors.append(&mut self.errors);
        let (_subst, mut solve_errors) = self.solve_constraints();
        errors.append(&mut solve_errors);
        errors
    }

    fn seed_ambient_effect_signatures(&mut self) {
        use crate::mir::effect::ambient;
        let num = Type::Union(vec![Type::Int, Type::Float]);
        let entries: [(&str, Vec<Type>, Type); ambient::RANDOM_LABELS.len()] = [
            ("random_random", vec![], Type::Float),
            ("random_rand_int", vec![num.clone(), num.clone()], Type::Int),
            (
                "random_rand_float",
                vec![num.clone(), num.clone()],
                Type::Float,
            ),
            (
                "random_rand_choice",
                vec![Type::List(Box::new(Type::Any))],
                Type::Any,
            ),
            ("random_seed", vec![num], Type::Nil),
            (
                "random_shuffle",
                vec![Type::List(Box::new(Type::Any))],
                Type::List(Box::new(Type::Any)),
            ),
        ];
        for (label, params, result) in entries {
            debug_assert!(ambient::is_ambient_label(label));
            self.effect_signatures
                .entry(label.to_string())
                .or_insert(EffectSignature { params, result });
        }
    }

    /// Expand declared `type Alias = Target` definitions inside `ty` with cycle protection.
    pub(super) fn resolve_type_aliases(&self, ty: &Type) -> Type {
        let mut visiting = HashSet::new();
        self.resolve_type_aliases_inner(ty, &mut visiting)
    }

    fn resolve_type_aliases_inner(&self, ty: &Type, visiting: &mut HashSet<String>) -> Type {
        match ty {
            Type::Trait { name, generics } => {
                if generics.is_empty()
                    && let Some(target) = self.type_aliases.get(name)
                    && visiting.insert(name.clone())
                {
                    let resolved = self.resolve_type_aliases_inner(target, visiting);
                    visiting.remove(name);
                    return resolved;
                }
                let resolved_generics: Vec<Type> = generics
                    .iter()
                    .map(|g| self.resolve_type_aliases_inner(g, visiting))
                    .collect();
                if self.struct_defs.contains_key(name) || self.enum_defs.contains_key(name) {
                    return Type::Concrete {
                        name: name.clone(),
                        generics: resolved_generics,
                        traits: Vec::new(),
                    };
                }
                Type::Trait {
                    name: name.clone(),
                    generics: resolved_generics,
                }
            }
            Type::List(elem) => {
                Type::List(Box::new(self.resolve_type_aliases_inner(elem, visiting)))
            }
            Type::Channel(elem) => {
                Type::Channel(Box::new(self.resolve_type_aliases_inner(elem, visiting)))
            }
            Type::TaskHandle(elem) => {
                Type::TaskHandle(Box::new(self.resolve_type_aliases_inner(elem, visiting)))
            }
            Type::Dict(k, v) => Type::Dict(
                Box::new(self.resolve_type_aliases_inner(k, visiting)),
                Box::new(self.resolve_type_aliases_inner(v, visiting)),
            ),
            Type::Result_(ok, err) => Type::Result_(
                Box::new(self.resolve_type_aliases_inner(ok, visiting)),
                Box::new(self.resolve_type_aliases_inner(err, visiting)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| Box::new(self.resolve_type_aliases_inner(e, visiting)))
                    .collect(),
            ),
            Type::Concrete {
                name,
                generics,
                traits,
            } => Type::Concrete {
                name: name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.resolve_type_aliases_inner(g, visiting))
                    .collect(),
                traits: traits
                    .iter()
                    .map(|t| self.resolve_type_aliases_inner(t, visiting))
                    .collect(),
            },
            Type::TraitObject {
                trait_name,
                generics,
            } => Type::TraitObject {
                trait_name: trait_name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.resolve_type_aliases_inner(g, visiting))
                    .collect(),
            },
            Type::Union(members) => Type::Union(
                members
                    .iter()
                    .map(|m| self.resolve_type_aliases_inner(m, visiting))
                    .collect(),
            ),
            Type::ForAll(vars, inner) => Type::ForAll(
                vars.clone(),
                Box::new(self.resolve_type_aliases_inner(inner, visiting)),
            ),
            Type::Arrow(inp, out, row) => Type::Arrow(
                Box::new(self.resolve_type_aliases_inner(inp, visiting)),
                Box::new(self.resolve_type_aliases_inner(out, visiting)),
                row.clone(),
            ),
            Type::Cons(h, t) => Type::Cons(
                Box::new(self.resolve_type_aliases_inner(h, visiting)),
                Box::new(self.resolve_type_aliases_inner(t, visiting)),
            ),
            Type::Tensor {
                dtype,
                shape,
                device,
            } => Type::Tensor {
                dtype: Box::new(self.resolve_type_aliases_inner(dtype, visiting)),
                shape: shape.clone(),
                device: device.clone(),
            },
            other => other.clone(),
        }
    }

    /// Resolve a nominal or built-in `impl` target type header (`Point<T>`, `list<T>`, `int`, etc.).
    pub(super) fn resolve_impl_target_type(
        &self,
        target_type: &str,
        target_generics: &[crate::mir::hint::TypeHint],
    ) -> Type {
        let gen_tys: Vec<Type> = target_generics
            .iter()
            .map(|h| self.resolve_type_aliases(h.to_type()))
            .collect();
        let lower = target_type.to_lowercase();
        if let Some(dt) = crate::common::DType::from_name(&lower) {
            return Type::from_dtype(dt);
        }
        match lower.as_str() {
            "int" => Type::Int,
            "float" => Type::Float,
            "number" => Type::Union(vec![Type::Int, Type::Float]),
            "string" => Type::String,
            "char" => Type::Char,
            "bool" => Type::Bool,
            "nil" => Type::Nil,
            "context" => Type::Context,
            "tcp_listener" | "tcplistener" => Type::TcpListener,
            "tcp_stream" | "tcpstream" => Type::TcpStream,
            "tls_listener" | "tlslistener" => Type::TlsListener,
            "tls_stream" | "tlsstream" => Type::TlsStream,
            "compute_dag" | "computedag" => Type::ComputeDag,
            "kv_allocator" | "kvallocator" => Type::KVAllocator,
            "memory_pool" | "memorypool" => Type::MemoryPool,
            "process_group" | "processgroup" => Type::ProcessGroup,
            "trace_profiler" | "traceprofiler" => Type::TraceProfiler,
            "checked_path" | "checkedpath" => Type::CheckedPath,
            "gpu_kernel" | "gpukernel" => Type::GpuKernel(None),
            "list" => Type::List(Box::new(gen_tys.into_iter().next().unwrap_or(Type::Any))),
            "dict" => {
                let mut it = gen_tys.into_iter();
                let k = it.next().unwrap_or(Type::String);
                let v = it.next().unwrap_or(Type::Any);
                Type::Dict(Box::new(k), Box::new(v))
            }
            "channel" => Type::Channel(Box::new(gen_tys.into_iter().next().unwrap_or(Type::Any))),
            "task_handle" | "taskhandle" => {
                Type::TaskHandle(Box::new(gen_tys.into_iter().next().unwrap_or(Type::Any)))
            }
            "tensor" => Type::any_tensor(),
            _ => {
                if let Some(aliased) = self.type_aliases.get(target_type) {
                    return self.resolve_type_aliases(aliased);
                }
                Type::Concrete {
                    name: target_type.to_string(),
                    generics: gen_tys,
                    traits: Vec::new(),
                }
            }
        }
    }

    /// Replace `Self` references inside trait/impl method signatures with `self_ty`.
    pub(super) fn substitute_self_type(ty: &Type, self_ty: &Type) -> Type {
        match ty {
            Type::Trait { name, generics } if name == "Self" && generics.is_empty() => {
                self_ty.clone()
            }
            Type::Concrete { name, generics, .. } if name == "Self" && generics.is_empty() => {
                self_ty.clone()
            }
            Type::TypeVar(TypeVarId::Rigid(s)) if s == "Self" => self_ty.clone(),
            Type::List(e) => Type::List(Box::new(Self::substitute_self_type(e, self_ty))),
            Type::Channel(e) => Type::Channel(Box::new(Self::substitute_self_type(e, self_ty))),
            Type::TaskHandle(e) => {
                Type::TaskHandle(Box::new(Self::substitute_self_type(e, self_ty)))
            }
            Type::Dict(k, v) => Type::Dict(
                Box::new(Self::substitute_self_type(k, self_ty)),
                Box::new(Self::substitute_self_type(v, self_ty)),
            ),
            Type::Result_(ok, err) => Type::Result_(
                Box::new(Self::substitute_self_type(ok, self_ty)),
                Box::new(Self::substitute_self_type(err, self_ty)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| Box::new(Self::substitute_self_type(e, self_ty)))
                    .collect(),
            ),
            Type::Cons(h, t) => Type::Cons(
                Box::new(Self::substitute_self_type(h, self_ty)),
                Box::new(Self::substitute_self_type(t, self_ty)),
            ),
            Type::Union(ms) => Type::Union(
                ms.iter()
                    .map(|m| Self::substitute_self_type(m, self_ty))
                    .collect(),
            ),
            Type::Trait { name, generics } => Type::Trait {
                name: name.clone(),
                generics: generics
                    .iter()
                    .map(|g| Self::substitute_self_type(g, self_ty))
                    .collect(),
            },
            Type::Concrete {
                name,
                generics,
                traits,
            } => Type::Concrete {
                name: name.clone(),
                generics: generics
                    .iter()
                    .map(|g| Self::substitute_self_type(g, self_ty))
                    .collect(),
                traits: traits
                    .iter()
                    .map(|t| Self::substitute_self_type(t, self_ty))
                    .collect(),
            },
            Type::TraitObject {
                trait_name,
                generics,
            } => Type::TraitObject {
                trait_name: trait_name.clone(),
                generics: generics
                    .iter()
                    .map(|g| Self::substitute_self_type(g, self_ty))
                    .collect(),
            },
            Type::Arrow(i, o, r) => Type::Arrow(
                Box::new(Self::substitute_self_type(i, self_ty)),
                Box::new(Self::substitute_self_type(o, self_ty)),
                r.clone(),
            ),
            Type::ForAll(vars, inner) => Type::ForAll(
                vars.clone(),
                Box::new(Self::substitute_self_type(inner, self_ty)),
            ),
            Type::Tensor {
                dtype,
                shape,
                device,
            } => Type::Tensor {
                dtype: Box::new(Self::substitute_self_type(dtype, self_ty)),
                shape: shape.clone(),
                device: device.clone(),
            },
            other => other.clone(),
        }
    }

    fn precompute_type_defs(&mut self, exprs: &[MirWitness]) {
        // P4-1A: Pre-register built-in operator traits (`Add`/`Sub`/`Mul`/`Div`/`Mod`/`Pow`/`MatMul`/`Eq`/`Neg`)
        // so `impl Add for Vec2` works out of the box while allowing explicit `trait Add` overrides.
        let builtin_bin_ops = [
            ("Add", "add", Type::Any),
            ("Sub", "sub", Type::Any),
            ("Mul", "mul", Type::Any),
            ("Div", "div", Type::Any),
            ("Mod", "rem", Type::Any),
            ("Pow", "pow", Type::Any),
            ("MatMul", "matmul", Type::Any),
            ("Eq", "eq", Type::Bool),
        ];
        for (tr_name, m_name, ret_ty) in builtin_bin_ops {
            self.trait_defs
                .entry(tr_name.to_string())
                .or_insert_with(|| TraitTypeDef {
                    name: tr_name.to_string(),
                    type_params: Vec::new(),
                    parents: Vec::new(),
                    methods: vec![TraitMethodTypeSig {
                        name: m_name.to_string(),
                        type_params: Vec::new(),
                        params: vec![("rhs".to_string(), Type::Any, false)],
                        return_type: ret_ty,
                        has_self: true,
                        has_default: false,
                        effect_row: None,
                    }],
                });
        }
        self.trait_defs
            .entry("Neg".to_string())
            .or_insert_with(|| TraitTypeDef {
                name: "Neg".to_string(),
                type_params: Vec::new(),
                parents: Vec::new(),
                methods: vec![TraitMethodTypeSig {
                    name: "neg".to_string(),
                    type_params: Vec::new(),
                    params: Vec::new(),
                    return_type: Type::Any,
                    has_self: true,
                    has_default: false,
                    effect_row: None,
                }],
            });

        MirWitness::visit_preorder(exprs, |w| match &w.kind {
            WitnessKind::StructDef {
                name,
                type_params,
                const_params,
                fields,
            } => {
                let fs = fields
                    .iter()
                    .map(|f| StructFieldTypeDef {
                        name: f.name.clone(),
                        ty: f.type_hint.to_type().clone(),
                        has_default: f.default.is_some(),
                        tags: f.tags.clone(),
                    })
                    .collect();
                self.struct_defs.insert(
                    name.clone(),
                    StructTypeDef {
                        name: name.clone(),
                        type_params: type_params.clone(),
                        const_params: const_params.clone(),
                        fields: fs,
                    },
                );
            }
            WitnessKind::EnumDef {
                name,
                type_params,
                const_params,
                variants,
            } => {
                let vs = variants
                    .iter()
                    .map(|v| {
                        (
                            v.name.clone(),
                            v.payload_types
                                .iter()
                                .map(|h| h.to_type().clone())
                                .collect(),
                        )
                    })
                    .collect();
                self.enum_defs.insert(
                    name.clone(),
                    EnumTypeDef {
                        name: name.clone(),
                        type_params: type_params.clone(),
                        const_params: const_params.clone(),
                        variants: vs,
                    },
                );
            }
            WitnessKind::KernelDef { name, params, .. } => {
                let sig = params.iter().map(|p| p.ty).collect();
                self.env.add(name.clone(), Type::GpuKernel(Some(sig)));
            }
            WitnessKind::TypeAlias { name, target } => {
                self.type_aliases
                    .insert(name.clone(), target.to_type().clone());
            }
            WitnessKind::TraitDef {
                name,
                type_params,
                parents,
                methods,
            } => {
                let ms = methods
                    .iter()
                    .map(|m| {
                        let skip = usize::from(m.has_self);
                        let params = m
                            .params
                            .iter()
                            .skip(skip)
                            .map(|p| {
                                let pty = p
                                    .type_hint
                                    .as_ref()
                                    .map(|h| h.to_type().clone())
                                    .unwrap_or(Type::Any);
                                (p.name.clone(), pty, p.default.is_some())
                            })
                            .collect();
                        let ret = m
                            .return_type
                            .as_ref()
                            .map(|h| h.to_type().clone())
                            .unwrap_or(Type::Any);
                        TraitMethodTypeSig {
                            name: m.name.clone(),
                            type_params: m.type_params.clone(),
                            params,
                            return_type: ret,
                            has_self: m.has_self,
                            has_default: m.default_body.is_some(),
                            effect_row: m.effect_annotation.clone(),
                        }
                    })
                    .collect();
                self.trait_defs.insert(
                    name.clone(),
                    TraitTypeDef {
                        name: name.clone(),
                        type_params: type_params.clone(),
                        parents: parents.clone(),
                        methods: ms,
                    },
                );
            }
            _ => {}
        });

        let raw_aliases = self.type_aliases.clone();
        for (name, target) in raw_aliases {
            let resolved = self.resolve_type_aliases(&target);
            self.type_aliases.insert(name, resolved);
        }
        let raw_structs = self.struct_defs.clone();
        for (name, mut def) in raw_structs {
            for f in &mut def.fields {
                f.ty = self.resolve_type_aliases(&f.ty);
            }
            self.struct_defs.insert(name, def);
        }
        let raw_enums = self.enum_defs.clone();
        for (name, mut def) in raw_enums {
            for (_, payload_tys) in &mut def.variants {
                for pty in payload_tys.iter_mut() {
                    *pty = self.resolve_type_aliases(pty);
                }
            }
            self.enum_defs.insert(name, def);
        }
        let raw_traits = self.trait_defs.clone();
        for (name, mut def) in raw_traits {
            for m in &mut def.methods {
                for (_, pty, _) in &mut m.params {
                    *pty = self.resolve_type_aliases(pty);
                }
                m.return_type = self.resolve_type_aliases(&m.return_type);
            }
            self.trait_defs.insert(name, def);
        }

        // Second pass: register inherent and trait `impl` method tables with resolved aliases & `Self`.
        let mut impl_nodes: Vec<&MirWitness> = Vec::new();
        MirWitness::visit_preorder(exprs, |w| {
            if matches!(&w.kind, WitnessKind::ImplDef { .. }) {
                impl_nodes.push(w);
            }
        });
        for w in impl_nodes {
            if let WitnessKind::ImplDef {
                type_params,
                trait_name,
                target_type,
                target_generics,
                methods,
                ..
            } = &w.kind
            {
                let self_ty = self.resolve_impl_target_type(target_type, target_generics);
                let target_tp: Vec<String> = self
                    .struct_defs
                    .get(target_type)
                    .map(|d| d.type_params.clone())
                    .or_else(|| {
                        self.enum_defs
                            .get(target_type)
                            .map(|d| d.type_params.clone())
                    })
                    .unwrap_or_else(|| type_params.clone());
                if let Some(tr) = trait_name {
                    self.trait_impl_pairs
                        .insert((tr.clone(), target_type.clone()));
                }
                for m in methods {
                    let skip = usize::from(m.has_self);
                    let params: Vec<(String, Type, bool)> = m
                        .params
                        .iter()
                        .skip(skip)
                        .map(|p| {
                            let raw = p
                                .type_hint
                                .as_ref()
                                .map(|h| self.resolve_type_aliases(h.to_type()))
                                .unwrap_or_else(|| self.fresh_type_var());
                            let subbed = Self::substitute_self_type(&raw, &self_ty);
                            (p.name.clone(), subbed, p.default.is_some())
                        })
                        .collect();
                    let ret_raw = m
                        .return_type
                        .as_ref()
                        .map(|h| self.resolve_type_aliases(h.to_type()))
                        .unwrap_or_else(|| self.fresh_type_var());
                    let ret = Self::substitute_self_type(&ret_raw, &self_ty);
                    let mut ambient = HashSet::new();
                    let body_tree_row =
                        Self::tree_effect_row(&m.body, &mut ambient, &self.fn_effect_rows);
                    let effect_row = match &m.effect_annotation {
                        Some(decl) if !decl.is_open() => decl.clone(),
                        Some(decl) => Self::union_effect_rows(decl, &body_tree_row),
                        None => body_tree_row,
                    };
                    let mut all_tps = type_params.clone();
                    for tp in &m.type_params {
                        if !all_tps.contains(tp) {
                            all_tps.push(tp.clone());
                        }
                    }
                    let sig = ImplMethodTypeSig {
                        name: m.name.clone(),
                        type_params: all_tps,
                        target_type_params: target_tp.clone(),
                        params,
                        return_type: ret,
                        has_self: m.has_self,
                        effect_row,
                    };
                    if let Some(tr) = trait_name {
                        self.trait_impl_methods
                            .insert((tr.clone(), target_type.clone(), m.name.clone()), sig);
                    } else {
                        self.inherent_impls
                            .insert((target_type.clone(), m.name.clone()), sig);
                    }
                }
            }
        }
    }

    fn precompute_effect_signatures(&mut self, exprs: &[MirWitness], errors: &mut Vec<TypeError>) {
        MirWitness::visit_preorder(exprs, |w| {
            if let WitnessKind::EffectSig {
                name,
                params,
                result,
            } = &w.kind
            {
                let sig = EffectSignature {
                    params: params
                        .iter()
                        .map(|h| self.resolve_type_aliases(h.to_type()))
                        .collect(),
                    result: result
                        .as_ref()
                        .map(|h| self.resolve_type_aliases(h.to_type()))
                        .unwrap_or(Type::Any),
                };
                match self.effect_signatures.get(name) {
                    Some(prev) if prev != &sig => {
                        errors.push(TypeError::UnificationFailure {
                            expected: format!("effect `{}` signature {:?}", name, prev),
                            got: format!("{:?}", sig),
                            span: Some(w.span),
                        });
                    }
                    _ => {
                        self.effect_signatures.insert(name.clone(), sig);
                    }
                }
            }
        });
    }

    pub fn precompute_fn_arities(&mut self, exprs: &[MirWitness]) {
        MirWitness::visit_preorder(exprs, |def| match &def.kind {
            WitnessKind::FnDef { name, params, .. } => {
                self.fn_arities.entry(name.clone()).or_insert(params.len());
                let min_arity = params.iter().filter(|p| p.default.is_none()).count();
                self.fn_param_specs
                    .entry(name.clone())
                    .or_insert(FnParamSpec {
                        names: params.iter().map(|p| p.name.clone()).collect(),
                        has_default: params.iter().map(|p| p.default.is_some()).collect(),
                        min_arity,
                    });
            }
            WitnessKind::MacroDef { name, params, .. } => {
                self.fn_arities.entry(name.clone()).or_insert(params.len());
            }
            _ => {}
        });
    }

    /// Pre-register EVERY `FnDef` and `MacroDef` into `env` and `fn_placeholders`
    /// (both annotated and unannotated) so recursion, forward calls, and
    /// undefined-function rejection work uniformly.
    pub fn precompute_fn_signatures(&mut self, exprs: &[MirWitness]) {
        let mut defs: Vec<&MirWitness> = Vec::new();
        MirWitness::visit_preorder(exprs, |w| {
            if matches!(
                &w.kind,
                WitnessKind::FnDef { .. } | WitnessKind::MacroDef { .. }
            ) {
                defs.push(w);
            }
        });
        for def in defs {
            match &def.kind {
                WitnessKind::FnDef {
                    name,
                    type_params,
                    params,
                    return_type,
                    effect_annotation,
                    ..
                } => {
                    if self.env.contains(name) {
                        continue;
                    }
                    let param_types: Vec<Type> = params
                        .iter()
                        .map(|p| {
                            p.type_hint
                                .as_ref()
                                .map(|h| self.resolve_type_aliases(h.to_type()))
                                .unwrap_or_else(|| self.fresh_type_var())
                        })
                        .collect();
                    let ret = return_type
                        .as_ref()
                        .map(|h| self.resolve_type_aliases(h.to_type()))
                        .unwrap_or_else(|| self.fresh_type_var());
                    let eff_row = effect_annotation
                        .clone()
                        .or_else(|| self.fn_effect_rows.get(name).cloned())
                        .unwrap_or(crate::mir::effect::EffectRow::Empty);
                    self.fn_placeholders
                        .insert(name.clone(), (param_types.clone(), ret.clone()));
                    let ty = Type::curried_arrow(&param_types, ret, &eff_row);
                    let mut rigids: Vec<TypeVarId> = type_params
                        .iter()
                        .map(|p| TypeVarId::Rigid(p.clone()))
                        .collect();
                    for v in env::collect_type_vars(&ty) {
                        if matches!(v, TypeVarId::Rigid(_)) && !rigids.contains(&v) {
                            rigids.push(v);
                        }
                    }
                    let ty = if rigids.is_empty() {
                        ty
                    } else {
                        Type::ForAll(rigids, Box::new(ty))
                    };
                    self.env.add(name.clone(), ty);
                }
                WitnessKind::MacroDef { name, params, .. } => {
                    if self.env.contains(name) {
                        continue;
                    }
                    let param_types: Vec<Type> =
                        (0..params.len()).map(|_| self.fresh_type_var()).collect();
                    let ret = self.fresh_type_var();
                    self.fn_placeholders
                        .insert(name.clone(), (param_types.clone(), ret.clone()));
                    let ty = Type::curried_arrow(
                        &param_types,
                        ret,
                        &crate::mir::effect::EffectRow::Empty,
                    );
                    self.env.add(name.clone(), ty);
                }
                _ => {}
            }
        }
    }

    /// v0.97: fn/task 定义效果行的不动点预计算。
    ///
    /// 顺序推断的限制：`task a() { b() }` 在 `task b()` 之前定义时，推断
    /// 到 a 的 body 时 `fn_effect_rows` 还没有 b 的行 —— mutual recursion
    /// 与前向引用的效果静默漏检。本方法用**树行走器**（不跑完整 HM，纯
    /// 数据操作）把全部定义的效果行迭代到不动点后并入登记表：
    /// - 行单调增长（标签只增不减），标签集来自树内有限的 Perform 节点，
    ///   迭代必然终止（另设 64 轮防御上限）；
    /// - 定义节点本身是纯的（行在调用时发生），但全部定义（含嵌套）都被
    ///   收集并各自计算行 —— 前向引用由此可见。
    pub fn precompute_fn_effect_rows(&mut self, exprs: &[MirWitness]) {
        let mut defs: Vec<&MirWitness> = Vec::new();
        MirWitness::visit_preorder(exprs, |w| {
            if matches!(&w.kind, WitnessKind::FnDef { .. }) {
                defs.push(w);
            }
        });
        if defs.is_empty() {
            return;
        }
        let mut table: HashMap<String, crate::mir::effect::EffectRow> = self.fn_effect_rows.clone();
        for round in 0..64u32 {
            let mut changed = false;
            for def in &defs {
                if let WitnessKind::FnDef {
                    name,
                    body,
                    effect_annotation,
                    ..
                } = &def.kind
                {
                    let mut ambient = std::collections::HashSet::new();
                    let body_row = Self::tree_effect_row(body, &mut ambient, &table);
                    let row = match effect_annotation {
                        Some(decl) if !decl.is_open() => decl.clone(),
                        Some(decl) => Self::union_effect_rows(decl, &body_row),
                        None => body_row,
                    };
                    let merged = match table.get(name) {
                        Some(prev) => Self::union_effect_rows(prev, &row),
                        None => row,
                    };
                    if table.get(name) != Some(&merged) {
                        table.insert(name.clone(), merged);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
            let _ = round;
        }
        for (name, row) in table {
            self.fn_effect_rows.insert(name, row);
        }
    }

    /// v0.97: witness 树的效果行直接计算（与 HM 推断同行代数的数据形态）。
    ///
    /// - Perform：环境已处理 → 无贡献；否则产生标签（实参行并入）。
    /// - Handle：body 行去掉被捕获标签（吸收）∪ handler 行。
    /// - Call：被调效果行（登记表 / 立即调用闭包内联 body 行）∪ 实参行。
    /// - Closure/FnDef/UpdateDef 定义节点：纯（行被捕获，不并入外层）。
    /// - LetBinding 绑定闭包/函数：绑定处纯；其他值行上浮。
    fn tree_effect_row(
        root: &MirWitness,
        ambient: &mut std::collections::HashSet<String>,
        table: &HashMap<String, crate::mir::effect::EffectRow>,
    ) -> crate::mir::effect::EffectRow {
        use crate::mir::effect::EffectRow;

        enum Task<'w> {
            Visit(&'w MirWitness),
            FinishHandle {
                saved_row: EffectRow,
                effect: &'w str,
                handler: &'w MirWitness,
                was_present: bool,
            },
        }

        let mut stack = vec![Task::Visit(root)];
        let mut cur_row = EffectRow::Empty;

        while let Some(task) = stack.pop() {
            match task {
                Task::FinishHandle {
                    saved_row,
                    effect,
                    handler,
                    was_present,
                } => {
                    let b = cur_row.remove(effect);
                    if !was_present {
                        ambient.remove(effect);
                    }
                    cur_row = Self::union_effect_rows(&saved_row, &b);
                    stack.push(Task::Visit(handler));
                }
                Task::Visit(w) => match &w.kind {
                    WitnessKind::Perform { effect, args } => {
                        if !ambient.contains(effect) {
                            let label_row =
                                EffectRow::Cons(effect.clone(), Box::new(EffectRow::Empty));
                            cur_row = Self::union_effect_rows(&cur_row, &label_row);
                        }
                        for a in args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                    }
                    WitnessKind::Handle {
                        effect,
                        body,
                        handler,
                        ..
                    } => {
                        let was_present = !ambient.insert(effect.clone());
                        let saved_row = std::mem::replace(&mut cur_row, EffectRow::Empty);
                        stack.push(Task::FinishHandle {
                            saved_row,
                            effect,
                            handler,
                            was_present,
                        });
                        stack.push(Task::Visit(body));
                    }
                    WitnessKind::Call {
                        callee,
                        args,
                        named_args,
                    } => {
                        for (_, a) in named_args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                        for a in args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                        match callee {
                            WitnessCallee::Var(n)
                            | WitnessCallee::Name(n)
                            | WitnessCallee::GenericName { name: n, .. } => {
                                if let Some(r) = table.get(n) {
                                    cur_row = Self::union_effect_rows(&cur_row, r);
                                }
                            }
                            WitnessCallee::Evaluated(e) => match &e.kind {
                                WitnessKind::Closure { body, .. } => {
                                    stack.push(Task::Visit(body));
                                }
                                _ => stack.push(Task::Visit(e)),
                            },
                            WitnessCallee::Builtin(_) | WitnessCallee::Method(_, _) => {}
                        }
                    }
                    WitnessKind::Closure { .. } | WitnessKind::FnDef { .. } => {}
                    WitnessKind::LetBinding {
                        value, init_body, ..
                    } => {
                        stack.push(Task::Visit(init_body));
                        if !matches!(
                            &value.kind,
                            WitnessKind::Closure { .. } | WitnessKind::FnDef { .. }
                        ) {
                            stack.push(Task::Visit(value));
                        }
                    }
                    WitnessKind::MethodCall {
                        receiver,
                        method,
                        args,
                        named_args,
                    } => {
                        for (_, a) in named_args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                        for a in args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                        if matches!(method.as_str(), "fill_" | "add_" | "mul_") {
                            let r = EffectRow::Cons(
                                crate::mir::effect::ambient::TENSOR_MUTATE_LABEL.to_string(),
                                Box::new(EffectRow::Empty),
                            );
                            cur_row = Self::union_effect_rows(&cur_row, &r);
                        }
                        match &receiver.kind {
                            WitnessKind::Variable(n) if n == "random" => {
                                if let Some(l) =
                                    crate::mir::effect::ambient::random_label_for_method(method)
                                {
                                    let r =
                                        EffectRow::Cons(l.to_string(), Box::new(EffectRow::Empty));
                                    cur_row = Self::union_effect_rows(&cur_row, &r);
                                }
                            }
                            _ => stack.push(Task::Visit(receiver)),
                        }
                    }
                    _ => {
                        for c in w.child_witnesses().into_iter().rev() {
                            stack.push(Task::Visit(c));
                        }
                    }
                },
            }
        }
        cur_row
    }

    /// v0.97: 效果行并集 —— 具名标签的集合并（保序去重）；Var（多态未知
    /// 行）不贡献具体标签，仅在全空时保留未知性。
    fn union_effect_rows(
        a: &crate::mir::effect::EffectRow,
        b: &crate::mir::effect::EffectRow,
    ) -> crate::mir::effect::EffectRow {
        let mut labels: Vec<String> = a.labels().into_iter().map(String::from).collect();
        for l in b.labels() {
            if !labels.iter().any(|x| x == l) {
                labels.push(l.to_string());
            }
        }
        let mut row = crate::mir::effect::EffectRow::Empty;
        for l in labels {
            row.extend(&l);
        }
        if matches!(row, crate::mir::effect::EffectRow::Empty)
            && (matches!(a, crate::mir::effect::EffectRow::Var(_))
                || matches!(b, crate::mir::effect::EffectRow::Var(_)))
        {
            row = crate::mir::effect::EffectRow::Var("rho".to_string());
        }
        row
    }

    /// v0.96: 把顶层残差行中的未处理效果定位到发起 witness（精准 span）。
    fn localize_unhandled_effect(&self, w: &MirWitness, label: &str) -> TypeError {
        let mut ambient = std::collections::HashSet::new();
        if let Some((span, via)) = self.find_unhandled(w, label, &mut ambient) {
            TypeError::EffectRowMismatch {
                expected: "no unhandled effects — wrap in a matching `handle` block".to_string(),
                got: match via {
                    Some(callee) => format!("{{ {label} }} (perform inside `{callee}`)"),
                    None => format!("{{ {label} }}"),
                },
                span: Some(span),
            }
        } else {
            TypeError::EffectRowMismatch {
                expected: "no unhandled effects".to_string(),
                got: format!("{{ {label} }}"),
                span: Some(w.span),
            }
        }
    }

    /// v0.96: 在 witness 树内找第一个发起未处理 `label` 效果的位置。
    ///
    /// - Handle 扩展环境已处理集（body 内的 perform 被它吸收）；
    /// - Closure/FnDef/UpdateDef 体是独立效果上下文（行已捕获进 Arrow /
    ///   `fn_effect_rows`）—— 不下潜，否则会用外层环境误判定义体内的
    ///   perform（这正是「词法检查错误拒绝合法程序」的陷阱）；
    /// - Call 到已登记效果行的 callee：行含未处理标签 → 报在调用点。
    fn find_unhandled(
        &self,
        root: &MirWitness,
        label: &str,
        ambient: &mut std::collections::HashSet<String>,
    ) -> Option<(Span, Option<String>)> {
        enum Task<'w> {
            Visit(&'w MirWitness),
            PopAmbient(&'w str, bool),
            CheckCall(Span, &'w str),
        }

        let mut stack = vec![Task::Visit(root)];
        while let Some(task) = stack.pop() {
            match task {
                Task::PopAmbient(effect, was_present) => {
                    if !was_present {
                        ambient.remove(effect);
                    }
                }
                Task::CheckCall(span, name) => {
                    if let Some(row) = self.fn_effect_rows.get(name)
                        && row.labels().contains(&label)
                        && !ambient.contains(label)
                    {
                        return Some((span, Some(name.to_string())));
                    }
                }
                Task::Visit(w) => match &w.kind {
                    WitnessKind::Perform { effect, args } => {
                        if effect == label && !ambient.contains(effect) {
                            return Some((w.span, None));
                        }
                        for a in args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                    }
                    WitnessKind::Handle {
                        effect,
                        body,
                        handler,
                        ..
                    } => {
                        let was_present = !ambient.insert(effect.clone());
                        stack.push(Task::Visit(handler));
                        stack.push(Task::PopAmbient(effect, was_present));
                        stack.push(Task::Visit(body));
                    }
                    WitnessKind::Call {
                        callee,
                        args,
                        named_args,
                    } => {
                        if let WitnessCallee::Var(name)
                        | WitnessCallee::Name(name)
                        | WitnessCallee::GenericName { name, .. } = callee
                        {
                            stack.push(Task::CheckCall(w.span, name));
                        }
                        for (_, a) in named_args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                        for a in args.iter().rev() {
                            stack.push(Task::Visit(a));
                        }
                    }
                    WitnessKind::Closure { .. } | WitnessKind::FnDef { .. } => {}
                    _ => {
                        for c in w.child_witnesses().into_iter().rev() {
                            stack.push(Task::Visit(c));
                        }
                    }
                },
            }
        }
        None
    }

    /// v0.80: 推断表达式类型 + effect row。
    ///
    /// 每个表达式产生 `(Type, EffectRow)` — 类型 + 该表达式可能产生的
    /// side effect 集合。EffectRow::Empty 表示纯表达式。
    pub fn infer_expr(
        &mut self,
        expr: &MirWitness,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let res = if matches!(
            &expr.kind,
            WitnessKind::Binary { .. }
                | WitnessKind::Or { .. }
                | WitnessKind::And { .. }
                | WitnessKind::Pipe { .. }
                | WitnessKind::MethodCall { .. }
        ) || matches!(
            &expr.kind,
            WitnessKind::Call {
                callee: WitnessCallee::Builtin(
                    crate::mir::witness::BuiltinOp::Neg | crate::mir::witness::BuiltinOp::Not
                ),
                args,
                named_args,
            } if args.len() == 1 && named_args.is_empty()
        ) {
            self.infer_infix_chain(expr)?
        } else {
            self.infer_expr_single(expr)?
        };
        self.record_span_type(expr.span, &res.0);
        Ok(res)
    }

    /// W2.3: 显式堆栈迭代推断二元算术/逻辑/管道/方法调用链，消除深链爆栈及 Pipe 克隆。
    fn infer_infix_chain(
        &mut self,
        root: &MirWitness,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::common::BinaryOp;
        use crate::mir::effect::EffectRow;

        enum ChainTask<'w> {
            Eval(&'w MirWitness),
            ApplyUnaryNeg {
                span: Span,
            },
            ApplyUnaryNot {
                arg_span: Span,
                span: Span,
            },
            ApplyBinary {
                op: &'w BinaryOp,
                span: Span,
            },
            ApplyLogic {
                left_span: Span,
                right_span: Span,
                span: Span,
            },
            ApplyPipeNamed {
                name: &'w str,
                callee_ty: Type,
                callee_row: EffectRow,
                builtin_sig: Option<builtin::BuiltinCallSpec>,
                arg_span: Span,
                span: Span,
            },
            ApplyPipeExpr {
                arg_span: Span,
                span: Span,
            },
            ApplyMethodCall {
                receiver: &'w MirWitness,
                recv_module_name: Option<&'w str>,
                method: &'w str,
                args: &'w [MirWitness],
                named_args: &'w [(String, MirWitness)],
                span: Span,
            },
        }

        let mut tasks = vec![ChainTask::Eval(root)];
        let mut vals: Vec<(Type, EffectRow)> = Vec::new();

        while let Some(task) = tasks.pop() {
            match task {
                ChainTask::Eval(w) => match &w.kind {
                    WitnessKind::Call {
                        callee: WitnessCallee::Builtin(crate::mir::witness::BuiltinOp::Neg),
                        args,
                        named_args,
                    } if args.len() == 1 && named_args.is_empty() => {
                        tasks.push(ChainTask::ApplyUnaryNeg { span: w.span });
                        tasks.push(ChainTask::Eval(&args[0]));
                    }
                    WitnessKind::Call {
                        callee: WitnessCallee::Builtin(crate::mir::witness::BuiltinOp::Not),
                        args,
                        named_args,
                    } if args.len() == 1 && named_args.is_empty() => {
                        tasks.push(ChainTask::ApplyUnaryNot {
                            arg_span: args[0].span,
                            span: w.span,
                        });
                        tasks.push(ChainTask::Eval(&args[0]));
                    }
                    WitnessKind::Binary { left, op, right } => {
                        tasks.push(ChainTask::ApplyBinary { op, span: w.span });
                        tasks.push(ChainTask::Eval(right));
                        tasks.push(ChainTask::Eval(left));
                    }
                    WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
                        tasks.push(ChainTask::ApplyLogic {
                            left_span: left.span,
                            right_span: right.span,
                            span: w.span,
                        });
                        tasks.push(ChainTask::Eval(right));
                        tasks.push(ChainTask::Eval(left));
                    }
                    WitnessKind::Pipe { left, right } => match &right.kind {
                        WitnessKind::Variable(name) => {
                            let (callee_ty, callee_row, builtin_sig) =
                                self.resolve_named_callee_header(name, w.span)?;
                            self.record_span_type(right.span, &callee_ty);
                            self.check_call_arity(
                                &WitnessCallee::Name(name.clone()),
                                &callee_ty,
                                builtin_sig.as_ref(),
                                1,
                                w.span,
                            )?;
                            tasks.push(ChainTask::ApplyPipeNamed {
                                name,
                                callee_ty,
                                callee_row,
                                builtin_sig,
                                arg_span: left.span,
                                span: w.span,
                            });
                            tasks.push(ChainTask::Eval(left));
                        }
                        _ => {
                            tasks.push(ChainTask::ApplyPipeExpr {
                                arg_span: left.span,
                                span: w.span,
                            });
                            tasks.push(ChainTask::Eval(right));
                            tasks.push(ChainTask::Eval(left));
                        }
                    },
                    WitnessKind::MethodCall {
                        receiver,
                        method,
                        args,
                        named_args,
                    } => {
                        let recv_module_name = match &receiver.kind {
                            WitnessKind::Variable(module) => Some(module.as_str()),
                            _ => None,
                        };
                        tasks.push(ChainTask::ApplyMethodCall {
                            receiver,
                            recv_module_name,
                            method,
                            args,
                            named_args,
                            span: w.span,
                        });
                        tasks.push(ChainTask::Eval(receiver));
                    }
                    _ => {
                        let res = self.infer_expr_single(w)?;
                        self.record_span_type(w.span, &res.0);
                        vals.push(res);
                    }
                },
                ChainTask::ApplyUnaryNeg { span } => {
                    let (arg_ty, arg_row) = vals.pop().expect("unary neg arg present");
                    let resolved = self.subst.apply(&arg_ty);
                    if let Some(res) = self.try_infer_unary_op_overload(
                        &resolved,
                        "Neg",
                        "neg",
                        arg_row.clone(),
                        span,
                    ) {
                        let (out_ty, out_row) = res?;
                        self.record_span_type(span, &out_ty);
                        vals.push((out_ty, out_row));
                        continue;
                    }
                    let out_ty = match &resolved {
                        Type::Error => Type::Error,
                        Type::Int | Type::Float | Type::Any | Type::TypeVar(_) => resolved,
                        Type::Scalar(dt) => {
                            if dt.is_unsigned_int() || dt.is_bool() {
                                return Err(vec![TypeError::UnificationFailure {
                                    expected: "signed numeric type for unary '-'".to_string(),
                                    got: resolved.name(),
                                    span: Some(span),
                                }]);
                            }
                            resolved
                        }
                        Type::Tensor { dtype, .. } => {
                            let dt_resolved = self.subst.apply(dtype);
                            if let Some(dt) = dt_resolved.as_dtype()
                                && (dt.is_unsigned_int() || dt.is_bool())
                            {
                                return Err(vec![TypeError::UnificationFailure {
                                    expected: "signed numeric Tensor DType for unary '-'"
                                        .to_string(),
                                    got: resolved.name(),
                                    span: Some(span),
                                }]);
                            }
                            resolved
                        }
                        _ => {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: "numeric type or Tensor for unary '-'".to_string(),
                                got: resolved.name(),
                                span: Some(span),
                            }]);
                        }
                    };
                    self.record_span_type(span, &out_ty);
                    vals.push((out_ty, arg_row));
                }
                ChainTask::ApplyUnaryNot { arg_span, span } => {
                    let (arg_ty, arg_row) = vals.pop().expect("unary not arg present");
                    if !matches!(arg_ty, Type::Error) {
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(arg_ty),
                                sup: Box::new(Type::Bool),
                            },
                            arg_span,
                        ));
                    }
                    self.record_span_type(span, &Type::Bool);
                    vals.push((Type::Bool, arg_row));
                }
                ChainTask::ApplyBinary { op, span } => {
                    let (right_ty, right_row) = vals.pop().expect("rhs value present");
                    let (left_ty, left_row) = vals.pop().expect("lhs value present");
                    let res =
                        self.apply_binop_types(op, left_ty, left_row, right_ty, right_row, span)?;
                    self.record_span_type(span, &res.0);
                    vals.push(res);
                }
                ChainTask::ApplyLogic {
                    left_span,
                    right_span,
                    span,
                } => {
                    let (right_ty, right_row) = vals.pop().expect("logic rhs present");
                    let (left_ty, left_row) = vals.pop().expect("logic lhs present");
                    let merged = self.merge_rows(left_row, right_row, left_span);
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(left_ty),
                            sup: Box::new(Type::Bool),
                        },
                        left_span,
                    ));
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(right_ty),
                            sup: Box::new(Type::Bool),
                        },
                        right_span,
                    ));
                    self.record_span_type(span, &Type::Bool);
                    vals.push((Type::Bool, merged));
                }
                ChainTask::ApplyPipeNamed {
                    name,
                    callee_ty,
                    callee_row,
                    builtin_sig,
                    arg_span,
                    span,
                } => {
                    let (arg_ty, arg_row) = vals.pop().expect("pipe arg present");
                    let res = if let Some(spec) = builtin_sig {
                        self.apply_builtin_call_types(
                            &spec,
                            vec![(arg_ty, arg_row, arg_span)],
                            span,
                        )
                    } else {
                        let _ = name;
                        self.apply_curried_call_types(
                            callee_ty,
                            callee_row,
                            vec![(arg_ty, arg_row, arg_span)],
                            span,
                        )
                    };
                    self.record_span_type(span, &res.0);
                    vals.push(res);
                }
                ChainTask::ApplyPipeExpr { arg_span, span } => {
                    let (callee_ty, callee_row) = vals.pop().expect("pipe rhs callee present");
                    let (left_ty, left_row) = vals.pop().expect("pipe lhs arg present");
                    let res = self.apply_curried_call_types(
                        callee_ty,
                        callee_row,
                        vec![(left_ty, left_row, arg_span)],
                        span,
                    );
                    self.record_span_type(span, &res.0);
                    vals.push(res);
                }
                ChainTask::ApplyMethodCall {
                    receiver,
                    recv_module_name,
                    method,
                    args,
                    named_args,
                    span,
                } => {
                    let (recv_ty, recv_row) = vals.pop().expect("method receiver present");
                    let res = self.infer_method_call_with_recv(
                        Some(receiver),
                        recv_ty,
                        recv_row,
                        recv_module_name,
                        method,
                        args,
                        named_args,
                        span,
                    )?;
                    self.record_span_type(span, &res.0);
                    vals.push(res);
                }
            }
        }

        Ok(vals.pop().expect("chain root value present"))
    }

    pub(super) fn infer_recover(
        &mut self,
        expr: &MirWitness,
    ) -> (Type, crate::mir::effect::EffectRow) {
        match self.infer_expr(expr) {
            Ok(res) => res,
            Err(errs) => {
                self.errors.extend(errs);
                (Type::Error, crate::mir::effect::EffectRow::Empty)
            }
        }
    }

    fn infer_expr_single(
        &mut self,
        expr: &MirWitness,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        match &expr.kind {
            WitnessKind::Literal(lit) => Ok((infer_lit(lit), crate::mir::effect::EffectRow::Empty)),
            WitnessKind::Variable(name) => {
                let ty = self.infer_var(name, expr.span)?;
                Ok((ty, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::Binary { .. }
            | WitnessKind::Or { .. }
            | WitnessKind::And { .. }
            | WitnessKind::Pipe { .. }
            | WitnessKind::MethodCall { .. } => unreachable!("routed via infer_infix_chain"),
            WitnessKind::Call {
                callee,
                args,
                named_args,
            } => self.infer_call(callee, args, named_args, expr.span),
            WitnessKind::TensorSlice { target, axes } => {
                self.infer_tensor_slice(target, axes, expr.span)
            }
            WitnessKind::NoGrad { body } => self.infer_no_grad(body, expr.span),
            WitnessKind::Spawn { target } => self.infer_spawn(target, expr.span),
            WitnessKind::Select { arms } => self.infer_select(arms, expr.span),
            WitnessKind::Parallel { body } => {
                let (_ty, body_row) = self.infer_expr(body)?;
                Ok((Type::Nil, body_row))
            }
            // v0.103: export 内部声明 —— 类型/效果与不加 export 时相同
            // （export 只影响跨模块可见性，属 import 收集侧的静态判定）。
            WitnessKind::Export { decl, .. } => self.infer_expr(decl),
            // ── v0.104.6 D472：4 类新构造（语句型）──
            // 类型恒为 Nil（与 worker / transaction / eval / aggregate 的
            // 语义一致：它们是语句，不产生值）；效果行从体 / 被断言的
            // 表达式 / 贡献值传播。
            WitnessKind::Worker { body, .. } => {
                let (_ty, body_row) = self.infer_expr(body)?;
                Ok((Type::Nil, body_row))
            }
            WitnessKind::Transaction {
                body, compensation, ..
            } => {
                let (_ty, body_row) = self.infer_expr(body)?;
                let (_ty2, comp_row) = self.infer_expr(compensation)?;
                Ok((Type::Nil, Self::union_effect_rows(&body_row, &comp_row)))
            }
            WitnessKind::Eval { given, expects, .. } => {
                let (_ty, given_row) = self.infer_expr(given)?;
                let mut row = given_row;
                for e in expects {
                    let (_t, r) = self.infer_expr(e)?;
                    row = Self::union_effect_rows(&row, &r);
                }
                Ok((Type::Nil, row))
            }
            WitnessKind::Aggregate { value, .. } => {
                let (_ty, value_row) = self.infer_expr(value)?;
                Ok((Type::Nil, value_row))
            }
            // commit / rollback 是 unit 语句 —— 无类型、无效果。
            WitnessKind::Commit | WitnessKind::Rollback => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::Closure {
                type_params,
                params,
                body,
            } => self.infer_closure(type_params, params, body.as_ref(), expr.span),
            WitnessKind::FnDef {
                name,
                type_params,
                params,
                return_type,
                effect_annotation,
                body,
            } => self.infer_fn_def(
                Some(name.as_str()),
                type_params,
                params,
                return_type.as_ref().map(|h| h.to_type()),
                effect_annotation.as_ref(),
                body.as_ref(),
                expr.span,
            ),
            WitnessKind::Match { scrutinee, arms } => {
                self.infer_match(scrutinee.as_ref(), arms, expr.span)
            }
            WitnessKind::If { cond, then, r#else } => {
                self.infer_if(cond.as_ref(), then.as_ref(), r#else.as_deref(), expr.span)
            }
            WitnessKind::List(items) => self.infer_list(items, expr.span),
            WitnessKind::Dict(entries) => self.infer_dict(entries, expr.span),
            WitnessKind::DynTrait {
                expr: inner,
                trait_name,
                generics,
            } => self.infer_dyn_trait(inner, trait_name, generics, expr.span),
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
            // v0.104: `for x in xs … end` 的真实推断 —— 之前是 v0.55 的桩
            // `Ok((Type::Nil, Empty))`，**完全不推断 iterable 与 body**，
            // 于是循环体内的一切错误被静默吞掉：
            //   for i in [1,2,3] / print(nosuchvar) end   -- 不报 Unbound，
            //                                             运行期得 nil
            //   for i in [1,2,3] / perform Ai("x") end    -- 效果行不进残差，
            //                                             根边界断言漏检
            //   for x in 5i …                             -- iterable 非列表
            //                                             到运行期才 len() 报错
            // 现在：迭代变量按 iterable 的元素类型绑定（list<T> → T；
            // string → char；TypeVar/dict 宽容绑定），body 在子作用域推断，
            // 效果行并入，循环整体为 Nil。
            WitnessKind::Loop {
                var,
                iterable,
                body,
            } => {
                let (iter_ty, iter_row) = self.infer_expr(iterable)?;
                let elem_ty = match &iter_ty {
                    Type::List(elem) => elem.as_ref().clone(),
                    Type::String => Type::Char,
                    // TypeVar（待推断）/ Any / Unknown / dict → 宽容：元素
                    // 类型取 fresh 变量，让 body 内的用法自然约束它。
                    _ => self.fresh_type_var(),
                };
                let saved_env = self.env.clone();
                self.record_binding(expr.span, var, &elem_ty);
                self.env.add(var.clone(), elem_ty);
                let (_, body_row) = self.infer_expr(body)?;
                self.env = saved_env;
                let row = self.merge_rows(iter_row, body_row, expr.span);
                Ok((Type::Nil, row))
            }
            WitnessKind::While { cond, body } => {
                let (cond_ty, cond_row) = self.infer_expr(cond)?;
                self.constraints.push(
                    SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(cond_ty),
                            sup: Box::new(Type::Bool),
                        },
                        cond.span,
                    )
                    .with_hint("while condition must be bool"),
                );
                let saved_env = self.env.clone();
                let (_, body_row) = self.infer_expr(body)?;
                self.env = saved_env;
                let row = self.merge_rows(cond_row, body_row, expr.span);
                Ok((Type::Nil, row))
            }
            WitnessKind::Return(Some(v)) => {
                let (v_ty, row) = self.infer_expr(v)?;
                if let Some(expected_ret) = self.return_targets.last().cloned() {
                    self.constraints.push(
                        SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(v_ty.clone()),
                                sup: Box::new(expected_ret),
                            },
                            v.span,
                        )
                        .with_hint("return expression must match function return type"),
                    );
                }
                Ok((v_ty, row))
            }
            WitnessKind::Return(None) => {
                if let Some(expected_ret) = self.return_targets.last().cloned() {
                    self.constraints.push(
                        SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(Type::Nil),
                                sup: Box::new(expected_ret),
                            },
                            expr.span,
                        )
                        .with_hint("empty return must be compatible with function return type"),
                    );
                }
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::Break(_) | WitnessKind::Continue(_) => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::IndexAssign {
                object,
                index,
                value,
            } => {
                let (_, r1) = self.infer_expr(object)?;
                let (_, r2) = self.infer_expr(index)?;
                let (_, r3) = self.infer_expr(value)?;
                let row = self.merge_rows(r1, r2, expr.span);
                let row = self.merge_rows(row, r3, expr.span);
                Ok((Type::Nil, row))
            }
            // 顶层声明：本身不产生标量结果类型。
            //
            // `TypeAlias` / `Import` 是**纯类型声明**（无子表达式）⇒ 返 `Nil`。
            // `MacroDef` 则**不**与它们并列：宏体是**活表达式**，若按纯声明处理，
            // 宏体内的类型错与未绑定变量会**双双静默**
            // （实测：宏体内 `1 + "str"` → `1.0str` exit 0；`nosuchvar` → exit 0）。
            // 故它路由到 `infer_macro_def`（转调 `infer_fn_def`）。
            // `StructDef` / `EnumDef` / `TraitDef` / `ImplDef` 各自有独立的
            // 推断入口（见下方分支），会把字段/变体/方法体的类型一并检查。
            WitnessKind::TypeAlias { .. } | WitnessKind::Import(_) => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::StructDef {
                name,
                type_params,
                const_params,
                fields,
            } => self.infer_struct_def(name, type_params, const_params, fields, expr.span),
            WitnessKind::EnumDef {
                name,
                type_params,
                const_params,
                variants,
            } => self.infer_enum_def(name, type_params, const_params, variants, expr.span),
            WitnessKind::TraitDef {
                name,
                type_params,
                parents,
                methods,
            } => self.infer_trait_def(name, type_params, parents, methods, expr.span),
            WitnessKind::ImplDef {
                type_params,
                const_params,
                trait_name,
                trait_generics,
                target_type,
                target_generics,
                methods,
            } => self.infer_impl_def(
                type_params,
                const_params,
                trait_name.as_deref(),
                trait_generics,
                target_type,
                target_generics,
                methods,
                expr.span,
            ),
            WitnessKind::KernelDef {
                name,
                params,
                shared,
                body,
            } => self.infer_kernel_def(name, params, shared, body, expr.span),
            WitnessKind::MacroDef { name, params, body } => {
                self.infer_macro_def(name, params, body.as_ref(), expr.span)
            }
            // v0.84: Sequence 推断 — 依次推断子表达式，合并 effect row，
            // 返回最后一个表达式的类型（do-notation 语义）。空 Sequence → Nil。
            WitnessKind::Sequence(exprs) => self.infer_sequence(exprs, expr.span),
            // v0.80: Perform — 产生 effect，返回 fresh type var（由 handler 决定具体类型）。
            WitnessKind::Perform { effect, args } => self.infer_perform(effect, args, expr.span),
            // v0.80: Handle — 捕获 effect，body 的 effect row 中移除被捕获的 effect。
            WitnessKind::Handle {
                effect,
                body,
                handler,
                k_param,
            } => self.infer_handle(effect, body.as_ref(), handler.as_ref(), k_param, expr.span),
            // v0.88: Quasiquote — 与 quote 对称，返回 String 类型（源码字符串）。
            // 推断各子表达式（Unquote/UnquoteSplice），合并 effect row；
            // Quote 段为静态常量不产生 effect。
            WitnessKind::Quasiquote { segments } => {
                let mut row = crate::mir::effect::EffectRow::Empty;
                for seg in segments {
                    let (_, r) = self.infer_expr(seg)?;
                    row = self.merge_rows(row, r, seg.span);
                }
                Ok((Type::String, row))
            }
            // v0.98: effect 签名声明 —— 纯类型层，无类型/效果贡献。
            //（签名本身已在 infer_program 预扫描注册进 effect_signatures。）
            WitnessKind::Try { expr: inner } => self.infer_try(inner.as_ref(), expr.span),
            WitnessKind::Defer { body } => self.infer_defer(body.as_ref(), expr.span),
            WitnessKind::EffectSig { .. } => Ok((Type::Nil, crate::mir::effect::EffectRow::Empty)),
        }
    }

    /// G7: Validate that `inferred` effect row is permitted by an explicit function/method
    /// effect annotation (`! pure`, `! io, async`, `! {io, ..r}`), returning the signature row
    /// exposed to callers.
    pub(super) fn check_declared_effect_row(
        &mut self,
        declared: &crate::mir::effect::EffectRow,
        inferred: &crate::mir::effect::EffectRow,
        context: &str,
        span: Span,
    ) -> crate::mir::effect::EffectRow {
        let inferred_applied = row::apply_row(inferred, &self.subst);
        if declared.is_open() {
            return Self::union_effect_rows(declared, &inferred_applied);
        }
        let allowed = declared.labels();
        let forbidden: Vec<&str> = inferred_applied
            .labels()
            .into_iter()
            .filter(|lbl| !allowed.contains(lbl))
            .collect();
        if !forbidden.is_empty() {
            self.errors.push(TypeError::EffectRowMismatch {
                expected: format!("{context} declared effect row `! {declared}`"),
                got: format!(
                    "body performs forbidden effect(s): {}",
                    forbidden.join(", ")
                ),
                span: Some(span),
            });
        }
        declared.clone()
    }

    /// Merge two effect rows, emitting a spanned `RowEq` constraint when open.
    pub(crate) fn merge_rows(
        &mut self,
        a: crate::mir::effect::EffectRow,
        b: crate::mir::effect::EffectRow,
        span: Span,
    ) -> crate::mir::effect::EffectRow {
        use crate::mir::effect::EffectRow;
        match (a, b) {
            (EffectRow::Empty, b) => b,
            (a, EffectRow::Empty) => a,
            (EffectRow::Cons(h, t), b) => {
                if row_contains_concrete(&b, &h) {
                    self.merge_rows(*t, b, span)
                } else {
                    EffectRow::Cons(h, Box::new(self.merge_rows(*t, b, span)))
                }
            }
            (a, EffectRow::Cons(h, t)) => {
                if row_contains_concrete(&a, &h) {
                    self.merge_rows(a, *t, span)
                } else {
                    EffectRow::Cons(h, Box::new(self.merge_rows(a, *t, span)))
                }
            }
            (EffectRow::Var(v1), EffectRow::Var(v2)) => {
                if v1 != v2 {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::RowEq(EffectRow::Var(v1), EffectRow::Var(v2.clone())),
                        span,
                    ));
                }
                EffectRow::Var(v2)
            }
        }
    }

    fn infer_perform(
        &mut self,
        effect: &str,
        args: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let mut arg_rows = crate::mir::effect::EffectRow::Empty;
        let mut arg_tys = Vec::with_capacity(args.len());
        for arg in args {
            let (t, row) = self.infer_expr(arg)?;
            arg_tys.push(t.clone());
            arg_rows = self.merge_rows(arg_rows, row, arg.span);
        }
        let sig = self.effect_signatures.get(effect).cloned();
        if let Some(sig) = &sig {
            if sig.params.len() != args.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: sig.params.len(),
                    actual: args.len(),
                    span,
                }]);
            }
            for (i, (a_ty, p_ty)) in arg_tys.iter().zip(sig.params.iter()).enumerate() {
                if !a_ty.compatible_with(p_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("perform `{}` arg {} : {:?}", effect, i, p_ty),
                        got: format!("{:?}", a_ty),
                        span: Some(args.get(i).map(|a| a.span).unwrap_or(span)),
                    }]);
                }
            }
        }
        let result_ty = match sig {
            Some(s) => s.result,
            None => self.fresh_type_var(),
        };
        if self.closure_depth == 0
            && let Some(frame) = self.handle_stack.last_mut()
            && frame.effect == effect
        {
            frame.sites.push(PerformSite {
                result_ty: result_ty.clone(),
                arg_tys,
            });
        }
        let perform_row = crate::mir::effect::EffectRow::Cons(
            effect.to_string(),
            Box::new(crate::mir::effect::EffectRow::Empty),
        );
        Ok((result_ty, self.merge_rows(arg_rows, perform_row, span)))
    }

    /// v0.80: Handle 推断 — 捕获 effect，body 的 effect row 中移除被捕获的 effect。
    ///
    /// handler 体内的 `__arg0`, `__arg1`, ... 是 perform 传参的运行时约定
    /// （见 `src/runtime/effect.rs`）。类型检查时在 handler 作用域内注册
    /// 这些变量，让 handler body 的引用通过 typeck。
    ///
    /// v0.96: 吸收语义改为**直接行差**（[`EffectRow::remove`]）—— 残差 =
    /// body 行去掉全部被捕获标签。此前用严格等式约束
    /// `RowEq(body_row, Cons(effect, residual))`，当 body 不 perform 被
    /// 捕获效果时（`handle X { 纯 body }`，定义了未触发的 handler ——
    /// 合法程序），Empty-vs-Cons 的 unify_row 分支误报 "pure vs {X}"。
    /// 未知行（Var，来自未注册 callee 的调用）保留约束推迟消解。
    ///
    /// v0.97: perform↔handler 静态连接 ——
    /// - `__arg0..__argN`（N = 位点最大实参数）按位点实参的推断类型注册，
    ///   不再一律 Any；位点异质时退化为 Any（不误拒，精度优雅降级）；
    /// - 每个 perform 位点的结果类型（fresh var）与 handler 返回类型
    ///   Eq 统一 —— runtime 契约「handler 返回值 = resume 值」由此静态化。
    ///
    /// 位点经 **handle 帧栈**原位收集（infer_perform 记录到帧顶）：推断
    /// Perform 时栈顶恰好是运行时将接管它的 handler —— 内层同标签 handle
    /// 的 body 归内层、内层 handler 体归外层，归属天然正确且零 span 依赖。
    /// 闭包/fn 体内不下记录（调用点上下文未知 —— 高阶位点的连接留给
    /// 后续 effect signature 声明）。
    fn infer_handle(
        &mut self,
        effect: &str,
        body: &MirWitness,
        handler: &MirWitness,
        k_param: &str,
        _span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        self.handle_stack.push(HandleFrame {
            effect: effect.to_string(),
            sites: Vec::new(),
        });
        let (body_ty, body_row) = self.infer_expr(body)?;
        let frame = self.handle_stack.pop().expect("handle frame pushed above");
        let sites = frame.sites;
        let residual_row = match &body_row {
            crate::mir::effect::EffectRow::Var(_) => {
                let residual = self.fresh_row_var();
                self.constraints.push(SpannedConstraint::new(
                    Constraint::RowEq(
                        body_row.clone(),
                        crate::mir::effect::EffectRow::Cons(
                            effect.to_string(),
                            Box::new(residual.clone()),
                        ),
                    ),
                    _span,
                ));
                residual
            }
            _ => body_row.remove(effect),
        };
        // __arg0..__argN 注册：v0.98 签名优先（声明的参数契约，覆盖位点
        // 推导 —— 位点已对签名校验）；无签名回退 v0.97 位点推导。
        let sig = self.effect_signatures.get(effect).cloned();
        let saved_env = self.env.clone();
        let max_arity = match &sig {
            Some(s) => s.params.len().max(1),
            None => sites
                .iter()
                .map(|s| s.arg_tys.len())
                .max()
                .unwrap_or(1)
                .max(1),
        };
        for i in 0..max_arity {
            let registered = if let Some(s) = &sig {
                s.params.get(i).cloned().unwrap_or(Type::Any)
            } else {
                let mut arg_ty: Option<Type> = None;
                let mut heterogeneous = false;
                for s in &sites {
                    if let Some(t) = s.arg_tys.get(i) {
                        match &arg_ty {
                            None => arg_ty = Some(t.clone()),
                            Some(prev) => {
                                if !prev.compatible_with(t) && !t.compatible_with(prev) {
                                    heterogeneous = true;
                                }
                            }
                        }
                    }
                }
                if heterogeneous {
                    Type::Any
                } else {
                    arg_ty.unwrap_or(Type::Any)
                }
            };
            self.env.add(format!("__arg{}", i), registered);
        }

        // W2.3: explicit delimited continuation (`resume`) typing vs legacy auto_resume.
        let explicit_k = !k_param.is_empty() && witness_uses_ident(handler, k_param);
        if explicit_k {
            let perform_res_ty = if let Some(s) = &sig {
                s.result.clone()
            } else if !sites.is_empty() {
                let unified = self.fresh_type_var();
                for s in &sites {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Eq(Box::new(s.result_ty.clone()), Box::new(unified.clone())),
                        _span,
                    ));
                }
                unified
            } else {
                self.fresh_type_var()
            };
            let k_ty = Type::Arrow(
                Box::new(perform_res_ty),
                Box::new(body_ty.clone()),
                residual_row.clone(),
            );
            self.env.add(k_param.to_string(), k_ty);
        }

        let (handler_ty, handler_row) = self.infer_expr(handler)?;
        self.env = saved_env;

        if explicit_k {
            self.constraints.push(SpannedConstraint::new(
                Constraint::Eq(Box::new(body_ty.clone()), Box::new(handler_ty)),
                _span,
            ));
        } else {
            match &sig {
                Some(s) => {
                    if !handler_ty.compatible_with(&s.result) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!("handler for `{}` returns {:?}", effect, s.result),
                            got: format!("{:?}", handler_ty),
                            span: Some(_span),
                        }]);
                    }
                }
                None => {
                    for s in &sites {
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Eq(
                                Box::new(s.result_ty.clone()),
                                Box::new(handler_ty.clone()),
                            ),
                            _span,
                        ));
                    }
                }
            }
        }
        Ok((body_ty, self.merge_rows(residual_row, handler_row, _span)))
    }
}

/// W2.3: 检查 `w` 中是否自由引用了标识符 `target`（用于检测 handler 是否显式使用 `resume`）。
pub(crate) fn witness_uses_ident(root: &MirWitness, target: &str) -> bool {
    enum Task<'w> {
        Visit(&'w MirWitness),
        SequenceSlice(&'w [MirWitness]),
    }

    let mut stack = vec![Task::Visit(root)];
    while let Some(task) = stack.pop() {
        match task {
            Task::SequenceSlice(slice) => {
                if let Some((first, rest)) = slice.split_first() {
                    if !rest.is_empty()
                        && !matches!(
                            &first.kind,
                            WitnessKind::LetBinding { name, .. } if name == target
                        )
                    {
                        stack.push(Task::SequenceSlice(rest));
                    }
                    stack.push(Task::Visit(first));
                }
            }
            Task::Visit(w) => match &w.kind {
                WitnessKind::Variable(name) => {
                    if name == target {
                        return true;
                    }
                }
                WitnessKind::Assign {
                    target: name,
                    value,
                } => {
                    if name == target {
                        return true;
                    }
                    stack.push(Task::Visit(value));
                }
                WitnessKind::Call {
                    callee,
                    args,
                    named_args,
                } => {
                    match callee {
                        WitnessCallee::Name(n)
                        | WitnessCallee::Var(n)
                        | WitnessCallee::GenericName { name: n, .. }
                            if n == target =>
                        {
                            return true;
                        }
                        WitnessCallee::Evaluated(e) => stack.push(Task::Visit(e)),
                        WitnessCallee::Name(_)
                        | WitnessCallee::GenericName { .. }
                        | WitnessCallee::Var(_)
                        | WitnessCallee::Builtin(_)
                        | WitnessCallee::Method(_, _) => {}
                    }
                    for (_, a) in named_args.iter().rev() {
                        stack.push(Task::Visit(a));
                    }
                    for a in args.iter().rev() {
                        stack.push(Task::Visit(a));
                    }
                }
                WitnessKind::Closure { params, body, .. }
                | WitnessKind::FnDef { params, body, .. } => {
                    if !params.iter().any(|p| p.name == target) {
                        stack.push(Task::Visit(body));
                    }
                }
                WitnessKind::Loop {
                    var,
                    iterable,
                    body,
                } => {
                    if var != target {
                        stack.push(Task::Visit(body));
                    }
                    stack.push(Task::Visit(iterable));
                }
                WitnessKind::Handle {
                    body,
                    handler,
                    k_param,
                    ..
                } => {
                    if k_param != target {
                        stack.push(Task::Visit(handler));
                    }
                    stack.push(Task::Visit(body));
                }
                WitnessKind::Sequence(items) => {
                    stack.push(Task::SequenceSlice(items.as_slice()));
                }
                _ => {
                    for c in w.child_witnesses().into_iter().rev() {
                        stack.push(Task::Visit(c));
                    }
                }
            },
        }
    }
    false
}

/// v0.80: 检查 effect row 是否包含具体 label（不对 Var 返回 true，
/// 与 EffectRow::contains 的多态语义不同）。
fn row_contains_concrete(row: &crate::mir::effect::EffectRow, label: &str) -> bool {
    match row {
        crate::mir::effect::EffectRow::Empty => false,
        crate::mir::effect::EffectRow::Var(_) => false,
        crate::mir::effect::EffectRow::Cons(h, t) => h == label || row_contains_concrete(t, label),
    }
}

fn infer_lit(lit: &crate::common::Literal) -> Type {
    use crate::common::Literal;
    match lit {
        Literal::Int(_, _) => Type::Int,
        Literal::TypedInt(_, dt, _) => Type::from_dtype(*dt),
        Literal::Float(_, _) => Type::Float,
        Literal::TypedFloat(_, dt, _) => Type::from_dtype(*dt),
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

    // ─── v0.96: 效果边界断言（编译期 unhandled effect 强制）───

    fn wit_perform(effect: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Perform {
                effect: effect.to_string(),
                args: vec![],
            },
            span: Span::new(7, 3),
        }
    }

    fn wit_handler() -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::String("handled".to_string(), Span::default())),
            span: Span::default(),
        }
    }

    fn wit_handle(effect: &str, body: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Handle {
                effect: effect.to_string(),
                body: Box::new(body),
                handler: Box::new(wit_handler()),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }
    }

    fn wit_fn_def(name: &str, body: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::FnDef {
                name: name.to_string(),
                type_params: vec![],
                params: vec![],
                return_type: None,
                effect_annotation: None,
                body: Box::new(body),
            },
            span: Span::default(),
        }
    }

    fn wit_call_var(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Call {
                callee: WitnessCallee::Var(name.to_string()),
                args: vec![],
                named_args: Vec::new(),
            },
            span: Span::new(9, 1),
        }
    }

    #[test]
    fn program_boundary_rejects_unhandled_perform() {
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[wit_perform("Ai")]);
        assert_eq!(errors.len(), 1, "顶层未处理 perform 必须报错");
        let msg = errors[0].to_string();
        assert!(msg.contains("Ai"), "错误应指名效果标签: {}", msg);
        assert!(msg.contains("handle"), "错误应提示 handle 补救: {}", msg);
        // 定位到 perform 发起点（span 7:3）
        assert!(msg.contains("line 7"), "错误应定位 perform: {}", msg);
    }

    #[test]
    fn program_boundary_accepts_handled_perform() {
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[wit_handle("Ai", wit_perform("Ai"))]);
        assert!(
            errors.is_empty(),
            "handle 兜住的 perform 不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn program_boundary_rejects_mismatched_handle() {
        let mut hm = HMInference::new();
        // handle Fs 兜不住 perform Ai
        let errors = hm.infer_program(&[wit_handle("Fs", wit_perform("Ai"))]);
        assert_eq!(errors.len(), 1, "不匹配的 handle 必须报错");
        assert!(errors[0].to_string().contains("Ai"));
    }

    #[test]
    fn handle_with_pure_body_is_accepted() {
        let mut hm = HMInference::new();
        // v0.96 修复的误拒：定义了未触发的 handler（纯 body）是合法程序。
        // 此前 RowEq(Empty, Cons(Ai, residual)) 走 unify_row 的
        // Empty-vs-Cons 分支误报 "pure vs { Ai }"。
        let errors = hm.infer_program(&[wit_handle("Ai", lit_int(42))]);
        assert!(
            errors.is_empty(),
            "纯 body 的 handle 不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn task_effect_propagates_to_call_outside_handle() {
        // task doIt() = perform "Ai" …（witness 层是 FnDef）
        let def = wit_fn_def("doIt", wit_perform("Ai"));
        let call = wit_call_var("doIt");
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def, call]);
        assert_eq!(errors.len(), 1, "跨函数 perform 无 handle 必须报错");
        let msg = errors[0].to_string();
        assert!(msg.contains("Ai"), "错误应指名标签: {}", msg);
        assert!(msg.contains("doIt"), "错误应指出效果来自哪个调用: {}", msg);
    }

    #[test]
    fn task_effect_absorbed_by_matching_handle_at_call_site() {
        // 定义时 perform 合法 —— 调用点在 handle 内即可（非词法检查）。
        let def = wit_fn_def("doIt", wit_perform("Ai"));
        let call_under_handle = wit_handle("Ai", wit_call_var("doIt"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def, call_under_handle]);
        assert!(
            errors.is_empty(),
            "调用点被 handle 兜住不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn let_bound_closure_effect_propagates() {
        // let f = fn() = perform "Ai" …；f() —— env 里 Arrow 携带行
        let closure = MirWitness {
            kind: WitnessKind::Closure {
                type_params: vec![],
                params: vec![],
                body: Box::new(wit_perform("Ai")),
            },
            span: Span::default(),
        };
        let binding = MirWitness {
            kind: WitnessKind::LetBinding {
                name: "f".to_string(),
                type_hint: None,
                value: Box::new(closure),
                init_body: Box::new(MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(Span::default())),
                    span: Span::default(),
                }),
            },
            span: Span::default(),
        };
        let call = wit_call_var("f");
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[binding, call]);
        assert_eq!(errors.len(), 1, "let 绑定闭包的效果调用必须报错");
        assert!(errors[0].to_string().contains("Ai"));
    }

    #[test]
    fn nested_handles_absorb_own_effects() {
        // 内层 handle Fs 吸收 perform Fs；外层 handle Ai 吸收（残差已纯）。
        let inner = wit_handle("Fs", wit_perform("Fs"));
        let outer = wit_handle("Ai", inner);
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[outer]);
        assert!(errors.is_empty(), "嵌套 handle 各吸收各的: {:?}", errors);
    }

    // ─── v0.97: mutual recursion / 前向引用的效果传播 ───

    use crate::common::BinaryOp;

    fn wit_call(name: &str, span: Span) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Call {
                callee: WitnessCallee::Var(name.to_string()),
                args: vec![],
                named_args: Vec::new(),
            },
            span,
        }
    }

    fn wit_fn_named(name: &str, body: MirWitness) -> MirWitness {
        wit_fn_def(name, body)
    }

    #[test]
    fn mutual_recursion_effect_propagates() {
        // a 调 b、b perform X：不动点后 a 的行含 X —— a() 无 handle 报错。
        let def_a = wit_fn_named("a", wit_call("b", Span::new(2, 1)));
        let def_b = wit_fn_named("b", wit_perform("X"));
        let call = wit_call("a", Span::new(4, 1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def_a, def_b, call]);
        assert_eq!(
            errors.len(),
            1,
            "mutual recursion 的效果必须传播: {:?}",
            errors
        );
        let msg = errors[0].to_string();
        assert!(msg.contains("X"), "错误应指名标签: {}", msg);
    }

    #[test]
    fn forward_task_reference_inside_handle_clean() {
        // 前向引用：main 定义时 helper 尚未出现，调用点在 handle 内 → 合法。
        let def_main = wit_fn_named("main", wit_handle("X", wit_call("helper", Span::new(2, 3))));
        let def_helper = wit_fn_named("helper", wit_perform("X"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def_main, def_helper]);
        assert!(
            errors.is_empty(),
            "前向引用 + handle 兜住不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn self_recursive_effect_propagates() {
        // a 递归调用自己且某分支 perform X → 不动点后 a 的行含 X。
        let def_a = wit_fn_named(
            "a",
            MirWitness {
                kind: WitnessKind::If {
                    cond: Box::new(MirWitness {
                        kind: WitnessKind::Literal(Literal::Bool(true, Span::default())),
                        span: Span::default(),
                    }),
                    then: Box::new(wit_call("a", Span::default())),
                    r#else: Some(Box::new(wit_perform("X"))),
                },
                span: Span::default(),
            },
        );
        let call = wit_call("a", Span::new(3, 1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def_a, call]);
        assert_eq!(errors.len(), 1, "自递归效果必须传播: {:?}", errors);
        assert!(errors[0].to_string().contains("X"));
    }

    // ─── v0.97: perform ↔ handler 静态连接 ───

    fn wit_str(s: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::String(s.to_string(), Span::default())),
            span: Span::default(),
        }
    }

    fn wit_var(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::default(),
        }
    }

    fn wit_add(left: MirWitness, right: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Binary {
                left: Box::new(left),
                op: BinaryOp::Add,
                right: Box::new(right),
            },
            span: Span::default(),
        }
    }

    fn wit_perform_args(effect: &str, args: Vec<MirWitness>) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Perform {
                effect: effect.to_string(),
                args,
            },
            span: Span::default(),
        }
    }

    fn wit_let_typed(name: &str, hint: Type, value: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::LetBinding {
                name: name.to_string(),
                type_hint: Some(crate::mir::hint::TypeHint::from_type(hint)),
                value: Box::new(value),
                init_body: Box::new(MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(Span::default())),
                    span: Span::default(),
                }),
            },
            span: Span::default(),
        }
    }

    #[test]
    fn perform_result_unifies_with_handler_return() {
        // handler 返回 String；perform 结果标注为 Int 使用 → 连接后
        // Eq(Int, fresh) + Eq(fresh, String) 消解冲突报错。无连接时
        // perform 是自由 fresh var，Int 标注静默通过（漏检）。
        let body = wit_let_typed("v", Type::Int, wit_perform_args("X", vec![wit_str("s")]));
        let program = [wit_handle("X", body)];
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&program);
        assert_eq!(
            errors.len(),
            1,
            "perform 结果应与 handler 返回类型统一: {:?}",
            errors
        );
    }

    #[test]
    fn handler_arg_typed_from_perform_site() {
        // 位点传 Int → __arg0 静态为 Int → handler 内 Int + Int 合法。
        let body = wit_perform_args("X", vec![lit_int(41)]);
        let handler = wit_add(wit_var("__arg0"), lit_int(1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert!(errors.is_empty(), "类型化 __arg0 应通过: {:?}", errors);
    }

    #[test]
    fn handler_arg_mismatch_reports() {
        // 位点传 String、handler 内将 __arg0 标注为 Int 使用 → 报错
        //（连接生效：__arg0 静态为 String 而非 Any）。
        let body = wit_perform_args("X", vec![wit_str("s")]);
        let handler = wit_let_typed("v", Type::Int, wit_var("__arg0"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert_eq!(errors.len(), 1, "__arg0 类型不匹配必须报错: {:?}", errors);
    }

    #[test]
    fn multi_arg_perform_registers_all_handler_params() {
        // 两参 perform → __arg0/__arg1 都注册（此前只注册 __arg0，
        // handler 引用 __arg1 误报 UnboundVariable）。
        let body = wit_perform_args("X", vec![lit_int(1), lit_int(2)]);
        let handler = wit_add(wit_var("__arg0"), wit_var("__arg1"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert!(
            errors.is_empty(),
            "__arg1 必须被注册（运行时按 __arg0..N 注入）: {:?}",
            errors
        );
    }

    #[test]
    fn heterogeneous_sites_degrade_to_any() {
        // 两个位点 __arg0 分别 String / Int → 异质退化为 Any，
        // handler 内不做错误拒绝（精度优雅降级，不误拒）。
        let body = MirWitness {
            kind: WitnessKind::Sequence(vec![
                wit_perform_args("X", vec![wit_str("a")]),
                wit_perform_args("X", vec![lit_int(1)]),
            ]),
            span: Span::default(),
        };
        let handler = wit_add(wit_var("__arg0"), lit_int(1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert!(
            errors.is_empty(),
            "异质位点退化为 Any 不应误拒: {:?}",
            errors
        );
    }

    // ─── v0.98: 显式 effect 签名声明（全管线：parse → witness → typeck）───

    fn typecheck_src(src: &str) -> Vec<TypeError> {
        let (_, witnesses) = crate::parser_v3::ParserV3::compile(src).expect("parse");
        HMInference::new().infer_program(&witnesses)
    }

    #[test]
    fn effect_signature_validates_perform_arity() {
        let errs = typecheck_src("effect Ask(string): string\nperform Ask(\"hi\", \"extra\")");
        assert_eq!(errs.len(), 1, "签名 arity 校验: {:?}", errs);
        assert!(
            errs[0].to_string().contains("Expected 1 arguments"),
            "ArityMismatch 应指向签名声明的参数数: {:?}",
            errs
        );
    }

    #[test]
    fn effect_signature_validates_perform_arg_type() {
        let errs = typecheck_src("effect Ask(string): string\nperform Ask(42)");
        assert_eq!(errs.len(), 1, "签名实参类型校验: {:?}", errs);
        assert!(
            errs[0].to_string().contains("Ask"),
            "错误应指名效果与参数: {:?}",
            errs
        );
    }

    #[test]
    fn effect_signature_static_result_in_task_body() {
        // 高阶位点的结果静态化：perform 在 task 体内、无 handle 在场，
        // 结果类型取自签名 → let 标注 Int 与签名 String 冲突报错。
        // （无签名时结果是自由 fresh var，此程序静默通过 = 漏检。）
        let errs = typecheck_src(
            "effect Ask(string): string\ntask doIt()\n  let v: number = perform Ask(\"hi\")\nend",
        );
        assert_eq!(errs.len(), 1, "签名结果静态连接: {:?}", errs);
    }

    #[test]
    fn effect_signature_static_result_positive() {
        let errs = typecheck_src(
            "effect Ask(string): string\ntask doIt()\n  let v: string = perform Ask(\"hi\")\nend",
        );
        assert!(errs.is_empty(), "结果类型匹配签名应通过: {:?}", errs);
    }

    #[test]
    fn effect_signature_handler_params_pass() {
        let errs = typecheck_src(
            "effect Ask(number): number\nlet r = handle Ask {\n  perform Ask(42)\n} {\n  let n: number = __arg0\n  n\n}",
        );
        assert!(errs.is_empty(), "__arg0 按签名参数类型化应通过: {:?}", errs);
    }

    #[test]
    fn effect_signature_handler_params_mismatch() {
        let errs = typecheck_src(
            "effect Ask(string): string\nlet r = handle Ask {\n  perform Ask(\"q\")\n} {\n  let n: number = __arg0\n  \"ok\"\n}",
        );
        assert_eq!(errs.len(), 1, "__arg0 与签名参数类型不符应报错: {:?}", errs);
    }

    #[test]
    fn effect_signature_handler_return_mismatch() {
        // handler 返回 String、签名结果 number → 返回值必须兼容声明契约。
        let errs = typecheck_src(
            "effect Ask(string): number\nlet r = handle Ask {\n  perform Ask(\"q\")\n} {\n  \"text\"\n}",
        );
        assert_eq!(errs.len(), 1, "handler 返回失配签名结果应报错: {:?}", errs);
        assert!(
            errs[0].to_string().contains("handler"),
            "错误应指明是 handler 返回失配: {:?}",
            errs
        );
    }

    #[test]
    fn effect_signature_no_result_is_any() {
        // 缺省返回标注 = Any（仅载荷契约）：handler 返回任意值均合法。
        let errs = typecheck_src(
            "effect Log(string)\nlet r = handle Log {\n  perform Log(\"x\")\n} {\n  \"ok\"\n}",
        );
        assert!(
            errs.is_empty(),
            "无结果签名的 handler 返回不应受限: {:?}",
            errs
        );
    }

    #[test]
    fn duplicate_effect_signature_conflicts() {
        let errs = typecheck_src("effect Ask(string): string\neffect Ask(number): number");
        assert_eq!(errs.len(), 1, "签名不一致的重复声明应报错: {:?}", errs);
    }

    #[test]
    fn duplicate_effect_signature_identical_is_idempotent() {
        let errs = typecheck_src("effect Ask(string): string\neffect Ask(string): string");
        assert!(errs.is_empty(), "一致的重复声明应幂等放行: {:?}", errs);
    }

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
                cond: Box::new(MirWitness {
                    kind: WitnessKind::Literal(Literal::Bool(true, Span::default())),
                    span: Span::default(),
                }),
                then: Box::new(lit_int(2)),
                r#else: Some(Box::new(lit_int(3))),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        let (_subst, errors) = hm.solve_constraints();
        assert!(errors.is_empty(), "if(bool,int,int) should unify cleanly");
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
        let (_subst, errors) = hm.solve_constraints();
        assert!(
            errors.is_empty(),
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
            &[],
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
                named_args: Vec::new(),
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
        // v0.96: 吸收语义改为直接行差 —— effect 被捕获后残差**精确为纯**，
        // 不再是宽松的 fresh row var（后者会向上泄漏未消解的未知行）。
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
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
        let (_subst, errors) = hm.solve_constraints();
        assert!(
            errors.is_empty(),
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
                &[],
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
                &[],
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
        let mut hm = HMInference::new();
        let a = TypeVarId::Meta(999);
        let forall = Type::ForAll(vec![a.clone()], Box::new(Type::TypeVar(a)));
        let result1 = hm.instantiate_if_forall(&forall);
        let result2 = hm.instantiate_if_forall(&forall);
        match (&result1, &result2) {
            (Type::TypeVar(c1), Type::TypeVar(c2)) => {
                assert_ne!(c1, c2, "twice instantiated must produce distinct TypeVarId");
            }
            _ => panic!("expected TypeVar after instantiate"),
        }
    }
}
