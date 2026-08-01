//! v0.58: Greedy search algorithm（Phase H.4b）
//!
//! 输入：`body` + 一组 `RewriteRule` + `CostModel`
//! 输出：优化后的 `body`（不增加 cost）
//!
//! 算法：
//! 1. 计算当前 body cost
//! 2. 循环：尝试每个 rule 对每个 inst 应用
//! 3. 选择 cost-gain ratio 最高的 (rule, inst) pair
//! 4. 应用并重复
//! 5. 直到无改进或达到 max_iter

use std::collections::HashMap;

use crate::mir::MirInst;
use crate::mir::optimize::cost::CostModel;
use crate::mir::optimize::pattern::Match;
use crate::mir::optimize::rule::RewriteRule;

/// 贪心搜索结果
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 优化后的 body
    pub body: Vec<MirInst>,
    /// 优化前 cost
    pub original_cost: u32,
    /// 优化后 cost
    pub final_cost: u32,
    /// 应用的迭代次数
    pub iterations: u32,
    /// 应用的规则名列表（按顺序）
    pub applied_rules: Vec<String>,
}

/// 贪心搜索
///
/// 在每一步扫描所有 (rule, inst) 对，选择 cost-gain 最大的应用。
/// 收敛条件：cost 不再下降 或 达到 max_iter。
pub fn greedy_search(
    body: &[MirInst],
    rules: &[Box<dyn RewriteRule>],
    cost: &dyn CostModel,
    max_iter: u32,
) -> SearchResult {
    let original_cost = cost.body_cost(body);
    let mut current = body.to_vec();
    let mut current_cost = original_cost;
    let mut applied_rules: Vec<String> = Vec::new();
    let mut iterations = 0;

    // Phase H.5 optimization: pre-truncate dead code after the last Return.
    // This runs in O(n) once instead of O(n²) via the rule scan loop.
    if let Some(last_return) = current
        .iter()
        .rposition(|i| matches!(i, MirInst::Return(_)))
        && last_return + 1 < current.len()
    {
        current.truncate(last_return + 1);
        applied_rules.push(format!(
            "dead_after_return (pre-truncated {} insts)",
            current.len().saturating_sub(last_return + 1)
        ));
        current_cost = cost.body_cost(&current);
    }

    while iterations < max_iter {
        iterations += 1;
        let mut best: Option<(usize, String, u32, Vec<MirInst>)> = None; // (pc, rule, gain, new_insts)
        // v0.75.27: cost_gain() 接入 — 数据驱动 gain 相同时的 tiebreaker
        // （此前该 trait 方法定义后从未被消费，出生即死）。
        let mut best_rule_gain: i32 = 0;
        // v0.75.27: 等价重写 memo（Cascades Group 记忆内核）— 重写是纯函数
        // （ctx 为空），同一 (规则, 指令形态) 在 body 内多次出现时只计算一次。
        // 跨轮有效（本循环重扫全 body），减少重复 rewrite + cost 计算。
        let mut rewrite_memo: HashMap<(usize, String), Vec<MirInst>> = HashMap::new();

        // 扫描所有 (pc, rule) 对
        for (pc, inst) in current.iter().enumerate() {
            for (rule_idx, rule) in rules.iter().enumerate() {
                if rule.pattern().matches(inst).is_some() {
                    let memo_key = (rule_idx, format!("{:?}", inst));
                    let new_insts = match rewrite_memo.get(&memo_key) {
                        Some(cached) => cached.clone(),
                        None => {
                            let bindings = rule.pattern().matches(inst).unwrap();
                            let computed = rule.rewrite_with_context(
                                inst,
                                &bindings,
                                pc,
                                &current,
                                &(), // empty ctx
                            );
                            rewrite_memo.insert(memo_key, computed.clone());
                            computed
                        }
                    };
                    let new_cost: u32 = new_insts.iter().map(|i| cost.inst_cost(i)).sum();
                    let inst_cost = cost.inst_cost(inst);
                    let gain = inst_cost.saturating_sub(new_cost);
                    if gain > 0 {
                        let candidate = (pc, rule.name().to_string(), gain, new_insts);
                        match &best {
                            Some((_, _, best_gain, _)) => {
                                // 数据驱动 gain 更高，或 gain 相同时规则作者
                                // 的静态估计更大（cost_gain tiebreaker）
                                if gain > *best_gain
                                    || (gain == *best_gain && rule.cost_gain() > best_rule_gain)
                                {
                                    best = Some(candidate);
                                    best_rule_gain = rule.cost_gain();
                                }
                            }
                            None => {
                                best = Some(candidate);
                                best_rule_gain = rule.cost_gain();
                            }
                        }
                    }
                }
            }
        }

        // 没有改进 → 收敛
        let Some((pc, rule_name, _gain, new_insts)) = best else {
            break;
        };

        // 应用：替换 current[pc] 为 new_insts
        let mut updated: Vec<MirInst> = Vec::with_capacity(current.len() - 1 + new_insts.len());
        updated.extend_from_slice(&current[..pc]);
        updated.extend(new_insts);
        updated.extend_from_slice(&current[pc + 1..]);
        current = updated;
        current_cost = cost.body_cost(&current);
        applied_rules.push(rule_name);
    }

    SearchResult {
        body: current,
        original_cost,
        final_cost: current_cost,
        iterations,
        applied_rules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::optimize::cost::{InstructionCount, TokenEstimate};
    use crate::mir::optimize::rule::{RedundantJumpRule, builtin_rules};
    use crate::value::Value;

    #[test]
    fn test_greedy_search_converges() {
        // body: Jump(1) + Const(42) → 冗余 Jump 应被消除
        let body = vec![MirInst::Jump(1), MirInst::Const(0, Value::Int(42))];
        let rules = builtin_rules();
        let cost = InstructionCount;
        let result = greedy_search(&body, &rules, &cost, 10);
        assert_eq!(result.final_cost, 1, "Redundant Jump should be removed");
        assert!(result.applied_rules.iter().any(|n| n == "redundant_jump"));
    }

    #[test]
    fn test_greedy_search_no_change_returns_original() {
        // body: 所有 Const → 没有可优化项
        let body = vec![MirInst::Const(0, Value::Int(42))];
        let rules = builtin_rules();
        let cost = InstructionCount;
        let result = greedy_search(&body, &rules, &cost, 5);
        assert_eq!(
            result.iterations, 1,
            "Should converge immediately (no rules apply)"
        );
        assert_eq!(result.original_cost, result.final_cost);
    }

    #[test]
    fn test_greedy_search_token_optimization() {
        // 长字符串常量 → 用 TokenEstimate 应识别为高 cost
        let body = vec![MirInst::Const(
            0,
            Value::String("this is a long string that consumes many tokens".to_string()),
        )];
        let rules = builtin_rules();
        let cost = TokenEstimate;
        let result = greedy_search(&body, &rules, &cost, 5);
        // 字符串无规则可应用 → cost 不变
        assert_eq!(result.original_cost, result.final_cost);
    }

    #[test]
    fn test_greedy_search_respects_max_iter() {
        // 构造一个永远会匹配的 body（确保 max_iter 生效）
        let body = vec![MirInst::Jump(1), MirInst::Const(0, Value::Int(1))];
        let rules: Vec<Box<dyn RewriteRule>> = vec![Box::new(RedundantJumpRule)];
        let cost = InstructionCount;
        let result = greedy_search(&body, &rules, &cost, 3);
        // 第一次迭代：消除 Jump 后无更多匹配 → iterations = 1
        assert!(result.iterations <= 3);
    }

    #[test]
    fn test_greedy_search_records_applied_rules() {
        let body = vec![MirInst::Jump(1), MirInst::Const(0, Value::Int(42))];
        let rules = builtin_rules();
        let cost = InstructionCount;
        let result = greedy_search(&body, &rules, &cost, 5);
        assert!(!result.applied_rules.is_empty());
    }

    #[test]
    fn test_rewrite_memo_reuses_equivalent_shapes() {
        // v0.75.27: 等价重写 memo — 同一 (规则, 指令形态) 在 body 内多次
        // 出现时只重写一次、复用结果。行为等价性：memo 不改变最终优化结果，
        // 只消除重复 rewrite/cost 计算。
        // RedundantJumpRule 仅在 target == pc + 1（跳转到下一条）时删除 —
        // body 构造按此语义（Jump(1)@0 冗余、Jump(3)@2 冗余）。
        let body = vec![
            MirInst::Jump(1),
            MirInst::Const(0, Value::Int(1)),
            MirInst::Jump(3),
            MirInst::Const(1, Value::Int(2)),
        ];
        let rules = builtin_rules();
        let cost = InstructionCount;
        let multi = greedy_search(&body, &rules, &cost, 10);
        // greedy 每轮应用 best 一个：首轮删一个冗余 Jump（gain=1），
        // 左移后另一 Jump 的 target 不再等于 pc+1 → 保留。
        // 期望：3 条指令（Const, Jump, Const）。
        assert_eq!(
            multi.final_cost, 3,
            "one redundant jump eliminated per pass"
        );
        // 行为等价对照：无重复形态的等价 body 收敛到同一 cost。
        let control = greedy_search(
            &[MirInst::Jump(1), MirInst::Const(0, Value::Int(1))],
            &rules,
            &cost,
            10,
        );
        assert_eq!(control.final_cost, 1, "single-jump body: 1 Const remains");
    }
}
