//! v0.110.0: 跨模块 import 符号表（typeck 阶段预扫描与合并）。
//!
//! typeck 阶段预扫描顶层 `import "path"` 语句，递归解析目标文件（visited
//! 集合防环），提取其顶层符号类型并合并进 HM env — 引用 import 的符号不再
//! 报 UnboundVariable。
//!
//! 路径解析与运行时 `mir_import` 完全一致（cwd 相对，`read_to_string(path)`），
//! 因此 `mora --check` 与运行时对同一 import 的解析不会分叉。运行时语义不变。
//!
//! 精度方案：模块自身的 `let` 绑定类型由该模块的 HM 推断产出（含
//! let-generalization）；`task`/`struct`/`enum`/`type` 等显式声明按名称登记
//! （这些名称不进 HM env，只有 let 绑定才进）。合并前做 sanitize：闭包身份
//! TypeVar / 未解析 TypeVar 退化为 `Closure`/`Any`，避免闭包身份在跨模块
//! 比较时无法匹配。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::mir::witness::{MirWitness, WitnessKind};
use crate::typeck::hm::{EnumTypeDef, FnParamSpec, ImplMethodTypeSig, StructTypeDef, TraitTypeDef};
use crate::typeck::{Type, TypeError, TypeVarId};

/// v0.98 / Wave 4: import 收集结果 —— env 绑定、effect 签名与名义类型/impl/参数规范全量传播。
#[derive(Debug, Default)]
pub struct ImportedModuleSymbols {
    pub env_bindings: Vec<(String, Type)>,
    pub effect_signatures: Vec<(String, crate::typeck::hm::EffectSignature)>,
    pub struct_defs: HashMap<String, StructTypeDef>,
    pub enum_defs: HashMap<String, EnumTypeDef>,
    pub trait_defs: HashMap<String, TraitTypeDef>,
    pub inherent_impls: HashMap<(String, String), ImplMethodTypeSig>,
    pub trait_impl_methods: HashMap<(String, String, String), ImplMethodTypeSig>,
    pub trait_impl_pairs: HashSet<(String, String)>,
    pub fn_param_specs: HashMap<String, FnParamSpec>,
    pub fn_arities: HashMap<String, usize>,
    pub fn_effect_rows: HashMap<String, crate::mir::effect::EffectRow>,
}

impl ImportedModuleSymbols {
    fn extend(&mut self, other: ImportedModuleSymbols) {
        self.env_bindings.extend(other.env_bindings);
        self.effect_signatures.extend(other.effect_signatures);
        self.struct_defs.extend(other.struct_defs);
        self.enum_defs.extend(other.enum_defs);
        self.trait_defs.extend(other.trait_defs);
        self.inherent_impls.extend(other.inherent_impls);
        self.trait_impl_methods.extend(other.trait_impl_methods);
        self.trait_impl_pairs.extend(other.trait_impl_pairs);
        self.fn_param_specs.extend(other.fn_param_specs);
        self.fn_arities.extend(other.fn_arities);
        self.fn_effect_rows.extend(other.fn_effect_rows);
    }

