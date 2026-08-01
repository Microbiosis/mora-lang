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

use crate::mir::expr::{MirExpr, MirExprKind};
use crate::typeck::Type;
use crate::typeck::TypeError;

/// 从一个模块的 MirExpr 列表提取其顶层符号类型。
///
/// - `let` 绑定：用该模块自身的 HM 推断结果（含 generalize）登记；
///   闭包身份 / 未解析 TypeVar 经 [`sanitize`] 退化为安全类型。
/// - `task`（FnDef）→ `Closure`；`struct`/`enum` → `Any`（占位）；
///   `type Alias = T` → 目标类型（真实信息）。
///
/// `pre` 是嵌套 import 已收集的符号，先合并进本模块 HM env —— 这样模块
/// 自己的 `let` 引用其 import 的符号时也能正确推断（传递 import 支持）。
fn extract_module_symbols(exprs: &[MirExpr], pre: &[(String, Type)]) -> Vec<(String, Type)> {
    let mut syms: Vec<(String, Type)> = Vec::new();

    // 1) let 绑定精确类型（模块自身的推断 + generalization）
    let mut hm = crate::typeck::hm::HMInference::new();
    for (name, ty) in pre {
        hm.env.add(name.clone(), ty.clone());
    }
    let _ = hm.infer_program(exprs); // 模块内部错误不在此冒泡（与运行时
    // mir_import 的 `let _type_errs` 一致）
    for (name, ty) in hm.env.all_bindings() {
        syms.push((name, sanitize(&ty)));
    }

    // 2) 显式声明名称登记（不在 HM env 中）
    for e in exprs {
        match &e.kind {
            MirExprKind::FnDef { name, .. } => syms.push((name.clone(), Type::Closure)),
            MirExprKind::StructDef { name, .. } | MirExprKind::EnumDef { name, .. } => {
                syms.push((name.clone(), Type::Any));
            }
            MirExprKind::TypeAlias { name, target } => {
                syms.push((name.clone(), target.clone()));
            }
            _ => {}
        }
    }

    syms
}

/// 合并前的安全化：闭包身份 TypeVar / 未解析 TypeVar 不能直接进目标 env
/// （不同模块的 fresh 变量命名空间相同，直接合会与目标模块的 closure_sigs
/// 侧表键冲突）。`forall<'a>.'a` 纯闭包身份 → `Closure`；结构型泛型值
/// （如 `list<'a>`）保留结构，内部 TypeVar 退化为 `Any`。
fn sanitize(ty: &Type) -> Type {
    match ty {
        Type::TypeVar(_) => Type::Any,
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
    exprs: &[MirExpr],
    visited: &mut HashSet<PathBuf>,
    errors: &mut Vec<TypeError>,
) -> Vec<(String, Type)> {
    let mut out: Vec<(String, Type)> = Vec::new();
    for e in exprs {
        if let MirExprKind::Import(path) = &e.kind {
            let path = path.clone();
            match std::fs::read_to_string(&path) {
                Ok(source) => {
                    let key = std::path::Path::new(&path);
                    let canon = key.canonicalize().unwrap_or_else(|_| key.to_path_buf());
                    if !visited.insert(canon) {
                        continue; // 已在栈上（a → b → a 环），跳过
                    }
                    match crate::interpreter::parse_code_v3(&source) {
                        Ok(module_exprs) => {
                            // 先递归子 import（收集符号供本模块推断预合并），
                            // 再提取本模块符号（传递 import 支持）
                            let nested = collect_imported_symbols(&module_exprs, visited, errors);
                            out.extend(extract_module_symbols(&module_exprs, &nested));
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
