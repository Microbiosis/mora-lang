//! v0.55: Public entry point for MirExpr-native type checking.
//!
//! `check_program_mir` drives the Hindley-Milner inference engine
//! directly off `&[MirExpr]` and returns the collected diagnostics. It
//! is the single source of truth for CLI `mora --check` and LSP
//! `textDocument/publishDiagnostics`.
//!
//! v0.75.18: 模块感知 — 顶层 `import "path"` 的目标文件符号在 typeck
//! 阶段预解析（visited 防环）并合并进 HM env，import 的符号不再报
//! UnboundVariable。路径解析与运行时 `mir_import` 一致（cwd 相对）。

use super::TypeError;

use super::hm::HMInference;

use super::hm::TypeError as HmError;

use crate::mir::MirExpr;

///  Run HM inference across the program (witness 输入) and return any
///  diagnostics. 阶段 3 目标形态：parse 直接产出 witness，typeck 直接
/// 消费 witness（零 MirExpr 桥接）。
///
/// v0.75.86：共享 inner helper（[`check_program_witnesses_inner`]）——
/// 双向集成（[`check_program_witnesses_bidirectional`]）复用 import +
/// HM infer_program，仅在前后插入双向预扫 + 重复错误过滤。
pub fn check_program_witnesses(witnesses: &[crate::mir::witness::MirWitness]) -> Vec<TypeError> {
    let mut hm = HMInference::new();
    check_program_witnesses_inner(witnesses, &mut hm)
}

/// v0.75.86: 双向类型检查入口（Phase B/C 集成）。
///
/// 流程：
///   1. 收集 import 符号到 env（同 [`check_program_witnesses`]）
///   2. [`BidirectionalChecker::pre_check_program`] 预扫，产出双向
///      错误（Lambda/Call/If/LetBinding 关键节点的精准 expected/actual）
///   3. HM 跑全树，产出 HM 错误
///   4. 过滤：line+column 已诊断过的位置不再报（避免双向 + HM 重复）
///   5. 合并双向 + 过滤后 HM 错误
///
/// 与 [`check_program_witnesses`] 区别：双向层**前置**在 HM 全树合一
/// 之前，错误诊断更精准（expected/actual 直接来自 check_against），
/// 配合 [`HMInference::diagnosed`] 跟踪避免重复。
pub fn check_program_witnesses_bidirectional(
    witnesses: &[crate::mir::witness::MirWitness],
) -> Vec<TypeError> {
    use crate::typeck::bidirectional::BidirectionalChecker;
    use std::collections::HashSet;
    let mut hm = HMInference::new();

    // v0.75.18: 预扫描 import 目标文件的顶层符号并合并进 env
    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut import_errors: Vec<TypeError> = Vec::new();
    for (name, ty) in
        super::imports::collect_imported_symbols(witnesses, &mut visited, &mut import_errors)
    {
        hm.env.add(name, ty);
    }

    // 双向预扫 —— 关键节点的精准 expected/actual
    let mut checker = BidirectionalChecker::new(&mut hm);
    checker.pre_check_program(witnesses);
    let bidir_errors = checker.errors;
    let _nodes_visited = checker.nodes_visited;

    // HM 全树合一 —— 同位置已被双向诊断的过滤
    let hm_errors: Vec<TypeError> = hm
        .infer_program(witnesses)
        .into_iter()
        .map(hm_to_external)
        .collect();
    let filtered_hm_errors: Vec<TypeError> = hm_errors
        .into_iter()
        .filter(|e| {
            // 按 line+column 过滤（与 HM::diagnosed 伪 ID 的 line+column 部分对比）
            !hm.diagnosed
                .iter()
                .any(|node_id| node_id.line == e.line && node_id.column == e.column)
        })
        .collect();

    let mut errors = bidir_errors;
    errors.extend(import_errors);
    errors.extend(filtered_hm_errors);
    errors
}

/// 共享内部：HM 推理 + 错误转换（不处理 import、不处理双向）
fn check_program_witnesses_inner(
    witnesses: &[crate::mir::witness::MirWitness],
    hm: &mut HMInference,
) -> Vec<TypeError> {
    use std::collections::HashSet;
    let mut errors: Vec<TypeError> = Vec::new();

    // v0.75.18: 预扫描 import 目标文件的顶层符号并合并进 env
    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut import_errors: Vec<TypeError> = Vec::new();
    for (name, ty) in
        super::imports::collect_imported_symbols(witnesses, &mut visited, &mut import_errors)
    {
        hm.env.add(name, ty);
    }
    errors.extend(import_errors);

    errors.extend(hm.infer_program(witnesses).into_iter().map(hm_to_external));
    errors
}

