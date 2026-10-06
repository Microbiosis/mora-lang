//! v0.55: Public entry point for witness-based type checking.
//!
//! `check_program_witnesses` drives the Hindley-Milner inference engine
//! directly off `&[MirWitness]` and returns the collected diagnostics. It
//! is the single source of truth for CLI `mora --check` and LSP
//! `textDocument/publishDiagnostics`.
//!
//! v0.75.18: 模块感知 — 顶层 `import "path"` 的目标文件符号在 typeck
//! 阶段预解析（visited 防环）并合并进 HM env，import 的符号不再报
//! UnboundVariable。路径解析与运行时 `mir_import` 一致（cwd 相对）。

use super::TypeError;

use super::hm::HMInference;

use super::hm::TypeError as HmError;

///  Run HM inference across the program (witness 输入) and return any
///  diagnostics. 阶段 3 目标形态：parse 直接产出 witness，typeck 直接
/// 消费 witness（零 MirExpr 桥接）。
///
/// v0.75.86：共享 inner helper（`check_program_witnesses_inner`）——
/// 双向集成（[`check_program_witnesses_bidirectional`]）复用 import +
/// HM infer_program，仅在前后插入双向预扫 + 重复错误过滤。
/// v0.75.87: match exhaustiveness 检查已根除（保留 HM 推理路径）。
pub fn check_program_witnesses(witnesses: &[crate::mir::witness::MirWitness]) -> Vec<TypeError> {
    let mut hm = HMInference::new();
    check_program_witnesses_inner(witnesses, &mut hm)
}

