//! Worker 5: Pregel v0.50 类型检查扩展
//!
//! 本模块提供 Pregel 编排模式的类型检查逻辑，直接使用 ast_v2.rs 中
//! Worker 1 已定义的真实类型（OrchestrateKind::Pregel 及其子结构）。
//!
//! 检查项（对应 orchestrate_v50_implementation_plan.md §7）：
//! - state_schema 中 reducer 与 type_hint 的兼容性
//! - edge 引用的节点名称存在性
//! - checkpoint 配置的合法性（存储后端已知性）
//! - interrupt_points 引用的节点存在性
//! - Command 表达式中的 goto 目标节点存在性
//! - Send 表达式中的目标节点存在性
//! - 动态边的完整性（map 需配 reduce / fan_in）
//!
//! 设计约束：零 panic —— 所有路径收集错误到 `Vec<TypeError>`。

use std::collections::{HashMap, HashSet};

use crate::ast_v2::{
    CheckpointConfig, DynamicKind, InterruptPoint, OrchestrateAgent, OrchestrateEdge, ReducerKind,
    StateChannel,
};
use crate::typeck::TypeError;

// ===================================================================
// 已知常量
// ===================================================================

const KNOWN_SAVERS: &[&str] = &["memory", "sqlite", "redis", "postgres"];

// ===================================================================
// 辅助函数
// ===================================================================

fn is_list_type_hint(hint: &str) -> bool {
    let h = hint.trim().to_lowercase();
    h.starts_with('[') || h.contains("list<") || h.contains("list ") || h == "list"
}

fn is_number_type_hint(hint: &str) -> bool {
    let h = hint.trim().to_lowercase();
    h.contains("number") || h.contains("int") || h.contains("float") || h == "num"
}

// ===================================================================
// 核心类型检查：check_orchestrate_pregel
// ===================================================================

/// 检查 Pregel 编排声明的完整类型一致性。
///
/// 返回 `Vec<TypeError>`：多错误收集模式。空向量表示无错误/警告。
/// 所有错误（致命或非致命）统一收集，由调用方决定如何呈现。
///
/// 参数说明：
/// - `agents`: 图中声明的 agent 列表
/// - `edges`: 有向边列表
/// - `state_schema`: 状态通道 schema
/// - `checkpoint`: 可选检查点配置
/// - `interrupt_points`: 中断点列表
/// - `command_gotos`: 编译期收集到的 Command goto 目标 (node_name, line)
/// - `send_targets`: 编译期收集到的 Send 目标 (node_name, line)
pub fn check_orchestrate_pregel(
    agents: &[OrchestrateAgent],
    edges: &[OrchestrateEdge],
    state_schema: &[StateChannel],
    checkpoint: &Option<CheckpointConfig>,
    interrupt_points: &[InterruptPoint],
    command_gotos: &[(String, usize)],
    send_targets: &[(String, usize)],
) -> Vec<TypeError> {
    let mut errors = Vec::new();

    // 1. 收集所有已知 agent 名称（含 @start/@exit 伪节点）
    let mut agent_names: HashSet<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    agent_names.insert("@start");
    agent_names.insert("@exit");

    // 2. 检查 state_schema 类型一致性
    for ch in state_schema {
        check_state_channel(ch, &mut errors);
    }

    // 3. 检查 edge 引用的节点存在性
    for edge in edges {
        check_edge_ref(edge, &agent_names, &mut errors);
    }

    // 4. 检查 checkpoint 配置
    if let Some(cp) = checkpoint {
        check_checkpoint(cp, &mut errors);
    }

    // 5. 检查 interrupt_points 引用的节点
    for ip in interrupt_points {
        if !agent_names.contains(ip.node_name.as_str()) {
            errors.push(TypeError::with_detail(
                0,
                format!("Interrupt point references unknown node '{}'", ip.node_name),
                "agent declared in orchestrate block or @start/@exit",
                ip.node_name.clone(),
                "declare the agent or remove the interrupt point",
            ));
        }
    }

    // 6. 检查 Command goto 目标
    for (goto, line) in command_gotos {
        if !agent_names.contains(goto.as_str()) {
            errors.push(TypeError::with_detail(
                *line,
                format!("Command goto references unknown node '{}'", goto),
                "agent declared in orchestrate block or @start/@exit",
                goto.clone(),
                "declare the agent or use a valid node name",
            ));
        }
    }

    // 7. 检查 Send 目标
    for (target, line) in send_targets {
        if !agent_names.contains(target.as_str()) {
            errors.push(TypeError::with_detail(
                *line,
                format!("Send target references unknown node '{}'", target),
                "agent declared in orchestrate block or @start/@exit",
                target.clone(),
                "declare the agent or use a valid node name",
            ));
        }
    }

    // 8. 检查动态边的完整性（map 应有对应 reduce / fan_in）
    check_dynamic_edge_consistency(edges, &mut errors);

    // 9. 检查 agent 名称唯一性
    check_agent_name_uniqueness(agents, &mut errors);

    errors
}

