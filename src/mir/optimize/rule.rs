//! v0.58: RewriteRule trait + 示例规则（Phase H.1c）
//!
//! 完全 MIR-native：规则从 `MirInst` 重写到 `Vec<MirInst>`，零 AST 依赖。

use crate::mir::optimize::pattern::{Match, MatchBindings, MirPattern};
use crate::mir::{MirInst, Reg};
use crate::value::Value;

/// 重写规则 trait
///
/// 每条规则：
/// 1. 用 `pattern()` 描述要匹配的指令结构
/// 2. `rewrite()` 给出匹配后生成的新指令序列（可空 = 删除）
/// 3. `cost_gain()` 评估是否值得重写（cost(before) - cost(after) > gain_threshold）
///
/// Phase H.2 扩展：需要 body 上下文（pc、body 长度）的规则可重写 `rewrite_with_context`
pub trait RewriteRule {
    /// 规则名（用于日志/调试）
    fn name(&self) -> &'static str;

    /// 匹配模式
    fn pattern(&self) -> &MirPattern;

    /// 重写：给定匹配的指令和绑定，返回新的指令序列
    /// - 空 Vec 表示「删除该指令」
    /// - `vec![inst]` 表示「替换为单条指令」
    /// - `vec![a, b]` 表示「展开为多条指令」
    ///
    /// 默认实现：直接调用 `rewrite_with_context` 并忽略上下文。
    fn rewrite(&self, inst: &MirInst, bindings: &MatchBindings) -> Vec<MirInst> {
        let empty_ctx = ();
        self.rewrite_with_context(inst, bindings, 0, &[], &empty_ctx)
    }

    /// 带上下文的重写（Phase H.2 扩展）
    ///
    /// 参数：
    /// - `inst`：匹配的指令
    /// - `bindings`：Pattern 提取的变量绑定
    /// - `pc`：当前指令在 body 中的位置
    /// - `body`：完整 body 引用（用于 pc+1 / 上下文查找）
    /// - `_ctx`：未来可扩展的 dataflow 上下文（v0.1 占位）
    fn rewrite_with_context(
        &self,
        inst: &MirInst,
        bindings: &MatchBindings,
        _pc: usize,
        _body: &[MirInst],
        _ctx: &dyn std::any::Any,
    ) -> Vec<MirInst> {
        let _ = (inst, bindings);
        Vec::new()
    }

    /// 优化收益（正数 = 值得重写，负数 = 应跳过）
    ///
    /// 默认实现：固定收益 1.0。子规则可重写为基于 cost 评估。
    fn cost_gain(&self) -> i32 {
        1
    }
}

/// 规则库 — 集中管理所有规则
/// Phase H.6: If 简化 — 常量条件的 JumpIf/JumpIfNot 折叠
pub struct IfSimplifyRule;

impl RewriteRule for IfSimplifyRule {
    fn name(&self) -> &'static str {
        "if_simplify"
    }
    /// Use Any pattern — the rule handles both JumpIf and JumpIfNot internally
    fn pattern(&self) -> &MirPattern {
        &IF_SIMPLIFY_PATTERN
    }

    fn rewrite_with_context(
        &self,
        inst: &MirInst,
        _bindings: &MatchBindings,
        pc: usize,
        body: &[MirInst],
        _ctx: &dyn std::any::Any,
    ) -> Vec<MirInst> {
        let (cond_reg, target, is_not) = match inst {
            MirInst::JumpIf(cond, t) => (*cond, *t, false),
            MirInst::JumpIfNot(cond, t) => (*cond, *t, true),
            _ => return vec![inst.clone()],
        };
        let const_val = body[..pc].iter().rev().find_map(|prev| {
            if let MirInst::Const(r, Value::Bool(v)) = prev
                && *r == cond_reg
            {
                return Some(*v);
            }
            None
        });
        match const_val {
            Some(true) if !is_not => vec![MirInst::Jump(target)],
            Some(false) if !is_not => Vec::new(),
            Some(true) if is_not => Vec::new(),
            Some(false) if is_not => vec![MirInst::Jump(target)],
            _ => vec![inst.clone()],
        }
    }

    fn cost_gain(&self) -> i32 {
        2
    }
}

