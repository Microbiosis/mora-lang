//! v0.75.86: 双向类型检查骨架入口（Phase A）。
//!
//! 设计目标：复用现有 HM 算法（[`crate::typeck::hm::HMInference`]）
//! 作为推断后端，在**关键节点**（Lambda 参数、Call 实参、If/Match
//! 分支）上叠加**期望类型**预判（synth ↑ / check ↓）—— 失败时产出
//! 精准的 `expected` / `actual` 诊断（不依赖 HM 全程序合一后才报错）。
//!
//! 与 AGENTS.md §6「最小修改原则」一致：骨架不替换 HM，仅在
//! 现有 `check_program_witnesses` 流程上**前置**一次双向预扫，
//! 并用 `HMInference::diagnosed` 跟踪已诊断节点，
//! HM 跑完后过滤重复错误。
//!
//! Phase A 范围（本次提交）：
//!   - `BidirectionalChecker<'a>` 结构 + `Mode` 枚举
//!   - `check_against(&MirWitness, expected: &Type) -> Result<Type, TypeError>` 入口
//!   - `synth` 模式转发 HM
//!   - `pre_check_program` 演示：递归 witness 树并在关键节点触发 check
//!   - 不替换任何 HM 代码 —— HM 行为 100% 保留
//!
//! Phase B/C 后续：Call 实参、If 条件、Match 分支等节点扩展。

use crate::common::Span;
use crate::mir::witness::{MirWitness, WitnessKind};
use crate::typeck::Type;
use crate::typeck::TypeError;
use crate::typeck::hm::HMInference;

/// v0.75.86: 双向定型模式状态机。
///
/// `Synth`（无期望类型，从表达式推出类型）+ `Check`（在已知期望类型
/// 下验证表达式合法，不产出类型）。**切换规则**：
///   - 用户显式标注节点（`let x: T = ...`）→ Check(T)
///   - 函数实参（已知 `f: A → B`）→ Check(A)
///   - 其他顶层 / 中间节点 → Synth
#[derive(Debug, Clone)]
pub enum Mode {
    /// 推导模式：调用 HM `infer_expr` 推类型
    Synth,
    /// 检查模式：在 `expected` 类型下验证 witness
    Check(Type),
}

/// v0.75.86: 双向类型检查器（叠加在 HMInference 之上）。
///
/// **不**替换 HM，**借用** HM 作为推断后端：
///   - `synth` 模式 → 调 `HMInference::infer_expr`
///   - `check` 模式 → 用 [`Type::subtype_of`] 验证 expected
///
/// 检查失败时调用 `HMInference::mark_diagnosed` 标记此节点
/// —— HM 跑完整树时，外部 ([`crate::typeck::check_mir`]) 可用
/// `is_diagnosed_at` 过滤重复错误。
pub struct BidirectionalChecker<'a> {
    /// 借用的 HM 引擎（不转移所有权）
    pub hm: &'a mut HMInference,
    /// 当前节点模式（栈结构支持嵌套）
    mode_stack: Vec<Mode>,
    /// 双向 check 直接产出的错误（不依赖 HM 跑全树）
    pub errors: Vec<TypeError>,
    /// 调试用：双向预扫覆盖的节点数
    pub nodes_visited: usize,
}

impl<'a> BidirectionalChecker<'a> {
    /// 构造新 checker。`hm` 必须比 checker 生命周期长
    /// （典型用法：函数局部 `let mut checker = BidirectionalChecker::new(&mut hm);`）
    pub fn new(hm: &'a mut HMInference) -> Self {
        Self {
            hm,
            mode_stack: Vec::new(),
            errors: Vec::new(),
            nodes_visited: 0,
        }
    }

    /// 当前模式（栈顶）。栈空时默认 Synth
    pub fn current_mode(&self) -> Mode {
        self.mode_stack.last().cloned().unwrap_or(Mode::Synth)
    }

