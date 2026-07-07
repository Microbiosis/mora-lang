//! v0.75.94: DiagFilter —— 双向定型（bidirectional）的「已诊断节点」跟踪器。
//!
//! ## 背景
//!
//! 架构审查报告（v0.75.90）标记「🔴 阻断级风险」：
//! HMInference 公共结构持有 `diagnosed: HashSet<WitnessNodeId>` 字段 +
//! 3 个方法（mark_diagnosed / is_diagnosed / is_diagnosed_at）。这是
//! 「双向定型专用基础设施」污染了「HM 公共 API」——未来 HM 重构必须
//! 保留 diagnosed 字段而无 typeck 内部业务需求。
//!
//! ## 设计
//!
//! DiagFilter 独立结构——持 `HashSet<WitnessNodeId>` + 3 个方法。
//! BidirectionalChecker 持 `DiagFilter`（不再借用 HMInference.diagnosed）。
//!
//! ## 收益
//!
//! HMInference 公共 API 不再有双向专用字段；DiagFilter 与 BidirectionalChecker
//! 共同生命周期——双向完成后 DiagFilter 即可释放，HMInference 可继续在
//! 后续多次 HM 检查间复用。
//!
//! ## 不变的
//!
//! - WitnessNodeId 仍是 `(line, column, kind_discriminant)` 三元组伪 ID
//! - mark_diagnosed / is_diagnosed / is_diagnosed_at 三个方法语义保持
//! - 双向 fallback 抑制机制行为不变

use std::collections::HashSet;

use crate::mir::witness::MirWitness;
use crate::mir::witness::WitnessKind;

/// v0.75.94: Witness 节点伪 ID —— (line, column, kind_discriminant)。
///
/// 详细设计见 [`DiagFilter`] 注释。MirWitness 公共结构未携带 NodeId 字段
/// （v0.55 MirWitness 重写时未继承），用 span + kind 三元组作伪 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WitnessNodeId {
    pub line: usize,
    pub column: usize,
    /// `WitnessKind` 的 discriminant 数字（`std::mem::discriminant`）
    /// ——同一位置不同 kind 视为不同节点
    pub kind: std::mem::Discriminant<WitnessKind>,
}

impl WitnessNodeId {
    /// 从 `MirWitness` 构造伪 ID
    pub fn from_witness(w: &MirWitness) -> Self {
        Self {
            line: w.span.line,
            column: w.span.column,
            kind: std::mem::discriminant(&w.kind),
        }
    }
    /// 从 (line, column, kind) 三元组构造（用于查表）
    pub fn from_parts(
        line: usize,
        column: usize,
        kind: std::mem::Discriminant<WitnessKind>,
    ) -> Self {
        Self { line, column, kind }
    }
}

/// v0.75.94: 双向定型专用「已诊断节点」跟踪器。
///
/// 替代 v0.75.86 起的 `HMInference::diagnosed` 字段：
/// - 持有 `HashSet<WitnessNodeId>`
/// - 提供 `mark_diagnosed` / `is_diagnosed` / `is_diagnosed_at` 三个方法
/// - 由 `BidirectionalChecker` 独占持有（HM 不感知）
///
/// 应用场景（v0.75.86 原始设计保留）：
/// ```ignore
/// // 双向 check 失败
/// if !actual.subtype_of(&expected) {
///     diag.mark_diagnosed(witness);  // 标记此节点已诊断
///     return Err(format_mismatch(actual, expected));
/// }
/// // HM 全跑后
/// errors.retain(|e| !diag.is_diagnosed_at(e.line, e.column, kind));
/// ```
#[derive(Debug, Default, Clone)]
pub struct DiagFilter {
    diagnosed: HashSet<WitnessNodeId>,
}

impl DiagFilter {
    /// 标记 witness 节点为「已诊断」—— 双向 fallback 抑制机制
    pub fn mark_diagnosed(&mut self, w: &MirWitness) {
        self.diagnosed.insert(WitnessNodeId::from_witness(w));
    }

    /// 查询 witness 节点是否已被标记为「已诊断」
    pub fn is_diagnosed(&self, w: &MirWitness) -> bool {
        self.diagnosed.contains(&WitnessNodeId::from_witness(w))
    }

    /// 便捷查询——给定 (line, column, kind_discriminant) 元组判断
    pub fn is_diagnosed_at(
        &self,
        line: usize,
        column: usize,
        kind: std::mem::Discriminant<WitnessKind>,
    ) -> bool {
        self.diagnosed
            .contains(&WitnessNodeId::from_parts(line, column, kind))
    }