/// Matches any instruction — IfSimplifyRule handles JumpIf/JumpIfNot discrimination internally
static IF_SIMPLIFY_PATTERN: MirPattern = MirPattern::Any;

pub fn builtin_rules() -> Vec<Box<dyn RewriteRule>> {
    vec![
        Box::new(ConstFoldingRule),
        Box::new(RedundantJumpRule),
        Box::new(IfSimplifyRule),
        Box::new(DeadAfterReturnRule),
    ]
}

/// Phase H.5: 删除 Return 之后的所有死指令（安全网）
///
/// 主要工作在 `greedy_search` 中通过 O(n) 预截断完成。
/// 此规则作为安全网，处理后续优化可能引入的新 Return 后的代码。
///
/// 模式：任何指令（`_ => true`），但仅当 `pc > last_return_pc` 时重写
///
/// 收益：消除 N 条死指令（N = body.len() - return_idx - 1）
pub struct DeadAfterReturnRule;

impl RewriteRule for DeadAfterReturnRule {
    fn name(&self) -> &'static str {
        "dead_after_return"
    }

    fn pattern(&self) -> &MirPattern {
        // 通配模式 — 匹配任何指令
        &WILDCARD_PATTERN
    }

    fn rewrite_with_context(
        &self,
        _inst: &MirInst,
        _bindings: &MatchBindings,
        pc: usize,
        body: &[MirInst],
        _ctx: &dyn std::any::Any,
    ) -> Vec<MirInst> {
        // 找到最后一个 Return 的位置
        let last_return = body
            .iter()
            .enumerate()
            .rev()
            .find(|(_, inst)| matches!(inst, MirInst::Return(_)))
            .map(|(i, _)| i);

        // 如果当前指令在最后一个 Return 之后 → 删除
        if let Some(ret_idx) = last_return
            && pc > ret_idx
        {
            return Vec::new();
        }
        // 否则保留
        vec![_inst.clone()]
    }

    fn cost_gain(&self) -> i32 {
        1
    }
}

static WILDCARD_PATTERN: MirPattern = MirPattern::Any;

/// Phase H.2: 冗余 Jump 消除
///
/// 模式：`MirInst::Jump(target)` 其中 `target == pc + 1`（跳转到紧邻的下一条指令，是 noop）
///
/// 重写：删除该 Jump 指令
///
/// 收益：消除 1 条指令 + 1 次 PC 递增
pub struct RedundantJumpRule;

impl RewriteRule for RedundantJumpRule {
    fn name(&self) -> &'static str {
        "redundant_jump"
    }

    fn pattern(&self) -> &MirPattern {
        &REDUNDANT_JUMP_PATTERN
    }

    fn rewrite(&self, _inst: &MirInst, _bindings: &MatchBindings) -> Vec<MirInst> {
        // 默认 rewrite 不会被调用（rewrite_with_context 已处理）
        Vec::new()
    }

    fn rewrite_with_context(
        &self,
        inst: &MirInst,
        _bindings: &MatchBindings,
        pc: usize,
        body: &[MirInst],
        _ctx: &dyn std::any::Any,
    ) -> Vec<MirInst> {
        // 检查是否是 Jump(target) 且 target == pc + 1
        if let MirInst::Jump(target) = inst
            && *target == pc + 1
            && *target < body.len()
        {
            // 是冗余 jump：跳转到下一条 → 删除
            return Vec::new();
        }
        // 不匹配：保留原指令
        vec![inst.clone()]
    }

    fn cost_gain(&self) -> i32 {
        1
    }
}

static REDUNDANT_JUMP_PATTERN: MirPattern = MirPattern::Jump {
    target: crate::mir::optimize::pattern::LabelMatcher::Any,
};

/// Phase H.7: 常量折叠（修复版）
///
/// `BinaryOp(dst, a, op, b)` 其中 a 和 b 均为 `Const` → 折叠为 `Const(dst, eval(a,op,b))`
pub struct ConstFoldingRule;