    /// 入栈：进入子节点 mode（Phase B/C 节点级 check 模式切换用）
    #[allow(dead_code)]
    pub(crate) fn push_mode(&mut self, m: Mode) {
        self.mode_stack.push(m);
    }

    /// 出栈：恢复父节点 mode
    #[allow(dead_code)]
    pub(crate) fn pop_mode(&mut self) {
        self.mode_stack.pop();
    }

    /// v0.75.86: check 模式入口 —— 在 `expected` 类型下验证 witness。
    ///
    /// 成功 → Ok(synthesized_type)（仍调 HM 推类型供后续约束用）
    /// 失败 → 标记已诊断 + Err(TypeError)
    pub fn check_against(&mut self, w: &MirWitness, expected: &Type) -> Result<Type, TypeError> {
        self.nodes_visited += 1;
        // 简易 check：调 HM 推类型 + subtype 验证
        let synth_ty = self.hm.infer_expr(w).map_err(|errs| {
            // HM 内部错误：转顶层 TypeError 兜底
            let _msg = errs
                .into_iter()
                .next()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown HM error".to_string());
            TypeError::new(w.span.line, format!("type inference failed: {}", _msg))
        })?;
        if !synth_ty.subtype_of(expected) {
            // 标记此节点已诊断 —— 防止 HM 跑完后报重复错误
            self.hm.mark_diagnosed(w);
            return Err(format_mismatch_error(&synth_ty, expected, w.span));
        }
        Ok(synth_ty)
    }

    /// v0.75.86: synth 模式入口 —— 推导 witness 类型。
    ///
    /// 当前实现：直接转发到 HM（保留 HM 现有所有行为）。
    ///
    /// 返回 `Vec<hm::TypeError>`（HM 内部错误类型）—— 调用方负责
    /// 转换为顶层 [`TypeError`]。设计选择：双向模块**不**重新实现
    /// 错误转换逻辑，避免 `hm_to_external` 重复（AGENTS.md §6）。
    /// 顶层集成（[`crate::typeck::check_mir`]）用 `hm_to_external` 统一转换。
    pub fn synth(&mut self, w: &MirWitness) -> Result<Type, Vec<crate::typeck::hm::TypeError>> {
        self.nodes_visited += 1;
        self.hm.infer_expr(w)
    }

    /// v0.75.86: 双向预扫入口（Phase A 当前覆盖 1 个节点）
    ///
    /// 遍历 witness 树，在**关键节点**前置双向检查；其它节点
    /// 仍由 HM 在 `infer_program` 阶段处理（保留现有行为）。
    ///
    /// 当前覆盖：Lambda 参数的 `type_hint` 与 body inferred 类型
    /// 的**自反**验证（subtype 自反是 subtype 的最简形式）——
    /// Phase A 仅做 demo，Phase B/C 扩展更复杂节点。
    pub fn pre_check_program(&mut self, witnesses: &[MirWitness]) {
        for w in witnesses {
            self.pre_check_witness(w);
        }
    }

    /// 递归遍历 witness 树
    fn pre_check_witness(&mut self, w: &MirWitness) {
        self.nodes_visited += 1;
        // 关键节点：Closure 参数 type_hint 自反 check
        if let WitnessKind::Closure { params, body } = &w.kind {
            for p in params {
                if let Some(hint) = &p.type_hint {
                    // 标注的 type_hint 必须 subtype 自身（自反检查）
                    if !hint.subtype_of(hint) {
                        self.hm.mark_diagnosed(w);
                        self.errors.push(format_mismatch_error(hint, hint, w.span));
                    }
                }
            }
            // 递归 body（保守 synth）
            self.pre_check_witness(body);
        } else {
            // 通用递归：所有有子节点的 variant
            self.recurse_witness(w);
        }
    }