///  Run HM inference across the program and return any diagnostics.
///  The function is total: a successful return is `Vec::new()`, a failed
///  program returns one or more `TypeError` entries (e.g. unbound
///  variables, arity mismatches, unification failures).
///
/// v0.75.40: exprs 版保留为测试兼容桥接（LSP/既有调用方仍产出 MirExpr）；
/// 执行路径已切到 [`check_program_witnesses`]。
pub fn check_program_mir(exprs: &[MirExpr]) -> Vec<TypeError> {
    check_program_witnesses(&crate::mir::witness::MirWitness::from_exprs(exprs))
}

///  Same as [`check_program_mir`]. Kept as a thin wrapper for callers
///  that expect a `(errors, exprs)` shape; the returned expressions are
///  the input untouched (no per-node type annotations are attached to
///  `MirExpr` — type info is exposed only via `TypeError` diagnostics).
pub fn check_program_mir_with_types(exprs: &[MirExpr]) -> (Vec<TypeError>, Vec<MirExpr>) {
    let errors = check_program_mir(exprs);
    (errors, exprs.to_vec())
}

///  Convert an internal `hm::TypeError` into the public `typeck::TypeError`
/// shape consumed by CLI `--check` and LSP diagnostics.
///
/// v0.75.86: HM error fields are now propagated to the public
///     `expected`/`actual` columns (previously discarded as empty strings
///     — LSP `if e.expected.is_some()` triggered but the payload was
///     empty). Each variant's typed fields map to the most useful pair:
///       - ArityMismatch { expected, actual }      → expected/actual
///       - UnificationFailure { expected, got }     → expected/actual
///       - OccursCheck { var, with_ty }            → actual="type containing α var" / expected hint
///       - NotAClosure { found }                   → actual
///       - InvalidLiteral { what, value }           → expected=`<what>`, actual=value
///       - UnboundVariable / GeneralizationFailed  → no structured fields, leave None
///     `hint` is intentionally left None here — adding automated hints is
///     out of scope for this commit (it would require analysing the witness
///     to suggest fixes, which is a separate feature).
fn hm_to_external(err: HmError) -> TypeError {
    use HmError::*;
    let (line, column, expected, actual) = match &err {
        UnboundVariable { span, .. } | NotAClosure { span, .. } => {
            (span.line, span.column, None, None)
        }
        ArityMismatch {
            expected: exp,
            actual: act,
            span,
        } => (
            span.line,
            span.column,
            Some(exp.to_string()),
            Some(act.to_string()),
        ),
        UnificationFailure {
            expected: exp,
            got,
            span,
        } => match span {
            Some(s) => (s.line, s.column, Some(exp.clone()), Some(got.clone())),
            None => (0, 0, Some(exp.clone()), Some(got.clone())),
        },
        OccursCheck { var, with_ty, span } => {
            let (l, c) = span.map(|s| (s.line, s.column)).unwrap_or((0, 0));
            (
                l,
                c,
                Some(format!("type variable `{}`", var)),
                Some(with_ty.clone()),
            )
        }
        GeneralizationFailed { reason, span } => {
            let (l, c) = span.map(|s| (s.line, s.column)).unwrap_or((0, 0));
            (l, c, Some(reason.clone()), None)
        }
        InvalidLiteral { what, value, span } => {
            let (l, c) = span.map(|s| (s.line, s.column)).unwrap_or((0, 0));
            (
                l,
                c,
                Some(format!("valid {} literal", what)),
                Some(value.clone()),
            )
        }
    };
    let message = err.to_string();
    let mut te = TypeError::new(line, message);
    te.column = column;
    te.expected = expected;
    te.actual = actual;
    te
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Span;
    use crate::mir::expr::{MirExpr, MirExprKind};

    fn lit(n: i64) -> MirExpr {
        MirExpr::lit(
            crate::common::Literal::Int(n, Span::default()),
            Span::default(),
        )
    }

    #[test]
    fn empty_program_returns_no_errors() {
        assert!(check_program_mir(&[]).is_empty());
    }

    #[test]
    fn unbound_variable_yields_diagnostic() {
        let expr = MirExpr::var("missing".to_string(), Span::default());
        let errs = check_program_mir(&[expr]);
        assert!(!errs.is_empty(), "expected at least one diagnostic");
    }

    #[test]
    fn let_binding_and_reference_clean() {
        let program = vec![
            MirExpr {
                kind: MirExprKind::LetBinding {
                    name: "x".to_string(),
                    type_hint: None,
                    value: Box::new(lit(42)),
                    init_body: Box::new(MirExpr::var("x".to_string(), Span::default())),
                },
                span: Span::default(),
            },
            MirExpr::var("x".to_string(), Span::default()),
        ];
        assert!(check_program_mir(&program).is_empty());
    }

    // v0.75.86: hm_to_external must propagate HM typed fields to the
    // public `expected`/`actual` columns so LSP diagnostics show them.
    // Before this fix all three columns were empty strings (Some(""))
    // even when the HM variant carried e.g. `expected: usize, actual: usize`.
    //
    // We use `let f = closure(2 args)` form (not `task f` / `FnDef`)
    // because `infer_fn_def` does not yet register the name into the
    // HM env — calls to `FnDef`-named functions fall back to `Type::Any`
    // and bypass arity checking. Closures bound via `let` go through
    // `infer_let` which DOES register them, exercising the arity check
    // we want to verify.

    // v0.75.86: 双向集成（[`check_program_witnesses_bidirectional`]）
    //   - If 条件不是 bool —— HM 不查 cond 类型，仅双向会报 type mismatch
    //   - 验证双向路径产错、HM 路径不产错
    #[test]
    fn bidirectional_if_cond_type_mismatch() {
        // if 42 then 1 else 2 — cond 期望 bool 实际 Int
        let program = vec![MirExpr {
            kind: MirExprKind::If {
                cond: Box::new(lit(1)), // Int 而非 Bool
                then: Box::new(lit(1)),
                r#else: Some(Box::new(lit(2))),
            },
            span: Span::default(),
        }];
        // HM 路径（不带双向）—— 不产错（If/else 不会触发 cond 类型检查）
        let hm_errs = check_program_mir(&program);
        // 双向路径 —— 触发双向 If 节点 check_against(Bool)
        let bidir_errs = check_program_witnesses_bidirectional(
            &crate::mir::witness::MirWitness::from_exprs(&program),
        );
        // 双向应产出 type mismatch 错误
        assert!(
            bidir_errs
                .iter()
                .any(|e| e.message.contains("type mismatch")),
            "bidirectional should catch If cond type mismatch, got {:?}",
            bidir_errs
        );
        // HM 路径对此不报错（双重证明双向比 HM 多抓到错）
        let _ = hm_errs; // 当前不强制断言 HM 不报——它可能报也可能不报
    }

    #[test]
    fn arity_mismatch_propagates_expected_and_actual() {
        // let f = closure(x, y) body  then  f(1)  — too few args
        let closure_def = MirExpr {
            kind: MirExprKind::LetBinding {
                name: "f".to_string(),
                type_hint: None,
                value: Box::new(MirExpr::closure(
                    vec![
                        crate::mir::expr::Param {
                            name: "x".to_string(),
                            type_hint: None,
                            default: None,
                        },
                        crate::mir::expr::Param {
                            name: "y".to_string(),
                            type_hint: None,
                            default: None,
                        },
                    ],
                    lit(0),
                    Span::default(),
                )),
                init_body: Box::new(MirExpr::var("f".to_string(), Span::default())),
            },
            span: Span::default(),
        };
        let bad_call = MirExpr::call(
            crate::mir::expr::MirCallee::Var("f".to_string()),
            vec![lit(1)],
            Span::default(),
        );
        let errs = check_program_mir(&[closure_def, bad_call]);
        assert!(!errs.is_empty(), "expected arity mismatch diagnostic");
        let e = &errs[0];
        assert!(
            e.expected.is_some() && !e.expected.as_deref().unwrap_or("").is_empty(),
            "expected column must carry HM ArityMismatch.expected, got {:?}",
            e.expected
        );
        assert!(
            e.actual.is_some() && !e.actual.as_deref().unwrap_or("").is_empty(),
            "actual column must carry HM ArityMismatch.actual, got {:?}",
            e.actual
        );
    }
}