// -------------------------------------------------------------------
// 子检查函数
// -------------------------------------------------------------------

fn check_state_channel(ch: &StateChannel, errors: &mut Vec<TypeError>) {
    if let Some(ref hint) = ch.type_hint {
        if hint.trim().is_empty() {
            errors.push(TypeError::new(
                0,
                format!("State channel '{}' has empty type hint", ch.name),
            ));
            return;
        }

        match ch.reducer {
            ReducerKind::Append => {
                if !is_list_type_hint(hint) {
                    errors.push(TypeError::with_detail(
                        0,
                        format!(
                            "State channel '{}' with @append requires list type, got '{}'",
                            ch.name, hint
                        ),
                        "list type (e.g. [T] or List<T>)",
                        hint.clone(),
                        "change type hint to a list type or use @last/@add reducer",
                    ));
                }
            }
            ReducerKind::Add => {
                if !is_number_type_hint(hint) {
                    errors.push(TypeError::with_detail(
                        0,
                        format!(
                            "State channel '{}' with @add requires number type, got '{}'",
                            ch.name, hint
                        ),
                        "number / int / float type",
                        hint.clone(),
                        "change type hint to a number type or use @last/@append reducer",
                    ));
                }
            }
            ReducerKind::Merge(_fn_id) => {
                // Merge 的 NodeId 指向合并函数，在类型检查层面仅确认存在即可。
                // 深层函数签名检查留给后续 passes（或运行时）。
            }
            ReducerKind::Last => {
                // 默认 reducer，任何类型均可，不检查
            }
        }
    }
}

fn check_edge_ref(
    edge: &OrchestrateEdge,
    agent_names: &HashSet<&str>,
    errors: &mut Vec<TypeError>,
) {
    // v0.50: 动态边的一端引用运行时生成的节点，不需要静态声明：
    // - Map/FanOut: from 是静态节点（任务生成器），to 是动态节点（不需要静态声明）
    // - Reduce/FanIn: from 是动态节点（不需要静态声明），to 是静态节点（聚合器）
    let skip_from = matches!(
        edge.dynamic,
        Some(DynamicKind::Reduce) | Some(DynamicKind::FanIn)
    );
    let skip_to = matches!(
        edge.dynamic,
        Some(DynamicKind::Map) | Some(DynamicKind::FanOut)
    );

    if !skip_from && !agent_names.contains(edge.from.as_str()) && edge.from != "@start" {
        errors.push(TypeError::with_detail(
            0,
            format!("Edge from unknown node '{}'", edge.from),
            "agent declared in orchestrate block or @start",
            edge.from.clone(),
            "declare the agent or use @start as source",
        ));
    }
    if !skip_to && !agent_names.contains(edge.to.as_str()) && edge.to != "@exit" {
        errors.push(TypeError::with_detail(
            0,
            format!("Edge to unknown node '{}'", edge.to),
            "agent declared in orchestrate block or @exit",
            edge.to.clone(),
            "declare the agent or use @exit as target",
        ));
    }
}

fn check_checkpoint(cp: &CheckpointConfig, errors: &mut Vec<TypeError>) {
    if !KNOWN_SAVERS.contains(&cp.saver.as_str()) {
        errors.push(TypeError::with_detail(
            0,
            format!("Unknown checkpoint saver '{}'", cp.saver),
            format!("one of: {}", KNOWN_SAVERS.join(", ")),
            cp.saver.clone(),
            "use a supported saver backend",
        ));
    }
}

