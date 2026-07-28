//! v0.58: RewriteRule trait + 示例规则（Phase H.1c）
//!
//! 完全 MIR-native：规则从 `MirInst` 重写到 `Vec<MirInst>`，零 AST 依赖。

use crate::mir::MirInst;
use crate::mir::optimize::pattern::{Match, MatchBindings, MirPattern};

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
///
/// Phase H.2 扩展：新增 `RedundantJumpRule`（Phase H.2 实战示例）。
pub fn builtin_rules() -> Vec<Box<dyn RewriteRule>> {
    vec![
        Box::new(DeadAssignRule),
        Box::new(ConstFoldingRule),
        Box::new(RedundantJumpRule),
    ]
}

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
        if let MirInst::Jump(target) = inst {
            if *target == pc + 1 && *target < body.len() {
                // 是冗余 jump：跳转到下一条 → 删除
                return Vec::new();
            }
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

/// 示例规则 1：删除 `Assign(dst, src)` 当 dst 后续未被使用时
///
/// 注：完整 DCE 需要数据流分析。本规则作为接口演示。
pub struct DeadAssignRule;

impl RewriteRule for DeadAssignRule {
    fn name(&self) -> &'static str {
        "dead_assign"
    }

    fn pattern(&self) -> &MirPattern {
        // 此规则仅作接口演示；完整实现见 mir/opt.rs::dead_code_elim
        &DEAD_ASSIGN_PATTERN
    }

    fn rewrite(&self, _inst: &MirInst, _bindings: &MatchBindings) -> Vec<MirInst> {
        // 删除指令
        Vec::new()
    }

    fn cost_gain(&self) -> i32 {
        1
    }
}

static DEAD_ASSIGN_PATTERN: MirPattern = MirPattern::Copy {
    dst: crate::mir::optimize::pattern::RegMatcher::Any,
    src: crate::mir::optimize::pattern::RegMatcher::Any,
};

/// 示例规则 2：常量折叠
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

    fn rewrite(&self, inst: &MirInst, _bindings: &MatchBindings) -> Vec<MirInst> {
        // 完整实现：解析 inst 为 BinaryOp，提取常量并求值
        if let MirInst::BinaryOp(dst, _, op, _) = inst {
            // 此处需要 lhs/rhs 来自 bindings。简化版：直接返回原指令
            vec![MirInst::BinaryOp(
                *dst,
                0, // 占位 — 实际实现需从 bindings 提取
                op.clone(),
                0,
            )]
        } else {
            // 模式不匹配时保留原指令
            vec![inst.clone()]
        }
    }

    fn cost_gain(&self) -> i32 {
        1
    }
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
    fn test_dead_assign_rule_rewrites_copy_to_empty() {
        let rule = DeadAssignRule;
        let inst = MirInst::Copy(0, 1);
        let bindings = MatchBindings::new();
        let result = rule.rewrite(&inst, &bindings);
        assert!(result.is_empty(), "DeadAssignRule should delete the instruction");
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
        let body = vec![
            MirInst::Const(0, Value::Int(42)),
            MirInst::Jump(1_usize),
        ];
        let result = apply_rules(&body, &rules);
        // ConstFoldingRule 不匹配 Const → 保留
        // DeadAssignRule 不匹配 Const → 保留
        // DeadAssignRule 不匹配 Jump → 保留
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_builtin_rules_contains_three() {
        let rules = builtin_rules();
        assert_eq!(rules.len(), 3);
        let names: Vec<&str> = rules.iter().map(|r| r.name()).collect();
        assert!(names.contains(&"dead_assign"));
        assert!(names.contains(&"const_folding"));
        assert!(names.contains(&"redundant_jump"));
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
        let result =
            rule.rewrite_with_context(&MirInst::Jump(1), &MatchBindings::new(), 0, &body, &empty_ctx);
        assert!(result.is_empty(), "Jump to next instruction should be deleted");
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
        let result =
            rule.rewrite_with_context(&MirInst::Jump(2), &MatchBindings::new(), 0, &body, &empty_ctx);
        assert_eq!(result.len(), 1, "Real jump should be preserved");
    }

    #[test]
    fn test_redundant_jump_out_of_bounds_preserved() {
        let rule = RedundantJumpRule;
        // body[0] = Jump(5) 但 body 长度仅 2 → 越界 → 保留
        let body = vec![MirInst::Jump(5), MirInst::Const(0, Value::Int(42))];
        let empty_ctx = ();
        let result =
            rule.rewrite_with_context(&MirInst::Jump(5), &MatchBindings::new(), 0, &body, &empty_ctx);
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
}
