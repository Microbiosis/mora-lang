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

use crate::mir::MirExpr;

use super::TypeError;
use super::hm::HMInference;

use super::hm::TypeError as HmError;

///  Run HM inference across the program and return any diagnostics.
///  The function is total: a successful return is `Vec::new()`, a failed
///  program returns one or more `TypeError` entries (e.g. unbound
///  variables, arity mismatches, unification failures).
pub fn check_program_mir(exprs: &[MirExpr]) -> Vec<TypeError> {
    let mut hm = HMInference::new();
    let mut errors: Vec<TypeError> = Vec::new();

    // v0.75.18: 预扫描 import 目标文件的顶层符号并合并进 env
    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut import_errors: Vec<TypeError> = Vec::new();
    for (name, ty) in
        super::imports::collect_imported_symbols(exprs, &mut visited, &mut import_errors)
    {
        hm.env.add(name, ty);
    }
    errors.extend(import_errors);

    errors.extend(hm.infer_program(exprs).into_iter().map(hm_to_external));
    errors
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
///  shape consumed by CLI `--check` and LSP diagnostics.
fn hm_to_external(err: HmError) -> TypeError {
    use HmError::*;
    let line = match &err {
        UnboundVariable { span, .. } | ArityMismatch { span, .. } | NotAClosure { span, .. } => {
            span.line
        }
        UnificationFailure { span, .. }
        | OccursCheck { span, .. }
        | GeneralizationFailed { span, .. }
        | InvalidLiteral { span, .. } => span.map(|s| s.line).unwrap_or(0),
        HmDisabled => 0,
    };
    let message = err.to_string();
    TypeError::with_detail(line, message, "", "", "")
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
}