    /// 通用递归：处理所有 WitnessKind 的子节点
    fn recurse_witness(&mut self, w: &MirWitness) {
        match &w.kind {
            // 无子节点（terminal variants）
            WitnessKind::Literal(_)
            | WitnessKind::Variable(_)
            | WitnessKind::Break(_)
            | WitnessKind::Continue(_)
            | WitnessKind::Import(_)
            | WitnessKind::TypeAlias { .. }
            | WitnessKind::EnumDef { .. }
            | WitnessKind::StructDef { .. }
            | WitnessKind::MacroDef { .. }
            | WitnessKind::Sequence(_) => {}
            // 二元操作
            WitnessKind::Binary { left, right, .. } => {
                self.pre_check_witness(left);
                self.pre_check_witness(right);
            }
            // 其它变体：Binary 的 op field 是 common::BinaryOp（privately
            // re-exported 在 mir::expr），这里只递归子节点不构造。
            // Loop, While, Or, And — 1-2 children
            // 1-2 子节点
            WitnessKind::Call { callee: _, args } => {
                for a in args {
                    self.pre_check_witness(a);
                }
            }
            WitnessKind::MethodCall { receiver, args, .. } => {
                self.pre_check_witness(receiver);
                for a in args {
                    self.pre_check_witness(a);
                }
            }
            WitnessKind::Closure { body, .. } | WitnessKind::FnDef { body, .. } => {
                self.pre_check_witness(body);
            }
            WitnessKind::Match { scrutinee, arms } => {
                self.pre_check_witness(scrutinee);
                for a in arms {
                    self.pre_check_witness(&a.body);
                    if let Some(g) = &a.guard {
                        self.pre_check_witness(g);
                    }
                }
            }
            WitnessKind::If { cond, then, r#else } => {
                self.pre_check_witness(cond);
                self.pre_check_witness(then);
                if let Some(e) = r#else {
                    self.pre_check_witness(e);
                }
            }
            WitnessKind::List(items) => {
                for i in items {
                    self.pre_check_witness(i);
                }
            }
            WitnessKind::Dict(entries) => {
                for (_k, v) in entries {
                    self.pre_check_witness(v);
                }
            }
            WitnessKind::LetBinding {
                value, init_body, ..
            } => {
                self.pre_check_witness(value);
                self.pre_check_witness(init_body);
            }
            WitnessKind::Assign { target: _, value } => {
                self.pre_check_witness(value);
            }
            WitnessKind::Loop { body, .. } | WitnessKind::While { body, .. } => {
                self.pre_check_witness(body);
            }
            WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
                self.pre_check_witness(left);
                self.pre_check_witness(right);
            }
            WitnessKind::Return(Some(e)) => {
                self.pre_check_witness(e);
            }
            WitnessKind::Return(None) => {}
            WitnessKind::DynTrait { expr, .. } => {
                self.pre_check_witness(expr);
            }
            WitnessKind::Prompt { parts } => {
                for p in parts {
                    self.pre_check_witness(p);
                }
            }
            WitnessKind::Orchestrate { kind, .. } => {
                // kind 是 Box<WitnessOrchestrateKind>，递归 input_var/result_var
                // 引用 witness 树这里不可见——保守跳过
                let _ = kind;
            }
            WitnessKind::IndexAssign {
                object,
                index,
                value,
            } => {
                self.pre_check_witness(object);
                self.pre_check_witness(index);
                self.pre_check_witness(value);
            }
        }
    }
}

// ─── 内部辅助 ───