fn check_dynamic_edge_consistency(edges: &[OrchestrateEdge], errors: &mut Vec<TypeError>) {
    let mut map_seen = false;
    let mut reduce_or_fan_in_seen = false;

    for edge in edges {
        match edge.dynamic {
            Some(DynamicKind::Map) | Some(DynamicKind::FanOut) => map_seen = true,
            Some(DynamicKind::Reduce) | Some(DynamicKind::FanIn) => reduce_or_fan_in_seen = true,
            None => {}
        }
    }

    // 动态展开边缺少汇聚时发出警告（非致命）
    if map_seen && !reduce_or_fan_in_seen {
        errors.push(TypeError::new(
            0,
            "Dynamic edge 'map' or 'fan_out' detected without corresponding 'reduce' \
             or 'fan_in' target. This may lead to uncollected parallel tasks.",
        ));
    }
}

fn check_agent_name_uniqueness(agents: &[OrchestrateAgent], errors: &mut Vec<TypeError>) {
    let mut seen = HashMap::new();
    for agent in agents {
        if seen.insert(agent.name.clone(), ()).is_some() {
            errors.push(TypeError::new(
                0,
                format!("Duplicate agent name '{}' in orchestrate block", agent.name),
            ));
        }
    }
}

// ===================================================================
// 兼容接口：为 check.rs 的 ExprKind::Command / Send 预留
// 当 check_expr 直接处理这些 variant 后，以下方法可逐步移除。
// ===================================================================

/// v0.50 预留：Command 表达式 `{ goto: ..., update: ..., resume: ... }` 类型检查。
/// 检查 update 值表达式和 resume 表达式，返回占位类型。
///
/// 当前 `check_expr` 已直接处理 `ExprKind::Command`，此方法保留供外部调用者使用。
pub fn check_command_placeholder(
    _goto: &Option<String>,
    update: &[(String, crate::ast_v2::NodeId)],
    resume: &Option<crate::ast_v2::NodeId>,
    type_checker: &mut crate::typeck::TypeChecker,
    arena: &crate::ast_v2::AstArena,
    symbols: &crate::typeck::SymbolTable,
) -> crate::typeck::Type {
    for (_, expr_id) in update {
        type_checker.check_expr(*expr_id, arena, symbols);
    }
    if let Some(resume_id) = resume {
        type_checker.check_expr(*resume_id, arena, symbols);
    }
    crate::typeck::Type::Union(vec![])
}

/// v0.50 预留：Send 表达式 `send("node", { ... })` 类型检查。
/// 检查 input 表达式，返回占位类型。
///
/// 当前 `check_expr` 已直接处理 `ExprKind::Send`，此方法保留供外部调用者使用。
pub fn check_send_placeholder(
    _target: &str,
    input: crate::ast_v2::NodeId,
    type_checker: &mut crate::typeck::TypeChecker,
    arena: &crate::ast_v2::AstArena,
    symbols: &crate::typeck::SymbolTable,
) -> crate::typeck::Type {
    type_checker.check_expr(input, arena, symbols);
    crate::typeck::Type::Union(vec![])
}