impl RewriteRule for ConstFoldingRule {
    fn name(&self) -> &'static str {
        "const_folding"
    }

    fn pattern(&self) -> &MirPattern {
        &CONST_FOLDING_PATTERN
    }

    fn rewrite_with_context(
        &self,
        inst: &MirInst,
        _bindings: &MatchBindings,
        pc: usize,
        body: &[MirInst],
        _ctx: &dyn std::any::Any,
    ) -> Vec<MirInst> {
        if let MirInst::BinaryOp(dst, lhs, op, rhs) = inst {
            // v0.75.33: 归纳变量保护 — `i = i + 1`（dst 出现在自身输入，
            // loop-carried dependence）绝不能折叠：回溯能找到「最近 Const」
            // 只有初始化值（如 Const(i, 0)），折叠会把归纳变量变恒值，
            // 破坏循环语义。非自依赖（a + b，a/b 非本指令写回）才安全。
            if dst == lhs || dst == rhs {
                return vec![inst.clone()];
            }
            // Scan backwards for constant definitions of lhs and rhs
            let lhs_val = find_const_backward(body, *lhs, pc);
            let rhs_val = find_const_backward(body, *rhs, pc);
            if let (Some(lv), Some(rv)) = (lhs_val, rhs_val)
                && let Ok(result) = crate::flow::eval_binary(lv, op, rv)
            {
                return vec![MirInst::Const(*dst, result)];
            }
        }
        vec![inst.clone()]
    }

    fn cost_gain(&self) -> i32 {
        2
    }
}

/// Find the most recent `Const` instruction within the same basic block
/// that defines `reg`, scanning backwards from `before_pc`.
/// Find the most recent `Const` instruction within the same basic block
/// that defines `reg`, scanning backwards from `before_pc`.
///
/// v0.75.33 正确性修复：此前只回溯到 `Label` 边界并找 `Const`——但 for 循环
/// lowering **不插 Label**，回溯会穿过整个循环体找到循环前的 `Const(i, 0)`
/// 初始化，把 `i = i + 1` 错折成 `i = 1`（循环恒值 bug）。现在遇到**最近
/// 的定义点**（`inst.dst() == reg`）即停止：是 `Const` 才取，非 `Const`
/// （BinaryOp/Var 等重新定义）返回 None —— 前面的 Const 已失效。
fn find_const_backward(body: &[MirInst], reg: Reg, before_pc: usize) -> Option<Value> {
    for inst in body[..before_pc].iter().rev() {
        if matches!(inst, MirInst::Label(_)) {
            break;
        }
        if let Some(dst) = inst.dst()
            && dst == reg
        {
            // 最近的定义点：Const 才有效，否则该 reg 被重新定义、前面失效
            return if let MirInst::Const(_, val) = inst {
                Some(val.clone())
            } else {
                None
            };
        }
    }
    None
}

static CONST_FOLDING_PATTERN: MirPattern = MirPattern::BinaryOp {
    dst: crate::mir::optimize::pattern::RegMatcher::Any,
    op: crate::mir::optimize::pattern::OpMatcher::Any,
    lhs: crate::mir::optimize::pattern::RegMatcher::Any,
    rhs: crate::mir::optimize::pattern::RegMatcher::Any,
};

