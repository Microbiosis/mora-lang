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

use std::collections::HashSet;

use super::TypeError;

use super::hm::HMInference;

use super::hm::TypeError as HmError;

use crate::mir::MirExpr;

///  Run HM inference across the program (witness 输入) and return any
///  diagnostics. 阶段 3 目标形态：parse 直接产出 witness，typeck 直接
/// 消费 witness（零 MirExpr 桥接）。
pub fn check_program_witnesses(witnesses: &[crate::mir::witness::MirWitness]) -> Vec<TypeError> {
    let mut hm = HMInference::new();
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
