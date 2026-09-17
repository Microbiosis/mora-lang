//! v0.75.18: 跨模块 import 符号表。
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
//! TypeVar / 未解析 TypeVar 退化为 `Closure`/`Any`，避免跨模块 closure_sigs
//! 侧表键冲突。

use std::collections::HashSet;
use std::path::PathBuf;

use crate::mir::witness::{MirWitness, WitnessKind};
use crate::typeck::Type;
use crate::typeck::TypeError;

/// v0.98: import 收集结果 —— env 绑定与 effect 签名双通道。
#[derive(Debug, Default)]
pub struct ImportedModuleSymbols {
    pub env_bindings: Vec<(String, Type)>,
    pub effect_signatures: Vec<(String, crate::typeck::hm::EffectSignature)>,
}

impl ImportedModuleSymbols {
    fn extend(&mut self, other: ImportedModuleSymbols) {
        self.env_bindings.extend(other.env_bindings);
        self.effect_signatures.extend(other.effect_signatures);
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
/// v0.75.40: 消费 MirWitness（parse 层直接产出；exprs 版由 check_program_mir
/// 桥接 from_exprs 后进入本函数，单一实现）。
fn extract_module_symbols(
    witnesses: &[MirWitness],
    pre: &[(String, Type)],
) -> ImportedModuleSymbols {
    let mut syms: Vec<(String, Type)> = Vec::new();

    // 1) 逐条顶层声明求**精确类型**。
    //
    // v0.103 修复：此前只把 `let` 绑定从模块 HM env 取出，而 `task`(FnDef)
    // 一律硬编码为裸 `Type::Closure` —— 但调用点需要精确的 Arrow 才能完成
    // `greet("x")` 的合一，于是「导入的 task 被调用」必然报
    // "expected closure, got fn (string) -> …"（无任何测试覆盖该路径，
    // 缺陷长期潜伏）。现在对每条顶层声明单独 `infer_expr` 取其真实类型：
    // FnDef → curried Arrow，let → 绑定类型，struct/enum → Unknown，
    // type alias → 目标类型。
    let mut hm = crate::typeck::hm::HMInference::new();
    for (name, ty) in pre {
        hm.env.add(name.clone(), ty.clone());
    }
    let _ = hm.infer_program(witnesses); // 模块内部错误不在此冒泡（与运行时
    // mir_import 的 `let _type_errs` 一致）
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
            // FnDef: 用推断出的精确 Arrow（此前硬编码 Closure → 调用必失败）
            WitnessKind::FnDef { name, .. } => {
                let ty = hm.infer_expr(d).map(|(t, _)| t).unwrap_or(Type::Closure);
                syms.push((name.clone(), sanitize(&ty)));
            }
            WitnessKind::StructDef { name, .. } | WitnessKind::EnumDef { name, .. } => {
                syms.push((name.clone(), Type::Unknown));
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
    for w in witnesses {
        if let WitnessKind::Export { names, .. } = &w.kind {
            for n in names {
                export_names.insert(n.clone());
            }
        }
    }
    // v0.103: 可见性收紧 —— **默认私有**。不做「无 export 则全导出」的兼容
    // 回退（§6 禁止兼容分支；本仓库当前零 import 用法，无须过渡）。
    syms.retain(|(n, _)| export_names.contains(n));

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
    }
}

/// 合并前的安全化：闭包身份 TypeVar / 未解析 TypeVar 不能直接进目标 env
/// （不同模块的 fresh 变量命名空间相同，直接合会与目标模块的 closure_sigs
/// 侧表键冲突）。`forall<'a>.'a` 纯闭包身份 → `Closure`；结构型泛型值
/// （如 `list<'a>`）保留结构，内部 TypeVar 退化为 `Any`。
fn sanitize(ty: &Type) -> Type {
    match ty {
        Type::TypeVar(_) => Type::Unknown,
        Type::ForAll(vars, inner) => match inner.as_ref() {
            Type::TypeVar(_) => Type::Closure,
            _ => Type::ForAll(vars.clone(), Box::new(sanitize(inner))),
        },
        Type::List(e) => Type::List(Box::new(sanitize(e))),
        Type::Dict(k, v) => Type::Dict(Box::new(sanitize(k)), Box::new(sanitize(v))),
        Type::Result_(ok, err) => Type::Result_(Box::new(sanitize(ok)), Box::new(sanitize(err))),
        Type::Union(members) => Type::Union(members.iter().map(sanitize).collect()),
        _ => ty.clone(),
    }
}

/// 递归收集所有 import 的顶层符号（含嵌套 import），visited 防环。
///
/// 读取失败 / 解析失败的文件产出一条 `TypeError` 诊断（与运行时
/// `mir_import` 的 hard error 语义一致）。返回的符号对直接 `env.add` 进
/// 目标 HM 环境。
pub fn collect_imported_symbols(
    witnesses: &[MirWitness],
    visited: &mut HashSet<PathBuf>,
    errors: &mut Vec<TypeError>,
) -> ImportedModuleSymbols {
    let mut out = ImportedModuleSymbols::default();
    for w in witnesses {
        if let WitnessKind::Import(path) = &w.kind {
            let path = path.clone();
            match std::fs::read_to_string(&path) {
                Ok(source) => {
                    let key = std::path::Path::new(&path);
                    let canon = key.canonicalize().unwrap_or_else(|_| key.to_path_buf());
                    if !visited.insert(canon) {
                        continue; // 已在栈上（a → b → a 环），跳过
                    }
                    match crate::parser_v3::ParserV3::compile(&source) {
                        Ok((_, module_witnesses)) => {
                            // 先递归子 import（收集符号供本模块推断预合并），
                            // 再提取本模块符号（传递 import 支持）
                            let nested =
                                collect_imported_symbols(&module_witnesses, visited, errors);
                            let own =
                                extract_module_symbols(&module_witnesses, &nested.env_bindings);
                            out.extend(own);
                            out.extend(nested);
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
        }
    }
    out
}