/// 在 `body` 上应用一组规则（v0.1 简化：单 pass 顺序应用）
pub fn apply_rules(body: &[MirInst], rules: &[Box<dyn RewriteRule>]) -> Vec<MirInst> {
    let empty_ctx = ();
    let mut out = Vec::with_capacity(body.len());
    for (pc, inst) in body.iter().enumerate() {
        let mut replaced = false;
        for rule in rules {
            if rule.pattern().matches(inst).is_some() {
                let bindings = rule.pattern().matches(inst).unwrap();
                let new_insts = rule.rewrite_with_context(inst, &bindings, pc, body, &empty_ctx);
                out.extend(new_insts);
                replaced = true;
                break;
            }
        }
        if !replaced {
            out.push(inst.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::mir::MirInst;
    use crate::value::Value;

    #[test]
    fn test_dead_assign_rule_removed() {
        // DeadAssignRule removed in v0.55 — MirInst::Copy no longer exists
        let rules = builtin_rules();
        assert!(!rules.is_empty(), "at least one rule should exist");
    }

    #[test]
    fn test_const_folding_rule_pattern_matches() {
        let rule = ConstFoldingRule;
        let inst = MirInst::BinaryOp(0, 1, BinaryOp::Add, 2);
        assert!(rule.pattern().matches(&inst).is_some());
    }

    #[test]
    fn test_const_folding_pattern_no_match_for_const() {
        let rule = ConstFoldingRule;
        let inst = MirInst::Const(0, Value::Int(42));
        assert!(rule.pattern().matches(&inst).is_none());
    }

    #[test]
    fn test_apply_rules_preserves_unmatched() {
        let rules = builtin_rules();
        let body = vec![MirInst::Const(0, Value::Int(42)), MirInst::Jump(1_usize)];
        let result = apply_rules(&body, &rules);
        // ConstFoldingRule 不匹配 Const → 保留
        // DeadAssignRule 不匹配 Const → 保留
        // DeadAssignRule 不匹配 Jump → 保留
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_builtin_rules_contains_three() {
        let rules = builtin_rules();
        assert_eq!(
            rules.len(),
            4,
            "const_folding + redundant_jump + if_simplify + dead_after_return"
        );
        let names: Vec<&str> = rules.iter().map(|r| r.name()).collect();
        assert!(names.contains(&"const_folding"));
        assert!(names.contains(&"redundant_jump"));
        assert!(names.contains(&"if_simplify"));
        assert!(names.contains(&"dead_after_return"));
    }

    // ========== RedundantJumpRule 测试（Phase H.2）==========

    #[test]
    fn test_redundant_jump_pattern_matches() {
        let rule = RedundantJumpRule;
        let inst = MirInst::Jump(0);
        assert!(rule.pattern().matches(&inst).is_some());
    }

    #[test]
    fn test_redundant_jump_deletes_self_loop() {
        let rule = RedundantJumpRule;
        // body[0] = Jump(1)，body[1] = ... → Jump(1) 是冗余
        let body = vec![MirInst::Jump(1), MirInst::Const(0, Value::Int(42))];
        let empty_ctx = ();
        let result = rule.rewrite_with_context(
            &MirInst::Jump(1),
            &MatchBindings::new(),
            0,
            &body,
            &empty_ctx,
        );
        assert!(
            result.is_empty(),
            "Jump to next instruction should be deleted"
        );
    }

    #[test]
    fn test_redundant_jump_keeps_real_jump() {
        let rule = RedundantJumpRule;
        // body[0] = Jump(2) → body[2] (跳过 body[1])
        let body = vec![
            MirInst::Jump(2),
            MirInst::Const(0, Value::Int(99)),
            MirInst::Const(1, Value::Int(42)),
        ];
        let empty_ctx = ();
        let result = rule.rewrite_with_context(
            &MirInst::Jump(2),
            &MatchBindings::new(),
            0,
            &body,
            &empty_ctx,
        );
        assert_eq!(result.len(), 1, "Real jump should be preserved");
    }

    #[test]
    fn test_redundant_jump_out_of_bounds_preserved() {
        let rule = RedundantJumpRule;
        // body[0] = Jump(5) 但 body 长度仅 2 → 越界 → 保留
        let body = vec![MirInst::Jump(5), MirInst::Const(0, Value::Int(42))];
        let empty_ctx = ();
        let result = rule.rewrite_with_context(
            &MirInst::Jump(5),
            &MatchBindings::new(),
            0,
            &body,
            &empty_ctx,
        );
        assert_eq!(result.len(), 1, "Out-of-bounds jump should be preserved");
    }

    #[test]
    fn test_apply_rules_removes_redundant_jump() {
        let rules = builtin_rules();
        // body[0] = Jump(1) 是冗余 → 应被删除
        let body = vec![MirInst::Jump(1), MirInst::Const(0, Value::Int(42))];
        let result = apply_rules(&body, &rules);
        assert_eq!(result.len(), 1, "Redundant jump should be removed");
        assert!(matches!(result[0], MirInst::Const(0, Value::Int(42))));
    }

    #[test]
    fn test_const_folding_folds_int_add() {
        let rule = ConstFoldingRule;
        let body = vec![
            MirInst::Const(1, Value::Int(10)),
            MirInst::Const(2, Value::Int(32)),
            MirInst::BinaryOp(3, 1, BinaryOp::Add, 2),
        ];
        let empty_ctx = ();
        let result = rule.rewrite_with_context(
            &MirInst::BinaryOp(3, 1, BinaryOp::Add, 2),
            &MatchBindings::new(),
            2, // pc of the BinaryOp
            &body,
            &empty_ctx,
        );
        assert_eq!(result.len(), 1, "should fold to single Const");
        assert!(
            matches!(&result[0], MirInst::Const(d, Value::Int(42)) if *d == 3),
            "should be Const(3, 42), got: {:?}",
            result[0]
        );
    }

    #[test]
    fn test_const_folding_no_fold_without_constants() {
        let rule = ConstFoldingRule;
        let body = vec![
            // No Const for r1 or r2 — can't fold
            MirInst::BinaryOp(3, 1, BinaryOp::Mul, 2),
        ];
        let empty_ctx = ();
        let result = rule.rewrite_with_context(
            &MirInst::BinaryOp(3, 1, BinaryOp::Mul, 2),
            &MatchBindings::new(),
            0,
            &body,
            &empty_ctx,
        );
        // Should preserve the original instruction unchanged
        assert_eq!(result.len(), 1);
        assert!(matches!(
            result[0],
            MirInst::BinaryOp(3, 1, BinaryOp::Mul, 2)
        ));
    }

    #[test]
    fn test_const_folding_block_boundary() {
        let rule = ConstFoldingRule;
        // Const for r1 is in a different basic block (after a Label)
        let body = vec![
            MirInst::Label(0),
            MirInst::Const(1, Value::Int(5)),
            MirInst::Label(3), // basic block boundary
            MirInst::BinaryOp(4, 1, BinaryOp::Add, 1),
        ];
        let empty_ctx = ();
        let result = rule.rewrite_with_context(
            &MirInst::BinaryOp(4, 1, BinaryOp::Add, 1),
            &MatchBindings::new(),
            3, // pc of the BinaryOp
            &body,
            &empty_ctx,
        );
        // Should NOT fold: Const(1) is behind a Label boundary
        // (but r1=1 uses the same register as rhs — should find rhs=1 resolves to same Const(1, Int(5)))
        // Actually: both lhs=1 and rhs=1 resolve to the same Const above Label(3) —
        // the find_const_backward stops at Label, so it WON'T find lhs/rhs.
        assert_eq!(result.len(), 1, "should not fold across block boundary");
    }

    #[test]
    fn test_const_folding_skips_induction_variable() {
        // v0.75.33: `i = i + 1`（dst == lhs，loop-carried）绝不能折叠 —
        // 回溯能找到最近 Const(i, 0) 初始化，折叠会把归纳变量变恒值。
        let rule = ConstFoldingRule;
        let empty_ctx = ();
        let body = vec![
            MirInst::Const(10, Value::Int(0)),            // i = 0 (init)
            MirInst::BinaryOp(10, 10, BinaryOp::Add, 12), // i = i + 1
        ];
        let result =
            rule.rewrite_with_context(&body[1], &MatchBindings::new(), 1, &body, &empty_ctx);
        assert_eq!(result.len(), 1, "归纳变量（dst==lhs）绝不能折叠");
        assert!(
            matches!(&result[0], MirInst::BinaryOp(..)),
            "应保留 BinaryOp，而非折叠成 Const"
        );
    }

    #[test]
    fn test_const_folding_stops_at_redefinition() {
        // v0.75.33: find_const_backward 遇到最近定义点（非 Const）即失效 —
        // reg 被重新定义后，更早的 Const 不再有效。
        let rule = ConstFoldingRule;
        let empty_ctx = ();
        let body = vec![
            MirInst::Const(5, Value::Int(100)),        // x = 100 (early)
            MirInst::BinaryOp(5, 5, BinaryOp::Add, 6), // x = x + y (redefine)
            MirInst::BinaryOp(7, 5, BinaryOp::Add, 8), // z = x + w — x 已被重定义
        ];
        // 对 pc=2 的 BinaryOp(7, 5, +, 8)：lhs=5 最近定义是 BinaryOp(5,...)
        // 非 Const → find_const_backward 返回 None → 不折叠
        let result =
            rule.rewrite_with_context(&body[2], &MatchBindings::new(), 2, &body, &empty_ctx);
        assert_eq!(result.len(), 1, "x 被重定义后不应折叠");
        assert!(matches!(&result[0], MirInst::BinaryOp(..)));
    }
}