    /// 便捷查询——任一 kind 是否已被诊断（仅按 (line, column) 过滤）。
    ///
    /// 用于 `hm_to_external` 过滤场景——`TypeError` 只有 line/column 字段，
    /// 缺失 `WitnessKind` discriminant；按 (line, column) 二元组宽松匹配
    /// 任何 kind 的诊断（Phase A 行为保留，详见 `check_program_witnesses_bidirectional` 注释）。
    pub fn is_diagnosed_at_line_column(&self, line: usize, column: usize) -> bool {
        self.diagnosed
            .iter()
            .any(|id| id.line == line && id.column == column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Literal, Span};
    use crate::mir::witness::{MirWitness, WitnessKind};

    fn lit_witness(line: usize, column: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(42, Span::new(line, column))),
            span: Span::new(line, column),
        }
    }

    fn var_witness(name: &str, line: usize, column: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::new(line, column),
        }
    }

    #[test]
    fn from_witness_dedups() {
        let w = lit_witness(1, 4);
        let id1 = WitnessNodeId::from_witness(&w);
        let id2 = WitnessNodeId::from_witness(&w);
        assert_eq!(id1, id2);
    }

    #[test]
    fn from_witness_distinguishes_kind() {
        let lit = lit_witness(1, 4);
        let var = MirWitness {
            kind: WitnessKind::Variable("x".to_string()),
            span: lit.span,
        };
        assert_ne!(
            WitnessNodeId::from_witness(&lit),
            WitnessNodeId::from_witness(&var),
        );
    }

    #[test]
    fn diagnosed_empty_set() {
        // 新建 DiagFilter 没有节点被诊断
        let diag = DiagFilter::default();
        let w = lit_witness(42, 0);
        assert!(!diag.is_diagnosed(&w));
    }

    #[test]
    fn diagnosed_mark_and_query() {
        let mut diag = DiagFilter::default();
        let w = lit_witness(42, 0);
        // 标记前 false
        assert!(!diag.is_diagnosed(&w));
        // 标记后 true
        diag.mark_diagnosed(&w);
        assert!(diag.is_diagnosed(&w));
        // 另一个未标记的 witness（不同行）仍 false
        let w2 = var_witness("y", 2, 0);
        assert!(!diag.is_diagnosed(&w2));
    }

    #[test]
    fn diagnosed_distinguishes_position() {
        // 同一表达式不同位置 → 视为不同节点
        let mut diag = DiagFilter::default();
        let w1 = var_witness("x", 1, 5);
        let w2 = var_witness("y", 2, 5); // 同样 kind（Variable）但 line 不同
        diag.mark_diagnosed(&w1);
        assert!(diag.is_diagnosed(&w1));
        assert!(!diag.is_diagnosed(&w2));
    }

    #[test]
    fn diagnosed_distinguishes_kind() {
        // 同一位置不同 kind → 视为不同节点
        let mut diag = DiagFilter::default();
        let lit = MirWitness {
            kind: WitnessKind::Literal(crate::common::Literal::Int(42, Span::default())),
            span: Span::new(1, 0),
        };
        let var = MirWitness {
            kind: WitnessKind::Variable("x".to_string()),
            span: Span::new(1, 0),
        };
        // 同一 line/column 不同 kind（Literal vs Variable）—— 应互不干扰
        diag.mark_diagnosed(&lit);
        assert!(diag.is_diagnosed(&lit));
        assert!(!diag.is_diagnosed(&var));
        diag.mark_diagnosed(&var);
        assert!(diag.is_diagnosed(&var));
    }

    #[test]
    fn diagnosed_at_query_works() {
        // is_diagnosed_at 便捷查询：给定 (line, column, kind_discriminant)
        let mut diag = DiagFilter::default();
        let w = var_witness("x", 5, 10);
        diag.mark_diagnosed(&w);
        let disc = std::mem::discriminant::<WitnessKind>(&w.kind);
        assert!(diag.is_diagnosed_at(5, 10, disc));
        // 不同 line → false
        assert!(!diag.is_diagnosed_at(6, 10, disc));
        // 不同 column → false
        assert!(!diag.is_diagnosed_at(5, 11, disc));
    }

    #[test]
    fn diagnosed_duplicate_mark_idempotent() {
        // 同一节点重复 mark → HashSet 保证幂等
        let mut diag = DiagFilter::default();
        let w = lit_witness(1, 0);
        diag.mark_diagnosed(&w);
        diag.mark_diagnosed(&w);
        diag.mark_diagnosed(&w);
        assert!(diag.is_diagnosed(&w));
    }

    #[test]
    fn diag_filter_is_diagnosed_at() {
        let mut diag = DiagFilter::default();
        let w = lit_witness(1, 4);
        diag.mark_diagnosed(&w);
        let id = WitnessNodeId::from_witness(&w);
        assert!(diag.is_diagnosed_at(id.line, id.column, id.kind));
    }

    #[test]
    fn diag_filter_is_diagnosed_at_line_column() {
        // v0.75.94: line/column 二元组宽松匹配（任何 kind）
        let mut diag = DiagFilter::default();
        let lit = lit_witness(1, 4);
        let _var = var_witness("x", 1, 4); // 同 line+col 不同 kind（is_diagnosed_at_line_column 仅检查 line+col）
        diag.mark_diagnosed(&lit);
        // 任一 kind 已被诊断 → true
        assert!(diag.is_diagnosed_at_line_column(1, 4));
        // 未诊断位置 → false
        assert!(!diag.is_diagnosed_at_line_column(2, 4));
        assert!(!diag.is_diagnosed_at_line_column(1, 5));
    }
}
