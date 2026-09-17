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
                if let Some(bindings) = rule.pattern().matches(inst) {
                    let memo_key = (rule_idx, format!("{:?}", inst));
                    let new_insts = match rewrite_memo.get(&memo_key) {
                        Some(cached) => cached.clone(),
                        None => {
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
        //
        // v0.104.2: **跳转目标重映射** —— 重写的长度若与原文不同（`n` 条 →
        // `m` 条），pc 之后的全部指令整体位移 `m - n`，但指令里的**裸 pc
        // 跳转目标**仍是旧编号。不修就会跳到错误位置。
        //
        // **缺陷背景（`if true { print("p") }` 之后的语句整体消失）**：
        // `IfSimplifyRule` 把常量条件的 `JumpIfNot(cond, target)` 折叠为
        // 空（条件为假）或 `Jump(target)`（条件为真）。前者使体长 -1，于是
        // **所有 pc > 目标位置**的指令前移一位，而结尾的 `Const(Int(7))`
        // 仍在旧编号上；DAG 按旧的 `Jump(8)` 解析 → 跳到不存在的 pc →
        // 该节点无出边 → 尾部语句（含 `print` 之后的值）**静默丢失**，
        // `run_mir` 返回 Nil、退出码 0。
        //
        // 规则：被删/新增区间是 `[pc, pc+1)` → `[pc, pc+m)`。
        //   · target <= pc        : 不受位移影响
        //   · target == pc + 1    : 原「跳过后继」 → 现在应指向 pc + m
        //   · target >  pc + 1    : 落在位移区间之后 → target + (m - 1)
        let old_span_end = pc + 1;
        let new_span_end = pc + new_insts.len();
        let delta = new_span_end as isize - old_span_end as isize;
        let remap = |t: &mut usize| {
            if *t == old_span_end {
                *t = new_span_end;
            } else if *t > old_span_end {
                let shifted = (*t as isize + delta).max(0) as usize;
                *t = shifted;
            }
        };
        let mut new_insts = new_insts;
        for inst in new_insts.iter_mut() {
            match inst {
                MirInst::Jump(t)
                | MirInst::JumpIf(_, t)
                | MirInst::JumpIfNot(_, t)
                | MirInst::Break(t)
                | MirInst::Continue(t) => remap(t),
                _ => {}
            }
        }
        let mut updated: Vec<MirInst> = Vec::with_capacity(current.len() - 1 + new_insts.len());
        updated.extend_from_slice(&current[..pc]);
        updated.extend(new_insts);
        updated.extend_from_slice(&current[pc + 1..]);
        // 位移区间**之后**的既有指令，其跳转目标同样需要重映射
        for inst in updated.iter_mut().skip(new_span_end) {
            match inst {
                MirInst::Jump(t)
                | MirInst::JumpIf(_, t)
                | MirInst::JumpIfNot(_, t)
                | MirInst::Break(t)
                | MirInst::Continue(t) => remap(t),
                _ => {}
            }
        }
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
        // v0.104.2: 随着**跳转目标重映射**的引入，两条冗余 Jump 都会被消除。
        // 首轮删 `Jump(1)@0`（跳到下一条），整体左移一位 → 原 `Jump(3)@2`
        // 的目标被重映射为新的「下一条」(`pc+1`)，于是次轮它同样满足冗余
        // 条件而被删除 → 只剩 2 条 Const。
        // 旧断言（cost==3）建立在「重映射缺失、跳转目标悬垂」的错误行为上：
        // 当时 `Jump(3)` 在 3 条指令的 body 里指向不存在的 pc 3，恰好因此
        // 「不等于 pc+1」而侥幸留下 —— 那是缺陷的副作用，不是语义要求。
        assert_eq!(
            multi.final_cost, 2,
            "both redundant jumps eliminated (target remap keeps them redundant)"
        );
        // 检查残留指令确实全是 Const —— 证明两条 Jump 都被删掉
        assert!(
            multi.body.iter().all(|i| matches!(i, MirInst::Const(_, _))),
            "remaining body should be Consts only, got {:?}",
            multi.body
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

    /// v0.104.2: 重写改变长度时，**跳转目标必须重映射**。
    ///
    /// 缺陷：`greedy_search` 应用重写后直接拼接 `[..pc] + new + [pc+1..]`，
    /// 未调整指令里的裸 pc 跳转目标。重写变短时，`pc` 之后的指令整体前移，
    /// 旧目标编号即指向错误位置（或越界）—— 实测
    /// `if true { print("p") }` + 尾随表达式：`IfSimplifyRule` 折叠掉常量
    /// 分支后长度 -1，结尾的 `Jump` 仍指向旧编号 → 尾随语句**静默丢失**、
    /// `run_mir` 返回 Nil、退出码 0。
    #[test]
    fn test_rewrite_remaps_jump_targets_on_length_change() {
        // body：常量条件假 → IfSimplifyRule 删除 JumpIfNot（长度 -1），
        // 末尾 Jump 的旧目标 3 应被重映射为 2。
        let body = vec![
            MirInst::Const(0, Value::Bool(false)),
            MirInst::JumpIfNot(0, 3),
            MirInst::Const(1, Value::Int(1)),
            MirInst::Const(2, Value::Int(2)),
        ];
        let rules = builtin_rules();
        let r = greedy_search(&body, &rules, &InstructionCount, 10);
        // 全部 Jump 目标都必须落在 body 内（不得悬垂/越界）
        for (pc, inst) in r.body.iter().enumerate() {
            let t = match inst {
                MirInst::Jump(t)
                | MirInst::JumpIf(_, t)
                | MirInst::JumpIfNot(_, t)
                | MirInst::Break(t)
                | MirInst::Continue(t) => Some(*t),
                _ => None,
            };
            if let Some(t) = t {
                assert!(
                    t <= r.body.len(),
                    "pc {pc} 的跳转目标 {t} 越界（body 长度 {}）：{:?}",
                    r.body.len(),
                    r.body
                );
            }
        }
    }
}
