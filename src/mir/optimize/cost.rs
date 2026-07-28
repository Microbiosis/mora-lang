//! v0.58: CostModel trait + 默认实现（Phase H.1d）
//!
//! Cost 抽象：
//! - 每条 `MirInst` 有自己的 cost（默认按指令类型）
//! - 整体 body cost = ∑ inst_cost
//! - AI 场景可实现 `TokenEstimate` cost model（Phase H.3 扩展）

use crate::mir::MirInst;
use crate::value::Value;

/// Cost model trait
///
/// 实现者可定义自定义 cost（如 token 消耗、内存、CPU 时间）。
pub trait CostModel {
    /// 单条指令 cost
    fn inst_cost(&self, inst: &MirInst) -> u32;

    /// 整体 body cost
    fn body_cost(&self, body: &[MirInst]) -> u32 {
        body.iter().map(|i| self.inst_cost(i)).sum()
    }
}

/// 默认实现：每条指令 cost = 1
pub struct InstructionCount;

impl CostModel for InstructionCount {
    fn inst_cost(&self, _inst: &MirInst) -> u32 {
        1
    }
}

/// AI token 估算（v0.58 Phase H.4 增强）
///
/// 关键设计：token 消耗不仅来自指令类别，还来自：
/// - 值的内容（String 长度、List 元素数）
/// - 参数个数（Call/MethodCall 携带 args）
/// - 嵌套深度（递归 Prompt 模板）
///
/// 这是 mora-lang AI 工作流优化的核心 — 「预计 token 消耗」驱动
/// 整个 CostModel 的语义。
pub struct TokenEstimate;

impl TokenEstimate {
    /// 估算 Value 的 token 消耗（粗略近似：1 token ≈ 4 字符）
    pub fn value_cost(&self, value: &Value) -> u32 {
        match value {
            Value::Nil | Value::Bool(_) => 1,
            Value::Int(n) => Self::digits(n.unsigned_abs()),
            Value::Float(f) => Self::digits(*f as u64) + 1, // +1 for decimal point
            Value::String(s) => (s.chars().count() / 4 + 1) as u32,
            Value::List(items) => {
                // 列表：2 (括号) + sum(items) + 1 per separator
                2 + items
                    .iter()
                    .map(|v| self.value_cost(v))
                    .sum::<u32>()
                    + items.len().saturating_sub(1) as u32
            }
            Value::Dict(pairs) => {
                // 字典：2 (大括号) + sum + 1 per pair separator
                let kv_sum: u32 = pairs
                    .iter()
                    .map(|(k, v)| (k.chars().count() / 4 + 1) as u32 + self.value_cost(v))
                    .sum();
                2 + kv_sum + pairs.len().saturating_sub(1) as u32
            }
            _ => 1, // 未知类型：保守估计
        }
    }

    /// 计算整数的十进制位数
    fn digits(n: u64) -> u32 {
        if n == 0 {
            1
        } else {
            (n as f64).log10().floor() as u32 + 1
        }
    }
}

impl CostModel for TokenEstimate {
    fn inst_cost(&self, inst: &MirInst) -> u32 {
        match inst {
            // 基础值指令：值本身的 token 成本
            MirInst::Const(_, v) => self.value_cost(v),
            MirInst::Var(_, _) => 1,
            // 二元运算：常量输入 → 可折叠为 Const
            MirInst::BinaryOp(_, _, _, _) => 3, // 折叠前 3 token，折叠后 1 token
            // 函数调用：name + args 的 token 总和
            MirInst::Call(_, name, args) => {
                let name_cost = (name.chars().count() / 4 + 1) as u32;
                name_cost + args.len() as u32 + 50 // 50 = 调用框架开销
            }
            // MethodCall: 同样按 args 计数
            MirInst::MethodCall(_, _, _, args) => args.len() as u32 + 60,
            // Prompt: 每个 part 累加
            MirInst::Prompt(_, parts) => {
                30 + parts.len() as u32 * 20 // 30 setup + 20 per part
            }
            // List/Dict 字面量
            MirInst::ListLit(_, items) => 2 + items.len() as u32,
            MirInst::DictLit(_, entries) => 2 + entries.len() as u32,
            // 控制流：基本
            MirInst::Return(_) => 1,
            MirInst::Jump(_) | MirInst::JumpIf(_, _) | MirInst::JumpIfNot(_, _) => 1,
            // 编排：50 + agents
            MirInst::Orchestrate { kind, .. } => {
                let n_agents = match kind.as_ref() {
                    crate::mir::expr::MirOrchestrateKind::Sequential { agents } => agents.len(),
                    crate::mir::expr::MirOrchestrateKind::Loop { agents, .. } => agents.len(),
                    crate::mir::expr::MirOrchestrateKind::Graph { agents, .. } => agents.len(),
                    crate::mir::expr::MirOrchestrateKind::Pregel { agents, .. } => agents.len(),
                };
                50 + n_agents as u32 * 30
            }
            // 任务/工具/Skill 定义不计
            MirInst::TaskDef { .. } => 0,
            MirInst::TraitDef { .. } => 0,
            MirInst::ImplDef { .. } => 0,
            MirInst::SkillDef { .. } => 0,
            MirInst::ToolDef { .. } => 0,
            MirInst::Closure { .. } => 0,
            // 其他未分类
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    #[test]
    fn test_instruction_count_uniform() {
        let cost = InstructionCount;
        assert_eq!(cost.inst_cost(&MirInst::Const(0, Value::Int(42))), 1);
        assert_eq!(cost.inst_cost(&MirInst::Jump(0)), 1);
    }

    #[test]
    fn test_body_cost_sum() {
        let cost = InstructionCount;
        let body = vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(2)),
            MirInst::Const(2, Value::Int(3)),
        ];
        assert_eq!(cost.body_cost(&body), 3);
    }