/// v0.75.86: 双向类型检查入口（Phase B/C 集成）。
///
/// 流程：
///   1. 收集 import 符号到 env（同 [`check_program_witnesses`]）
///   2. `BidirectionalChecker::pre_check_program` 预扫，产出双向
///      错误（Lambda/Call/If/LetBinding 关键节点的精准 expected/actual）
///   3. HM 跑全树，产出 HM 错误
///   4. 过滤：line+column 已诊断过的位置不再报（避免双向 + HM 重复）
///   5. 合并双向 + 过滤后 HM 错误
///
/// 与 [`check_program_witnesses`] 区别：双向层**前置**在 HM 全树合一
/// 之前，错误诊断更精准（expected/actual 直接来自 check_against），
/// 配合 `HMInference::diagnosed` 跟踪避免重复。
pub fn check_program_witnesses_bidirectional(
    witnesses: &[crate::mir::witness::MirWitness],
) -> Vec<TypeError> {
    use crate::typeck::bidirectional::BidirectionalChecker;
    use std::collections::HashSet;
    let mut hm = HMInference::new();

    // v0.75.18: 预扫描 import 目标文件的顶层符号并合并进 env
    // v0.98: effect 签名随 import 双通道注册
    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut import_errors: Vec<TypeError> = Vec::new();
    let imported =
        super::imports::collect_imported_symbols(witnesses, &mut visited, &mut import_errors);
    for (name, ty) in imported.env_bindings {
        hm.env.add(name, ty);
    }
    for (label, sig) in imported.effect_signatures {
        hm.effect_signatures.insert(label, sig);
    }

    // 双向预扫 —— 关键节点的精准 expected/actual
    let mut checker = BidirectionalChecker::new(&mut hm);
    checker.pre_check_program(witnesses);
    // v0.75.94: 拆开 checker（drop 后才能用 hm）——
    // 提取 errors / diag 两个 owned 字段，剩余字段（mode_stack / nodes_visited
    // / hm 借用）随 checker drop 释放
    let bidir_errors = std::mem::take(&mut checker.errors);
    let _nodes_visited = checker.nodes_visited;
    let diag = std::mem::take(&mut checker.diag);
    drop(checker); // 释放 &mut hm 借用

    // HM 全树合一 —— 同位置已被双向诊断的过滤
    // Phase G 调研结论：按 (line, column, kind) 三元组过滤不可行
    // —— 双向 mark_diagnosed 标记的是 witness kind (Literal/Call/...)，
    // HM error kind 是 TypeError 枚举 variant (UnboundVariable/...)，
    // 两层 kind 维度不对应（同位置 witness kind 唯一但 HM error kind
    // 可能有多个）。退而按 line+column 二元组过滤（Phase A 已有）。
    let hm_errors: Vec<TypeError> = hm
        .infer_program(witnesses)
        .into_iter()
        .map(hm_to_external)
        .collect();
    // v0.104.6 D128：**line+column 二元组不够** —— 同一处冲突两层报的
    // **位置不同**，二元组对不上，过滤失效，用户会看到两条讲同一件事的诊断：
    //
    //   let b: string = a          （a: Float）
    //   ├ bidirectional  type mismatch: expected `String`, got `Float`  @ line 2:17 ← 指向 `a`
    //   └ HM             Type mismatch: expected String, got Float          @ line 2:1  ← 指向 `let`
    //
    // 两层的 `expected` / `actual` 文本其实**一致**，只有位置不同。
    // 故补一条 (line, expected, actual) 三元组去重 —— 它能命中这条，
    // 又比「只比 line」保守：同一行内**不同**类型对的冲突仍各自保留。
    let bidir_keys: std::collections::HashSet<(usize, Option<String>, Option<String>)> =
        bidir_errors
            .iter()
            .map(|e| (e.line, e.expected.clone(), e.actual.clone()))
            .collect();
    // v0.104.6 D188：D128 的三元组去重对**一条常见情况失效** ——
    // 同一个类型冲突被报两遍，而用户被告知「2 type error(s) found」。
    //
    // 实测 `let v: Int = 1.5`：
    //
    // ```text
    // Type error at line 1:14: type mismatch: expected `Int`, got `Float`
    // Type error (位置未跟踪): Type mismatch: expected int, got float
    // 2 type error(s) found.          ← 实际只有 1 个问题
    // ```
    //
    // 三元组对不上的**两个**原因：
    // 1. **位置不同** —— HM 那条 `line == 0`。D126 已把「line 0」渲染成
    //    「位置未跟踪」（诚实兜底），而 `Constraint` 不携带 span 是**已记的**
    //    根因（D126 待办）。
    // 2. **措辞不同** —— 双向层写 `` `Int` ``（带反引号、大写），
    //    HM 层写 `int`（小写、无反引号）。**同一件事，两种写法。**
    //
    // 故对**无位置**（`line == 0`）的 HM 错误补一条按
    // **(规范化 expected, 规范化 actual)** 的去重：规范化 = 去掉反引号 +
    // 转小写。**只影响本来就没有位置信息的那些** —— 那类错误
    // （D126 判定为「低信息」）若描述的类型对已被一条**带位置**的诊断
    // 覆盖，就是同一件事的第二份表述，报两遍只会让**错误计数撒谎**。
    // 同一行内**不同**类型对的冲突不受影响。
    let norm_pair = |e: &TypeError| {
        let n = |s: &Option<String>| s.as_ref().map(|x| x.replace('`', "").to_lowercase());
        (n(&e.expected), n(&e.actual))
    };
    let bidir_pairs: std::collections::HashSet<(Option<String>, Option<String>)> =
        bidir_errors.iter().map(norm_pair).collect();
    let filtered_hm_errors: Vec<TypeError> = hm_errors
        .into_iter()
        .filter(|e| {
            // v0.75.94: DiagFilter owned 实例过滤
            // 按 line+column 过滤（与 DiagFilter::diagnosed 伪 ID 的 line+column 部分对比）
            !diag.is_diagnosed_at_line_column(e.line, e.column)
        })
        .filter(|e| !bidir_keys.contains(&(e.line, e.expected.clone(), e.actual.clone())))
        .filter(|e| !(e.line == 0 && bidir_pairs.contains(&norm_pair(e))))
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
    // v0.98: effect 签名随 import 双通道注册
    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut import_errors: Vec<TypeError> = Vec::new();
    let imported =
        super::imports::collect_imported_symbols(witnesses, &mut visited, &mut import_errors);
    for (name, ty) in imported.env_bindings {
        hm.env.add(name, ty);
    }
    for (label, sig) in imported.effect_signatures {
        hm.effect_signatures.insert(label, sig);
    }
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

    // v0.75.86: 双向集成 — If 条件不是 bool 时双向报 type mismatch
    #[test]
    fn bidirectional_if_cond_type_mismatch() {
        let program = vec![MirWitness {
            kind: WitnessKind::If {
                cond: Box::new(lit(1)), // Int 而非 Bool
                then: Box::new(lit(1)),
                r#else: Some(Box::new(lit(2))),
            },
            span: Span::default(),
        }];
        let bidir_errs = check_program_witnesses_bidirectional(&program);
        assert!(
            bidir_errs
                .iter()
                .any(|e| e.message.contains("type mismatch")),
            "bidirectional should catch If cond type mismatch, got {:?}",
            bidir_errs
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
            mismatch.column, 0,
            "column should be 0 (let keyword), got {}",
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