/// v0.75.86: 格式化 subtype 失配错误。
///
/// 返顶层公开 [`TypeError`]（与 [`crate::typeck::hm::TypeError`] 不同——
/// `hm::TypeError` 是 HM 内部枚举，hm_to_external 决定如何转换）。
pub fn format_mismatch_error(actual: &Type, expected: &Type, span: Span) -> TypeError {
    let message = format!(
        "type mismatch: expected `{:?}`, got `{:?}`",
        expected, actual
    );
    let mut e = TypeError::new(span.line, message);
    e.column = span.column;
    e.expected = Some(format!("{:?}", expected));
    e.actual = Some(format!("{:?}", actual));
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{BinaryOp, Literal, Span};
    use crate::mir::witness::{MirWitness, WitnessKind, WitnessParam};
    // 注意：mir::expr::BinaryOp 是私有 re-export，测试用 common::BinaryOp

    fn lit_witness(n: i64, line: usize, col: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::new(0, 0))),
            span: Span::new(line, col),
        }
    }

    fn closure_witness_with_hint(
        param_name: &str,
        param_hint: Type,
        body: MirWitness,
    ) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Closure {
                params: vec![WitnessParam {
                    name: param_name.to_string(),
                    type_hint: Some(param_hint),
                    default: None,
                }],
                body: Box::new(body),
            },
            span: Span::new(1, 0),
        }
    }

    #[test]
    fn mode_default_is_synth() {
        let mut hm = HMInference::new();
        let checker = BidirectionalChecker::new(&mut hm);
        match checker.current_mode() {
            Mode::Synth => {}
            Mode::Check(_) => panic!("default mode should be Synth"),
        }
    }

    #[test]
    fn mode_stack_push_pop() {
        let mut hm = HMInference::new();
        let mut checker = BidirectionalChecker::new(&mut hm);
        checker.push_mode(Mode::Check(Type::Int));
        assert!(matches!(checker.current_mode(), Mode::Check(Type::Int)));
        checker.pop_mode();
        assert!(matches!(checker.current_mode(), Mode::Synth));
    }

    #[test]
    fn check_against_int_literal_succeeds() {
        let mut hm = HMInference::new();
        let mut checker = BidirectionalChecker::new(&mut hm);
        let w = lit_witness(42, 1, 0);
        // Int <: Int (自反) — 应成功
        let result = checker.check_against(&w, &Type::Int);
        assert!(result.is_ok());
    }

    #[test]
    fn check_against_int_literal_vs_float_fails() {
        let mut hm = HMInference::new();
        let mut checker = BidirectionalChecker::new(&mut hm);
        let w = lit_witness(42, 1, 0);
        let result = checker.check_against(&w, &Type::Float);
        assert!(result.is_err());
    }

    #[test]
    fn check_against_marks_diagnosed() {
        let mut hm = HMInference::new();
        let mut checker = BidirectionalChecker::new(&mut hm);
        let w = lit_witness(42, 1, 0);
        // check 失败应标记
        let _ = checker.check_against(&w, &Type::Float);
        assert!(hm.is_diagnosed(&w));
    }

    #[test]
    fn pre_check_program_visits_closure() {
        let mut hm = HMInference::new();
        let mut checker = BidirectionalChecker::new(&mut hm);
        // Lambda 带 type_hint: Int
        let w = closure_witness_with_hint("x", Type::Int, lit_witness(42, 2, 4));
        checker.pre_check_program(&[w]);
        // 预扫访问了 closure + body = 2 个节点
        assert!(checker.nodes_visited >= 2);
    }

    #[test]
    fn synth_forwards_to_hm() {
        let mut hm = HMInference::new();
        let mut checker = BidirectionalChecker::new(&mut hm);
        let w = lit_witness(42, 1, 0);
        // synth 应该成功（HM infer_expr 处理 Literal）
        let result = checker.synth(&w);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Type::Int);
    }

    #[test]
    fn recurse_visit_all_subnodes() {
        let mut hm = HMInference::new();
        let mut checker = BidirectionalChecker::new(&mut hm);
        // Binary(+, lit(1), lit(2)) — recurse 应访问 3 个节点
        let w = MirWitness {
            kind: WitnessKind::Binary {
                left: Box::new(lit_witness(1, 1, 0)),
                op: BinaryOp::Add,
                right: Box::new(lit_witness(2, 1, 4)),
            },
            span: Span::new(1, 0),
        };
        checker.pre_check_program(&[w]);
        // 至少 3 个节点（root + left + right）
        assert!(checker.nodes_visited >= 3);
    }
}