    fn extend_ref(&mut self, other: &ImportedModuleSymbols) {
        self.env_bindings.extend(other.env_bindings.iter().cloned());
        self.effect_signatures
            .extend(other.effect_signatures.iter().cloned());
        self.struct_defs.extend(
            other
                .struct_defs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        self.enum_defs
            .extend(other.enum_defs.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.trait_defs
            .extend(other.trait_defs.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.inherent_impls.extend(
            other
                .inherent_impls
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        self.trait_impl_methods.extend(
            other
                .trait_impl_methods
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        self.trait_impl_pairs
            .extend(other.trait_impl_pairs.iter().cloned());
        self.fn_param_specs.extend(
            other
                .fn_param_specs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        self.fn_arities
            .extend(other.fn_arities.iter().map(|(k, v)| (k.clone(), *v)));
        self.fn_effect_rows.extend(
            other
                .fn_effect_rows
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
    }

    pub fn apply_to_hm(&self, hm: &mut crate::typeck::hm::HMInference) {
        for (name, ty) in &self.env_bindings {
            hm.env.add(name.clone(), ty.clone());
        }
        for (label, sig) in &self.effect_signatures {
            hm.effect_signatures.insert(label.clone(), sig.clone());
        }
        for (k, v) in &self.struct_defs {
            hm.struct_defs.insert(k.clone(), v.clone());
        }
        for (k, v) in &self.enum_defs {
            hm.enum_defs.insert(k.clone(), v.clone());
        }
        for (k, v) in &self.trait_defs {
            hm.trait_defs.insert(k.clone(), v.clone());
        }
        for (k, v) in &self.inherent_impls {
            hm.inherent_impls.insert(k.clone(), v.clone());
        }
        for (k, v) in &self.trait_impl_methods {
            hm.trait_impl_methods.insert(k.clone(), v.clone());
        }
        for p in &self.trait_impl_pairs {
            hm.trait_impl_pairs.insert(p.clone());
        }
        for (k, v) in &self.fn_param_specs {
            hm.fn_param_specs.insert(k.clone(), v.clone());
        }
        for (k, v) in &self.fn_arities {
            hm.fn_arities.insert(k.clone(), *v);
        }
        for (k, v) in &self.fn_effect_rows {
            hm.fn_effect_rows.insert(k.clone(), v.clone());
        }
    }
}

/// 从一个模块的 witness 列表提取其顶层符号类型。
///
/// - `let` 绑定：用该模块自身的 HM 推断结果（含 generalize）登记；
///   闭包身份 / 未解析 TypeVar 经 [`sanitize`] 退化为安全类型。
/// - `task`（FnDef）→ `Closure`；`struct`/`enum` → `Any`（占位）；
///   `type Alias = T` → 目标类型（真实信息）。
///
/// `pre` 是嵌套 import 已收集的符号，先合并进本模块 HM env —— 这样模块
/// 自己的 `let` 引用其 import 的符号时也能正确推断（传递 import 支持）。
///
/// 消费 MirWitness（parse 层直接产出 witness，唯一实现）。
fn extract_module_symbols(
    witnesses: &[MirWitness],
    nested: &ImportedModuleSymbols,
    module_path: &str,
    errors: &mut Vec<TypeError>,
) -> ImportedModuleSymbols {
    let mut syms: Vec<(String, Type)> = Vec::new();

    // 1) 逐条顶层声明求**精确类型**，并将模块内部静态类型错误冒泡到导入方检查结果。
    let mut hm = crate::typeck::hm::HMInference::new();
    nested.apply_to_hm(&mut hm);
    for hm_err in hm.infer_program(witnesses) {
        let mut ext = crate::typeck::check_mir::hm_to_external(hm_err);
        ext.message = format!("in imported module '{}': {}", module_path, ext.message);
        errors.push(ext);
    }
    // let 绑定（已在 env 中，含 generalization）
    for (name, ty) in hm.env.all_bindings() {
        syms.push((name, sanitize(&ty)));
    }

    // 显式声明名称登记（不在 HM env 中）。
    // v0.103: `export <声明>` 把声明包在 `WitnessKind::Export` 里 —— 提取
    // 顶层符号时必须**下潜**到 decl。
    fn top_decls<'a>(w: &'a MirWitness, out: &mut Vec<&'a MirWitness>) {
        match &w.kind {
            WitnessKind::Export { decl, .. } => top_decls(decl, out),
            WitnessKind::Sequence(items) => {
                for it in items {
                    top_decls(it, out);
                }
            }
            _ => out.push(w),
        }
    }
    let mut decls: Vec<&MirWitness> = Vec::new();
    for w in witnesses {
        top_decls(w, &mut decls);
    }
    for d in &decls {
        match &d.kind {
            // FnDef: 优先复用 infer_program 已泛化并求解好的签名
            WitnessKind::FnDef { name, .. } => {
                if !syms.iter().any(|(n, _)| n == name) {
                    let ty = hm.infer_expr(d).map(|(t, _)| t).unwrap_or(Type::Closure);
                    syms.push((name.clone(), sanitize(&ty)));
                }
            }
            WitnessKind::StructDef { .. }
            | WitnessKind::EnumDef { .. }
            | WitnessKind::TraitDef { .. } => {}
            WitnessKind::KernelDef { name, params, .. } => {
                if !syms.iter().any(|(n, _)| n == name) {
                    let sig = params.iter().map(|p| p.ty).collect();
                    syms.push((name.clone(), Type::GpuKernel(Some(sig))));
                }
            }
            WitnessKind::TypeAlias { name, target } => {
                syms.push((name.clone(), target.to_type().clone()));
            }
            _ => {}
        }
    }

    // v0.103: 导出集 —— 只有 `export <声明>` 包裹的名字对导入方可见
    // （spec §10.2）。未导出的绑定是模块私有。
    let mut export_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut exported_impl_targets: HashSet<String> = HashSet::new();
    let mut exported_impl_traits: HashSet<(String, String)> = HashSet::new();
    for w in witnesses {
        if let WitnessKind::Export { names, decl } = &w.kind {
            for n in names {
                export_names.insert(n.clone());
            }
            if let WitnessKind::ImplDef {
                trait_name,
                target_type,
                ..
            } = &decl.kind
            {
                if let Some(tr) = trait_name {
                    exported_impl_traits.insert((tr.clone(), target_type.clone()));
                } else {
                    exported_impl_targets.insert(target_type.clone());
                }
            }
        }
    }
    // v0.103: 可见性收紧 —— **默认私有**。
    syms.retain(|(n, _)| export_names.contains(n));

    let struct_defs: HashMap<String, StructTypeDef> = hm
        .struct_defs
        .into_iter()
        .filter(|(n, _)| export_names.contains(n))
        .collect();
    let enum_defs: HashMap<String, EnumTypeDef> = hm
        .enum_defs
        .into_iter()
        .filter(|(n, _)| export_names.contains(n))
        .collect();
    let trait_defs: HashMap<String, TraitTypeDef> = hm
        .trait_defs
        .into_iter()
        .filter(|(n, _)| export_names.contains(n))
        .collect();
    let inherent_impls: HashMap<(String, String), ImplMethodTypeSig> = hm
        .inherent_impls
        .into_iter()
        .filter(|((tgt, _), _)| export_names.contains(tgt) || exported_impl_targets.contains(tgt))
        .collect();
    let trait_impl_methods: HashMap<(String, String, String), ImplMethodTypeSig> = hm
        .trait_impl_methods
        .into_iter()
        .filter(|((tr, tgt, _), _)| {
            export_names.contains(tr)
                || export_names.contains(tgt)
                || exported_impl_traits.contains(&(tr.clone(), tgt.clone()))
        })
        .collect();
    let trait_impl_pairs: HashSet<(String, String)> = hm
        .trait_impl_pairs
        .into_iter()
        .filter(|(tr, tgt)| {
            export_names.contains(tr)
                || export_names.contains(tgt)
                || exported_impl_traits.contains(&(tr.clone(), tgt.clone()))
        })
        .collect();
    let fn_param_specs: HashMap<String, FnParamSpec> = hm
        .fn_param_specs
        .into_iter()
        .filter(|(n, _)| export_names.contains(n))
        .collect();
    let fn_arities: HashMap<String, usize> = hm
        .fn_arities
        .into_iter()
        .filter(|(n, _)| export_names.contains(n))
        .collect();
    let fn_effect_rows: HashMap<String, crate::mir::effect::EffectRow> = hm
        .fn_effect_rows
        .into_iter()
        .filter(|(n, _)| export_names.contains(n))
        .collect();

    // 3) v0.98: effect 签名导出（模块声明的效果契约随 import 传播）
    let mut effect_signatures: Vec<(String, crate::typeck::hm::EffectSignature)> = Vec::new();
    for w in witnesses {
        if let WitnessKind::EffectSig {
            name,
            params,
            result,
        } = &w.kind
        {
            effect_signatures.push((
                name.clone(),
                crate::typeck::hm::EffectSignature {
                    params: params.iter().map(|h| h.to_type().clone()).collect(),
                    result: result
                        .as_ref()
                        .map(|h| h.to_type().clone())
                        .unwrap_or(crate::typeck::Type::Any),
                },
            ));
        }
    }

    ImportedModuleSymbols {
        env_bindings: syms,
        effect_signatures,
        struct_defs,
        enum_defs,
        trait_defs,
        inherent_impls,
        trait_impl_methods,
        trait_impl_pairs,
        fn_param_specs,
        fn_arities,
        fn_effect_rows,
    }
}

/// Sanitize exported types across module boundaries: rigid type parameters
/// and `ForAll` wrappers stay polymorphic, while unresolved meta variables
/// degrade safely to `Any` (or `Closure` for naked closure identities).
fn sanitize(ty: &Type) -> Type {
    match ty {
        Type::TypeVar(TypeVarId::Rigid(_)) => ty.clone(),
        Type::TypeVar(TypeVarId::Meta(_)) => Type::Any,
        Type::ForAll(vars, inner) => match inner.as_ref() {
            Type::TypeVar(_) => Type::Closure,
            _ => {
                let kept_vars: Vec<TypeVarId> = vars
                    .iter()
                    .filter(|v| matches!(v, TypeVarId::Rigid(_)))
                    .cloned()
                    .collect();
                let clean_inner = sanitize_keeping(inner, &kept_vars);
                if kept_vars.is_empty() {
                    clean_inner
                } else {
                    Type::ForAll(kept_vars, Box::new(clean_inner))
                }
            }
        },
        Type::List(e) => Type::List(Box::new(sanitize(e))),
        Type::Channel(e) => Type::Channel(Box::new(sanitize(e))),
        Type::TaskHandle(e) => Type::TaskHandle(Box::new(sanitize(e))),
        Type::Dict(k, v) => Type::Dict(Box::new(sanitize(k)), Box::new(sanitize(v))),
        Type::Result_(ok, err) => Type::Result_(Box::new(sanitize(ok)), Box::new(sanitize(err))),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| Box::new(sanitize(e))).collect()),
        Type::Cons(h, t) => Type::Cons(Box::new(sanitize(h)), Box::new(sanitize(t))),
        Type::Arrow(i, o, row) => Type::Arrow(
            Box::new(sanitize_param(i)),
            Box::new(sanitize(o)),
            sanitize_row(row),
        ),
        Type::Union(members) => Type::Union(members.iter().map(sanitize).collect()),
        _ => ty.clone(),
    }
}

fn sanitize_row(row: &crate::mir::effect::EffectRow) -> crate::mir::effect::EffectRow {
    use crate::mir::effect::EffectRow;
    match row {
        EffectRow::Empty | EffectRow::Var(_) => EffectRow::Empty,
        EffectRow::Cons(label, tail) => {
            EffectRow::Cons(label.clone(), Box::new(sanitize_row(tail)))
        }
    }
}

fn sanitize_param(ty: &Type) -> Type {
    match ty {
        Type::Arrow(_, _, _) => Type::Any,
        Type::Tuple(elems) => {
            Type::Tuple(elems.iter().map(|e| Box::new(sanitize_param(e))).collect())
        }
        _ => sanitize(ty),
    }
}

fn sanitize_param_keeping(ty: &Type, kept: &[TypeVarId]) -> Type {
    match ty {
        Type::Arrow(_, _, _) => Type::Any,
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| Box::new(sanitize_param_keeping(e, kept)))
                .collect(),
        ),
        _ => sanitize_keeping(ty, kept),
    }
}

fn sanitize_keeping(ty: &Type, kept: &[TypeVarId]) -> Type {
    match ty {
        Type::TypeVar(id) if kept.contains(id) => ty.clone(),
        Type::TypeVar(TypeVarId::Rigid(_)) => ty.clone(),
        Type::TypeVar(TypeVarId::Meta(_)) => Type::Any,
        Type::List(e) => Type::List(Box::new(sanitize_keeping(e, kept))),
        Type::Channel(e) => Type::Channel(Box::new(sanitize_keeping(e, kept))),
        Type::TaskHandle(e) => Type::TaskHandle(Box::new(sanitize_keeping(e, kept))),
        Type::Dict(k, v) => Type::Dict(
            Box::new(sanitize_keeping(k, kept)),
            Box::new(sanitize_keeping(v, kept)),
        ),
        Type::Result_(ok, err) => Type::Result_(
            Box::new(sanitize_keeping(ok, kept)),
            Box::new(sanitize_keeping(err, kept)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| Box::new(sanitize_keeping(e, kept)))
                .collect(),
        ),
        Type::Cons(h, t) => Type::Cons(
            Box::new(sanitize_keeping(h, kept)),
            Box::new(sanitize_keeping(t, kept)),
        ),
        Type::Arrow(i, o, row) => Type::Arrow(
            Box::new(sanitize_param_keeping(i, kept)),
            Box::new(sanitize_keeping(o, kept)),
            sanitize_row(row),
        ),
        Type::Union(members) => {
            Type::Union(members.iter().map(|m| sanitize_keeping(m, kept)).collect())
        }
        _ => ty.clone(),
    }
}

pub fn collect_imported_symbols(
    witnesses: &[MirWitness],
    visited: &mut HashSet<PathBuf>,
    errors: &mut Vec<TypeError>,
) -> ImportedModuleSymbols {
    collect_imported_symbols_with_modules(witnesses, visited, errors, None)
}

pub fn collect_imported_symbols_with_modules(
    witnesses: &[MirWitness],
    visited: &mut HashSet<PathBuf>,
    errors: &mut Vec<TypeError>,
    virtual_modules: Option<&HashMap<String, String>>,
) -> ImportedModuleSymbols {
    let mut cache: HashMap<PathBuf, ImportedModuleSymbols> = HashMap::new();
    collect_imported_symbols_cached(witnesses, visited, &mut cache, errors, virtual_modules)
}

fn collect_imported_symbols_cached(
    witnesses: &[MirWitness],
    active_stack: &mut HashSet<PathBuf>,
    cache: &mut HashMap<PathBuf, ImportedModuleSymbols>,
    errors: &mut Vec<TypeError>,
    virtual_modules: Option<&HashMap<String, String>>,
) -> ImportedModuleSymbols {
    let mut out = ImportedModuleSymbols::default();
    for w in witnesses {
        if let WitnessKind::Import(path) = &w.kind {
            let path = path.clone();
            let key = std::path::Path::new(&path);
            let canon = key.canonicalize().unwrap_or_else(|_| key.to_path_buf());
            if let Some(cached) = cache.get(&canon) {
                out.extend_ref(cached);
                continue;
            }
            if !active_stack.insert(canon.clone()) {
                continue; // 真环（a → b → a），跳过
            }
            let read_res = if let Some(s) = virtual_modules.and_then(|m| m.get(&path)) {
                Ok(s.clone())
            } else if let Some(std_src) = crate::resolve_std_module(&path) {
                Ok(std_src.to_string())
            } else {
                std::fs::read_to_string(&path)
            };
            match read_res {
                Ok(source) => {
                    match crate::parser_v3::ParserV3::compile_with_modules(&source, virtual_modules)
                    {
                        Ok((_, module_witnesses)) => {
                            let nested = collect_imported_symbols_cached(
                                &module_witnesses,
                                active_stack,
                                cache,
                                errors,
                                virtual_modules,
                            );
                            let mut own =
                                extract_module_symbols(&module_witnesses, &nested, &path, errors);
                            own.extend(nested);
                            out.extend_ref(&own);
                            cache.insert(canon.clone(), own);
                        }
                        Err(parse_err) => errors.push(TypeError::new(
                            0,
                            format!("import error: failed to parse {}: {}", path, parse_err),
                        )),
                    }
                }
                Err(io_err) => errors.push(TypeError::new(
                    0,
                    format!("import error: failed to read {}: {}", path, io_err),
                )),
            }
            active_stack.remove(&canon);
        }
    }
    out
}