    #[test]
    fn test_token_estimate_call_expensive() {
        let cost = TokenEstimate;
        let call_cost = cost.inst_cost(&MirInst::Call(0, "test".to_string(), vec![]));
        let const_cost = cost.inst_cost(&MirInst::Const(0, Value::Int(1)));
        assert!(call_cost > const_cost);
    }

    // ========== Phase H.4 测试：值内容 token 估算 ==========

    #[test]
    fn test_value_cost_string_length() {
        let cost = TokenEstimate;
        // 短字符串（≤4 字符 → 1 token）
        assert_eq!(cost.value_cost(&Value::String("hi".to_string())), 1);
        // 4 字符 → 1 + 1 = 2 tokens
        assert_eq!(cost.value_cost(&Value::String("1234".to_string())), 2);
        // 8 字符 → 2 + 1 = 3 tokens
        assert_eq!(cost.value_cost(&Value::String("12345678".to_string())), 3);
    }

    #[test]
    fn test_value_cost_int_digits() {
        let cost = TokenEstimate;
        assert_eq!(cost.value_cost(&Value::Int(0)), 1);
        assert_eq!(cost.value_cost(&Value::Int(42)), 2);
        assert_eq!(cost.value_cost(&Value::Int(12345)), 5);
    }

    #[test]
    fn test_value_cost_list_sum() {
        let cost = TokenEstimate;
        let list = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        // [1, 2, 3] → 2 (括号) + 1+1+1 (每个 Int 1 token) + 2 (分隔符) = 7
        assert_eq!(cost.value_cost(&list), 7);
    }

    #[test]
    fn test_inst_cost_string_constant() {
        let cost = TokenEstimate;
        // "hello world" → 3 tokens
        let inst = MirInst::Const(0, Value::String("hello world".to_string()));
        assert_eq!(cost.inst_cost(&inst), 3);
    }

    #[test]
    fn test_inst_cost_orchestrate_scales_with_agents() {
        let cost = TokenEstimate;
        use crate::mir::expr::{MirAgentDef, MirOrchestrateKind};

        let make_agents = |n: usize| -> Vec<MirAgentDef> {
            (0..n)
                .map(|i| MirAgentDef {
                    name: format!("a{}", i),
                    task_mir_expr: None,
                    verify_mir_expr: None,
                    with_config: None,
                    reads_channels: Vec::new(),
                    task_body: crate::mir::MirFunction {
                        params: Vec::new(),
                        body: Vec::new(),
                        n_regs: 0,
                    },
                    verify_body: None,
                })
                .collect()
        };

        let small = MirInst::Orchestrate {
            input_var: "x".to_string(),
            result_var: "y".to_string(),
            kind: Box::new(MirOrchestrateKind::Sequential {
                agents: make_agents(1),
            }),
        };
        let large = MirInst::Orchestrate {
            input_var: "x".to_string(),
            result_var: "y".to_string(),
            kind: Box::new(MirOrchestrateKind::Sequential {
                agents: make_agents(5),
            }),
        };
        assert!(cost.inst_cost(&large) > cost.inst_cost(&small));
    }
}