// ===================================================================
// 单元测试
// ===================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast_v2::{InterruptWhen, NodeId};
    use crate::typeck::TypeError;

    fn no_errors(result: Vec<TypeError>) -> Vec<TypeError> {
        assert!(result.is_empty(), "expected no errors, got: {:?}", result);
        result
    }

    fn has_error(result: &[TypeError], pat: &str) -> bool {
        result.iter().any(|e| e.message.contains(pat))
    }

    #[test]
    fn test_valid_pregel_no_errors() {
        let agents = vec![
            OrchestrateAgent {
                name: "classifier".into(),
                with_config: None,
                task_expr: NodeId(0),
                verify_expr: None,
            },
            OrchestrateAgent {
                name: "handler_urgent".into(),
                with_config: None,
                task_expr: NodeId(0),
                verify_expr: None,
            },
            OrchestrateAgent {
                name: "handler_normal".into(),
                with_config: None,
                task_expr: NodeId(0),
                verify_expr: None,
            },
        ];
        let edges = vec![
            OrchestrateEdge {
                from: "@start".into(),
                to: "classifier".into(),
                condition: None,
                dynamic: None,
            },
            OrchestrateEdge {
                from: "classifier".into(),
                to: "handler_urgent".into(),
                condition: Some(NodeId(0)),
                dynamic: None,
            },
            OrchestrateEdge {
                from: "classifier".into(),
                to: "handler_normal".into(),
                condition: Some(NodeId(0)),
                dynamic: None,
            },
        ];
        let state = vec![
            StateChannel {
                name: "messages".into(),
                type_hint: Some("[Message]".into()),
                reducer: ReducerKind::Append,
            },
            StateChannel {
                name: "total_cost".into(),
                type_hint: Some("number".into()),
                reducer: ReducerKind::Add,
            },
            StateChannel {
                name: "last_decision".into(),
                type_hint: Some("string".into()),
                reducer: ReducerKind::Last,
            },
        ];
        let cp = Some(CheckpointConfig {
            saver: "memory".into(),
            thread_id: Some(NodeId(0)),
        });
        let interrupts = vec![InterruptPoint {
            node_name: "handler_urgent".into(),
            when: InterruptWhen::Before,
        }];

        let result = check_orchestrate_pregel(&agents, &edges, &state, &cp, &interrupts, &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_append_non_list_type_error() {
        let state = vec![StateChannel {
            name: "messages".into(),
            type_hint: Some("string".into()),
            reducer: ReducerKind::Append,
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "requires list type"));
    }

    #[test]
    fn test_add_non_number_type_error() {
        let state = vec![StateChannel {
            name: "total".into(),
            type_hint: Some("string".into()),
            reducer: ReducerKind::Add,
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "requires number type"));
    }

    #[test]
    fn test_edge_unknown_node() {
        let agents = vec![OrchestrateAgent {
            name: "A".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let edges = vec![OrchestrateEdge {
            from: "A".into(),
            to: "B".into(),
            condition: None,
            dynamic: None,
        }];
        let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "unknown node"));
        assert!(has_error(&result, "B"));
    }

    #[test]
    fn test_checkpoint_unknown_saver() {
        let cp = Some(CheckpointConfig {
            saver: "mongodb".into(),
            thread_id: None,
        });
        let result = check_orchestrate_pregel(&[], &[], &[], &cp, &[], &[], &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "Unknown checkpoint saver"));
    }

    #[test]
    fn test_interrupt_unknown_node() {
        let agents = vec![OrchestrateAgent {
            name: "A".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let interrupts = vec![InterruptPoint {
            node_name: "B".into(),
            when: InterruptWhen::Before,
        }];
        let result = check_orchestrate_pregel(&agents, &[], &[], &None, &interrupts, &[], &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(
            &result,
            "Interrupt point references unknown node"
        ));
    }

    #[test]
    fn test_command_goto_unknown_node() {
        let agents = vec![OrchestrateAgent {
            name: "A".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let gotos = vec![("B".into(), 42)];
        let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &gotos, &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "Command goto references unknown node"));
    }

    #[test]
    fn test_send_target_unknown_node() {
        let agents = vec![OrchestrateAgent {
            name: "A".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let sends = vec![("B".into(), 42)];
        let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &[], &sends);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "Send target references unknown node"));
    }

    #[test]
    fn test_dynamic_map_without_reduce_warning() {
        let agents = vec![OrchestrateAgent {
            name: "split".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let edges = vec![OrchestrateEdge {
            from: "split".into(),
            to: "proc".into(),
            condition: None,
            dynamic: Some(DynamicKind::Map),
        }];
        let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "map") && has_error(&result, "reduce"));
    }

    #[test]
    fn test_duplicate_agent_name() {
        let agents = vec![
            OrchestrateAgent {
                name: "A".into(),
                with_config: None,
                task_expr: NodeId(0),
                verify_expr: None,
            },
            OrchestrateAgent {
                name: "A".into(),
                with_config: None,
                task_expr: NodeId(0),
                verify_expr: None,
            },
        ];
        let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &[], &[]);
        assert_eq!(result.len(), 1);
        assert!(has_error(&result, "Duplicate agent name"));
    }

    #[test]
    fn test_valid_start_exit_edges() {
        let edges = vec![OrchestrateEdge {
            from: "@start".into(),
            to: "@exit".into(),
            condition: None,
            dynamic: None,
        }];
        let result = check_orchestrate_pregel(&[], &edges, &[], &None, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_reducer_last_any_type_ok() {
        let state = vec![
            StateChannel {
                name: "x".into(),
                type_hint: Some("CustomType".into()),
                reducer: ReducerKind::Last,
            },
            StateChannel {
                name: "y".into(),
                type_hint: None,
                reducer: ReducerKind::Last,
            },
        ];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_multiple_errors_collected() {
        let state = vec![
            StateChannel {
                name: "a".into(),
                type_hint: Some("string".into()),
                reducer: ReducerKind::Append,
            },
            StateChannel {
                name: "b".into(),
                type_hint: Some("bool".into()),
                reducer: ReducerKind::Add,
            },
        ];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        assert_eq!(result.len(), 2);
        assert!(has_error(&result, "requires list type"));
        assert!(has_error(&result, "requires number type"));
    }

    #[test]
    fn test_sqlite_saver_valid() {
        let cp = Some(CheckpointConfig {
            saver: "sqlite".into(),
            thread_id: None,
        });
        let result = check_orchestrate_pregel(&[], &[], &[], &cp, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_dynamic_map_reduce_pair_valid() {
        // v0.50: 动态边需要声明静态端节点（map 的 from, reduce 的 to）
        let agents = vec![
            OrchestrateAgent {
                name: "split".into(),
                with_config: None,
                task_expr: NodeId(0),
                verify_expr: None,
            },
            OrchestrateAgent {
                name: "join".into(),
                with_config: None,
                task_expr: NodeId(0),
                verify_expr: None,
            },
        ];
        let edges = vec![
            OrchestrateEdge {
                from: "split".into(),
                to: "proc".into(),
                condition: None,
                dynamic: Some(DynamicKind::Map),
            },
            OrchestrateEdge {
                from: "proc".into(),
                to: "join".into(),
                condition: None,
                dynamic: Some(DynamicKind::Reduce),
            },
        ];
        let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_empty_state_schema_no_errors() {
        let result = check_orchestrate_pregel(&[], &[], &[], &None, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_number_hint_with_add_int() {
        let state = vec![StateChannel {
            name: "counter".into(),
            type_hint: Some("int".into()),
            reducer: ReducerKind::Add,
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_list_hint_with_append_bracket() {
        let state = vec![StateChannel {
            name: "items".into(),
            type_hint: Some("[string]".into()),
            reducer: ReducerKind::Append,
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_merge_reducer_valid() {
        let state = vec![StateChannel {
            name: "context".into(),
            type_hint: Some("Context".into()),
            reducer: ReducerKind::Merge(NodeId(1)),
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        no_errors(result);
    }

    #[test]
    fn test_interrupt_before_after_both_valid() {
        let agents = vec![OrchestrateAgent {
            name: "review".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let interrupts = vec![
            InterruptPoint {
                node_name: "review".into(),
                when: InterruptWhen::Before,
            },
            InterruptPoint {
                node_name: "review".into(),
                when: InterruptWhen::After,
            },
        ];
        let result = check_orchestrate_pregel(&agents, &[], &[], &None, &interrupts, &[], &[]);
        no_errors(result);
    }

    // ---------- v0.51 P0-7/P0-9: collect_command_sends 收集 → pregel_check 验证 ----------

    #[test]
    fn collected_command_goto_triggers_validation() {
        // 模拟 typeck 收集后传给 pregel_check：
        // command_goto 引用未声明节点 "nonexistent" → 期望 pregel_check 报错
        let agents = vec![OrchestrateAgent {
            name: "root".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let gotos = vec![("nonexistent".into(), 42_usize)];
        let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &gotos, &[]);
        assert!(
            has_error(
                &result,
                "Command goto references unknown node 'nonexistent'"
            ),
            "expected Command goto error, got: {:?}",
            result
        );
    }

    #[test]
    fn collected_send_target_triggers_validation() {
        // send target 引用未声明节点 "missing_node" → 期望 pregel_check 报错
        let agents = vec![OrchestrateAgent {
            name: "root".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        }];
        let sends = vec![("missing_node".into(), 7_usize)];
        let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &[], &sends);
        assert!(
            has_error(
                &result,
                "Send target references unknown node 'missing_node'"
            ),
            "expected Send target error, got: {:?}",
            result
        );
    }
}
