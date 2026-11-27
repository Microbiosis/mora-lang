//! v0.110.0: Public entry point for witness-based type checking.
//!
//! `check_program_witnesses` drives the Hindley-Milner inference engine
//! directly off `&[MirWitness]` and returns the collected diagnostics. It
//! is the single source of truth for CLI `mora --check` and LSP
//! `textDocument/publishDiagnostics`.
//!
//! 模块感知 — 顶层 `import "path"` 的目标文件符号在 typeck
//! 阶段预解析（visited 防环）并合并进 HM env，import 的符号不再报
//! UnboundVariable。路径解析与运行时 `mir_import` 一致（cwd 相对）。

use super::TypeError;

use super::hm::HMInference;

use super::hm::TypeError as HmError;

///  Run HM inference across the program (witness 输入) and return any
///  diagnostics. 阶段 3 目标形态：parse 直接产出 witness，typeck 直接
/// 消费 witness（无表达式中间层桥接）。
///
/// 入口链：`check_program_witnesses` → `check_program_witnesses_with_modules`
/// → 共享 inner helper `check_program_witnesses_inner`（import 符号预扫 +
/// HM `infer_program`）。
pub fn check_program_witnesses(witnesses: &[crate::mir::witness::MirWitness]) -> Vec<TypeError> {
    check_program_witnesses_with_modules(witnesses, None)
}

pub fn check_program_witnesses_with_modules(
    witnesses: &[crate::mir::witness::MirWitness],
    virtual_modules: Option<&std::collections::HashMap<String, String>>,
) -> Vec<TypeError> {
    let mut hm = HMInference::new();
    check_program_witnesses_inner(witnesses, &mut hm, virtual_modules)
}

/// Resolved HM type summary produced alongside diagnostics for LSP hover/completion.
#[derive(Debug, Default, Clone)]
pub struct InferredProgramInfo {
    pub diagnostics: Vec<TypeError>,
    pub env_types: std::collections::HashMap<String, super::Type>,
    pub span_types: std::collections::HashMap<crate::common::Span, super::Type>,
    pub bindings: Vec<(crate::common::Span, String, super::Type)>,
}

/// Run HM inference across `witnesses` and return both diagnostics and resolved HM types.
pub fn infer_program_types(witnesses: &[crate::mir::witness::MirWitness]) -> InferredProgramInfo {
    let mut hm = HMInference::new();
    let diagnostics = check_program_witnesses_inner(witnesses, &mut hm, None);
    let env_types = hm
        .env
        .all_bindings()
        .into_iter()
        .map(|(k, v)| (k, hm.resolve_type(&v)))
        .collect();
    let span_types = hm
        .inferred_spans
        .iter()
        .map(|(span, ty)| (*span, hm.resolve_type(ty)))
        .collect();
    let bindings = hm
        .recorded_bindings
        .iter()
        .map(|(span, name, ty)| (*span, name.clone(), hm.resolve_type(ty)))
        .collect();
    InferredProgramInfo {
        diagnostics,
        env_types,
        span_types,
        bindings,
    }
}

/// Resolve a builtin or module object's static HM type representation (for LSP hover/completion).
pub fn lookup_builtin_or_module_type(name: &str) -> Option<super::Type> {
    let mut hm = HMInference::new();
    hm.resolve_builtin_hover_type(name)
}

fn check_program_witnesses_inner(
    witnesses: &[crate::mir::witness::MirWitness],
    hm: &mut HMInference,
    virtual_modules: Option<&std::collections::HashMap<String, String>>,
) -> Vec<TypeError> {
    use std::collections::HashSet;
    let mut errors: Vec<TypeError> = Vec::new();

    // v0.75.18: 预扫描 import 目标文件的顶层符号并合并进 env
    // v0.98: effect 签名随 import 双通道注册
    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut import_errors: Vec<TypeError> = Vec::new();
    let imported = super::imports::collect_imported_symbols_with_modules(
        witnesses,
        &mut visited,
        &mut import_errors,
        virtual_modules,
    );
    imported.apply_to_hm(hm);
    errors.extend(import_errors);

    errors.extend(hm.infer_program(witnesses).into_iter().map(hm_to_external));
    errors
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
pub(super) fn hm_to_external(err: HmError) -> TypeError {
    use HmError::*;
    if let WithHint { inner, hint } = err {
        let mut te = hm_to_external(*inner);
        te.hint = Some(hint);
        return te;
    }
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
        // v0.104.6 D124：dict 字面量同质约束的专门变体。消息由 HM 侧的
        // Display 给出（已带键名与同质约束说明），这里只补 `expected` 字段，
        // 使 IDE / LSP 的结构化诊断能拿到可展示的期望类型。
        DictValueTypeMismatch {
            key,
            expected: exp,
            got,
            span,
        } => {
            let (l, c) = span.map(|s| (s.line, s.column)).unwrap_or((0, 0));
            (
                l,
                c,
                Some(format!("{}（为保持同质，键 '{key}' 的值需与之相同）", exp)),
                Some(got.clone()),
            )
        }
        // v0.104.6 D125：list 元素同质约束的姊妹变体。
        ListElementTypeMismatch {
            index,
            expected: exp,
            got,
            span,
        } => {
            let (l, c) = span.map(|s| (s.line, s.column)).unwrap_or((0, 0));
            (
                l,
                c,
                Some(format!(
                    "{}（为保持同质，下标 {index} 的元素需与之相同）",
                    exp
                )),
                Some(got.clone()),
            )
        }
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
        EffectRowMismatch {
            expected,
            got,
            span,
        } => {
            let (l, c) = span.map(|s| (s.line, s.column)).unwrap_or((0, 0));
            (l, c, Some(expected.clone()), Some(got.clone()))
        }
        UnsatisfiedDimConstraint {
            constraint,
            context,
            span,
        } => {
            let (l, c) = span.map(|s| (s.line, s.column)).unwrap_or((0, 0));
            (l, c, Some(constraint.clone()), Some(context.clone()))
        }
        WithHint { .. } => unreachable!("handled above"),
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
    use crate::common::{Literal, Span};
    use crate::mir::witness::{MirWitness, WitnessArm, WitnessCallee, WitnessKind, WitnessPattern};

    fn lit(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::default())),
            span: Span::default(),
        }
    }

    fn lit_at(n: i64, line: usize, col: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::new(line, col))),
            span: Span::new(line, col),
        }
    }

    fn str_lit_at(s: &str, line: usize, col: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::String(s.to_string(), Span::new(line, col))),
            span: Span::new(line, col),
        }
    }

    fn bool_lit_at(b: bool, line: usize, col: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Bool(b, Span::new(line, col))),
            span: Span::new(line, col),
        }
    }

    fn var(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::default(),
        }
    }

    fn var_at(name: &str, line: usize, col: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::new(line, col),
        }
    }

    #[test]
    fn empty_program_returns_no_errors() {
        assert!(check_program_witnesses(&[]).is_empty());
    }

    #[test]
    fn unbound_variable_yields_diagnostic() {
        let w = var("missing");
        let errs = check_program_witnesses(&[w]);
        assert!(!errs.is_empty(), "expected at least one diagnostic");
    }

    #[test]
    fn let_binding_and_reference_clean() {
        let program = vec![
            MirWitness {
                kind: WitnessKind::LetBinding {
                    name: "x".to_string(),
                    type_hint: None,
                    value: Box::new(lit(42)),
                    init_body: Box::new(var("x")),
                },
                span: Span::default(),
            },
            var("x"),
        ];
        assert!(check_program_witnesses(&program).is_empty());
    }

    #[test]
    fn if_cond_type_mismatch() {
        let program = vec![MirWitness {
            kind: WitnessKind::If {
                cond: Box::new(lit(1)),
                then: Box::new(lit(1)),
                r#else: Some(Box::new(lit(2))),
            },
            span: Span::default(),
        }];
        let errs = check_program_witnesses(&program);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("Type mismatch")
                    || e.expected.as_deref() == Some("bool")),
            "unified checker should catch If cond type mismatch, got {:?}",
            errs
        );
    }

    #[test]
    fn arity_mismatch_propagates_expected_and_actual() {
        // 调用非函数类型时 typeck 报错
        let not_a_fn = MirWitness {
            kind: WitnessKind::LetBinding {
                name: "x".to_string(),
                type_hint: None,
                value: Box::new(lit(42)),
                init_body: Box::new(var("x")),
            },
            span: Span::default(),
        };
        let bad_call = MirWitness {
            kind: WitnessKind::Call {
                callee: WitnessCallee::Var("x".to_string()),
                args: vec![lit(1)],
                named_args: Vec::new(),
            },
            span: Span::default(),
        };
        let errs = check_program_witnesses(&[not_a_fn, bad_call]);
        assert!(
            !errs.is_empty(),
            "expected type error when calling non-function, got none"
        );
        let e = &errs[0];
        assert!(
            e.expected.is_some() || e.actual.is_some() || !e.message.is_empty(),
            "error should carry diagnostic info, got {:?}",
            e
        );
    }

    // v0.75.86: let with type_hint 不一致应报真实行号
    #[test]
    fn let_with_type_hint_mismatch_uses_real_line() {
        let program = vec![MirWitness {
            kind: WitnessKind::LetBinding {
                name: "x".to_string(),
                type_hint: Some(crate::mir::hint::TypeHint::from_type(
                    crate::typeck::Type::Int,
                )),
                value: Box::new(str_lit_at("hello", 1, 5)),
                init_body: Box::new(var_at("x", 2, 0)),
            },
            span: Span::new(1, 0),
        }];
        let errs = check_program_witnesses(&program);
        assert!(!errs.is_empty(), "expected at least one error, got none");
        let mismatch = errs
            .iter()
            .find(|e| e.message.contains("Type mismatch"))
            .expect("expected type mismatch error");
        assert_eq!(
            mismatch.line, 1,
            "line should be 1, got {} (line 0 = bug)",
            mismatch.line
        );
        assert_eq!(
            mismatch.column, 5,
            "column should be 5 (RHS value span), got {}",
            mismatch.column
        );
    }

    // v0.75.86: match arms body type 不一致应报真实行号
    #[test]
    fn match_arms_body_type_mismatch_uses_real_line() {
        let program = vec![MirWitness {
            kind: WitnessKind::Match {
                scrutinee: Box::new(lit_at(42, 1, 0)),
                arms: vec![
                    WitnessArm {
                        pattern: WitnessPattern::Wildcard,
                        guard: None,
                        body: str_lit_at("str", 2, 4),
                    },
                    WitnessArm {
                        pattern: WitnessPattern::Wildcard,
                        guard: None,
                        body: lit_at(99, 3, 4),
                    },
                ],
            },
            span: Span::new(1, 0),
        }];
        let errs = check_program_witnesses(&program);
        if let Some(e) = errs.iter().find(|e| e.message.contains("Type")) {
            assert!(
                e.line > 0,
                "match arm mismatch should report real line, got line {}",
                e.line
            );
        }
    }

    // v0.75.86: if-else 分支 type 不一致应报真实行号
    #[test]
    fn if_branches_type_mismatch_uses_real_line() {
        let program = vec![MirWitness {
            kind: WitnessKind::If {
                cond: Box::new(bool_lit_at(true, 1, 3)),
                then: Box::new(lit_at(42, 1, 10)),
                r#else: Some(Box::new(str_lit_at("str", 1, 18))),
            },
            span: Span::new(1, 0),
        }];
        let errs = check_program_witnesses(&program);
        if let Some(e) = errs.iter().find(|e| e.message.contains("Type mismatch")) {
            assert!(
                e.line > 0,
                "if branches mismatch should report real line, got line {}",
                e.line
            );
        }
        for e in &errs {
            assert!(
                e.line > 0,
                "if-else error should have real line, got line 0: {:?}",
                e
            );
        }
    }

    // v0.75.86: 完整 HM span 化集成测试
    #[test]
    fn all_typeck_errors_have_real_line() {
        let program = vec![
            MirWitness {
                kind: WitnessKind::LetBinding {
                    name: "x".to_string(),
                    type_hint: Some(crate::mir::hint::TypeHint::from_type(
                        crate::typeck::Type::Int,
                    )),
                    value: Box::new(str_lit_at("str", 1, 12)),
                    init_body: Box::new(var_at("x", 1, 0)),
                },
                span: Span::new(1, 0),
            },
            var_at("nonexistent", 2, 5),
        ];
        let errs = check_program_witnesses(&program);
        assert!(errs.len() >= 2, "expected >= 2 errors, got {}", errs.len());
        for e in &errs {
            assert!(
                e.line > 0,
                "any typeck error should have real line, got line 0: {:?}",
                e
            );
        }
    }

    // v0.75.86: binop 元素类型不一致应报真实行号
    #[test]
    fn binop_type_mismatch_uses_real_line() {
        let program = vec![MirWitness {
            kind: WitnessKind::Binary {
                left: Box::new(lit_at(1, 1, 4)),
                op: crate::common::BinaryOp::Add,
                right: Box::new(str_lit_at("str", 1, 8)),
            },
            span: Span::new(1, 0),
        }];
        let errs = check_program_witnesses(&program);
        assert!(!errs.is_empty());
        assert!(
            errs[0].line > 0,
            "binop mismatch should have real line, got {}",
            errs[0].line
        );
    }

    // v0.75.86: list elem 类型不一致应报真实行号
    #[test]
    fn list_elem_type_mismatch_uses_real_line() {
        let program = vec![MirWitness {
            kind: WitnessKind::List(vec![
                lit_at(1, 1, 4),
                str_lit_at("str", 1, 8),
                lit_at(3, 1, 16),
            ]),
            span: Span::new(1, 0),
        }];
        let errs = check_program_witnesses(&program);
        assert!(!errs.is_empty());
        assert!(
            errs[0].line > 0,
            "list elem mismatch should have real line, got {}",
            errs[0].line
        );
    }
}
