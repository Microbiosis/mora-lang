//! Worker 5: Pregel v0.50 集成测试
//!
//! 测试矩阵（对应 orchestrate_v50_implementation_plan.md §8）：
//!
//! | 测试 | 内容 | 预期 |
//! |------|------|------|
//! | test_pregel_basic | Pregel 基本类型检查 | 通过 |
//! | test_pregel_parallel_append | 并行节点 @append 到同一 channel | 类型合法 |
//! | test_pregel_checkpoint | Checkpoint 配置检查 | 通过 |
//! | test_pregel_command_goto | Command 动态路由目标检查 | 路由合法 |
//! | test_pregel_command_update | Command 状态更新字段检查 | 通过 |
//! | test_pregel_dynamic_map | 动态 map 派发边检查 | N 任务合法 |
//! | test_pregel_dynamic_reduce | 动态 reduce 聚合边检查 | 结果合法 |
//! | test_pregel_interrupt_resume | interrupt before + resume 检查 | 暂停点合法 |
//! | test_pregel_rewind | 回溯到指定步骤 schema 检查 | 通过 |
//! | test_pregel_stress_100_channels | 100 个 channel 的 schema 检查 | 不 panic |
//! | test_pregel_full_graph | 完整 LangGraph 风格图 | 全类型通过 |
//! | test_pregel_type_error_accumulation | 多错误累积报告 | 所有错误被收集 |
//!
//! 注意：当前 Worker 1/2/3/4 的 AST/Parser/Interpreter/Checkpoint 尚未全部完成，
//! 本测试集通过 `typeck::pregel_check` 占位 API 进行类型层面的集成验证。
//! 当各 Worker 完成后，可扩展为端到端运行时测试。

use mora::ast_v2::{
    CheckpointConfig, DynamicKind, InterruptPoint, InterruptWhen, NodeId, OrchestrateAgent,
    OrchestrateEdge, ReducerKind, StateChannel,
};
use mora::typeck::pregel_check::check_orchestrate_pregel;
use mora::typeck::TypeError;

// ===================================================================
// 辅助断言
// ===================================================================

fn assert_no_errors(result: Vec<TypeError>) {
    assert!(
        result.is_empty(),
        "expected no errors, got {} errors: {:?}",
        result.len(),
        result
    );
}

fn assert_has_error(result: &Vec<TypeError>, pat: &str) -> bool {
    let found = result.iter().any(|e| e.message.contains(pat));
    assert!(
        found,
        "expected error containing '{}', got: {:?}",
        pat, result
    );
    found
}

fn assert_error_count(result: &Vec<TypeError>, expected: usize) {
    assert_eq!(
        result.len(),
        expected,
        "expected {} errors, got {}: {:?}",
        expected,
        result.len(),
        result
    );
}

// ===================================================================
// 基础场景
// ===================================================================

#[test]
fn test_pregel_basic() {
    // 对应 test matrix: test_pregel_basic
    // 一个最简单的 Pregel 图：classifier -> handler
    let agents = vec![
        OrchestrateAgent {
            name: "classifier".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "handler".into(),
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
            to: "handler".into(),
            condition: None,
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
        saver: "sqlite".into(),
        thread_id: Some(NodeId(0)),
    });
    let interrupts = vec![InterruptPoint {
        node_name: "handler".into(),
        when: InterruptWhen::Before,
    }];

    let result = check_orchestrate_pregel(
        &agents, &edges, &state, &cp, &interrupts, &[], &[],
    );
    assert_no_errors(result);
}

#[test]
fn test_pregel_parallel_append() {
    // 对应 test matrix: test_pregel_parallel
    // 两个并行节点都写入 messages channel，@append 保证列表合并安全
    let agents = vec![
        OrchestrateAgent {
            name: "node_a".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "node_b".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let edges = vec![
        OrchestrateEdge {
            from: "@start".into(),
            to: "node_a".into(),
            condition: None,
            dynamic: None,
        },
        OrchestrateEdge {
            from: "@start".into(),
            to: "node_b".into(),
            condition: None,
            dynamic: None,
        },
    ];
    let state = vec![StateChannel {
        name: "messages".into(),
        type_hint: Some("[string]".into()),
        reducer: ReducerKind::Append,
    }];

    let result = check_orchestrate_pregel(&agents, &edges, &state, &None, &[], &[], &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_checkpoint() {
    // 对应 test matrix: test_pregel_checkpoint
    // 验证 memory 和 sqlite 两种后端都被接受
    for saver in ["memory", "sqlite"] {
        let cp = Some(CheckpointConfig {
            saver: saver.into(),
            thread_id: Some(NodeId(0)),
        });
        let result = check_orchestrate_pregel(&[], &[], &[], &cp, &[], &[], &[]);
        assert_no_errors(result);
    }
}

#[test]
fn test_pregel_checkpoint_unknown_saver() {
    let cp = Some(CheckpointConfig {
        saver: "mongodb".into(),
        thread_id: None,
    });
    let result = check_orchestrate_pregel(&[], &[], &[], &cp, &[], &[], &[]);
    assert_error_count(&result, 1);
    assert_has_error(&result, "Unknown checkpoint saver");
}

// ===================================================================
// Command 动态控制流
// ===================================================================

#[test]
fn test_pregel_command_goto() {
    // 对应 test matrix: test_pregel_command_goto
    // 节点返回 Command 时 goto 目标必须是图中已声明的 agent
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
    // 模拟 classifier 返回 command { goto: "handler_urgent" }
    let gotos = vec![("handler_urgent".into(), 42)];
    let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &gotos, &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_command_goto_invalid() {
    // goto 目标不存在时应报错
    let agents = vec![
        OrchestrateAgent {
            name: "classifier".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let gotos = vec![("nonexistent".into(), 20)];
    let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &gotos, &[]);
    assert_error_count(&result, 1);
    assert_has_error(&result, "Command goto references unknown node");
}

#[test]
fn test_pregel_command_goto_special_nodes() {
    // @start 和 @exit 作为 goto 目标应该是合法的（运行时语义）
    let agents = vec![
        OrchestrateAgent {
            name: "classifier".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let gotos = vec![("@exit".into(), 10), ("@start".into(), 11)];
    let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &gotos, &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_command_update() {
    // 对应 test matrix: test_pregel_command_update
    // Command 的 update 字段影响状态，需保证状态 schema 已声明
    // 类型检查层面：验证 update 引用的 channel 名称在 state_schema 中已存在
    // （当前 placeholder 版本中仅检查表达式子结构，channel 名检查留给运行时）
    let state = vec![
        StateChannel {
            name: "priority".into(),
            type_hint: Some("number".into()),
            reducer: ReducerKind::Last,
        },
        StateChannel {
            name: "messages".into(),
            type_hint: Some("[string]".into()),
            reducer: ReducerKind::Append,
        },
    ];
    let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
    assert_no_errors(result);
}

// ===================================================================
// Dynamic Dispatch (Map-Reduce)
// ===================================================================

#[test]
fn test_pregel_dynamic_map() {
    // 对应 test matrix: test_pregel_dynamic_map
    // split -> process 动态展开为 N 个并行任务
    let agents = vec![
        OrchestrateAgent {
            name: "split".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "process".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let edges = vec![OrchestrateEdge {
        from: "split".into(),
        to: "process".into(),
        condition: None,
        dynamic: Some(DynamicKind::Map),
    }];
    let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
    // map 无 reduce 会发出警告
    assert_error_count(&result, 1);
    assert_has_error(&result, "map");
    assert_has_error(&result, "reduce");
}

#[test]
fn test_pregel_dynamic_reduce() {
    // 对应 test matrix: test_pregel_dynamic_reduce
    // process -> join 聚合 N 个结果
    let agents = vec![
        OrchestrateAgent {
            name: "process".into(),
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
    let edges = vec![OrchestrateEdge {
        from: "process".into(),
        to: "join".into(),
        condition: None,
        dynamic: Some(DynamicKind::Reduce),
    }];
    let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_dynamic_map_reduce_full() {
    // 完整 Map-Reduce 链路：split -> (map) -> process -> (reduce) -> join
    let agents = vec![
        OrchestrateAgent {
            name: "split".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "process".into(),
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
            to: "process".into(),
            condition: None,
            dynamic: Some(DynamicKind::Map),
        },
        OrchestrateEdge {
            from: "process".into(),
            to: "join".into(),
            condition: None,
            dynamic: Some(DynamicKind::Reduce),
        },
    ];
    let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_dynamic_fan_out_fan_in() {
    // FanOut / FanIn 语义等价于固定并行 worker 的展开与汇聚
    let agents = vec![
        OrchestrateAgent {
            name: "source".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "worker1".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "worker2".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "sink".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let edges = vec![
        OrchestrateEdge {
            from: "source".into(),
            to: "worker1".into(),
            condition: None,
            dynamic: Some(DynamicKind::FanOut),
        },
        OrchestrateEdge {
            from: "source".into(),
            to: "worker2".into(),
            condition: None,
            dynamic: Some(DynamicKind::FanOut),
        },
        OrchestrateEdge {
            from: "worker1".into(),
            to: "sink".into(),
            condition: None,
            dynamic: Some(DynamicKind::FanIn),
        },
        OrchestrateEdge {
            from: "worker2".into(),
            to: "sink".into(),
            condition: None,
            dynamic: Some(DynamicKind::FanIn),
        },
    ];
    let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
    assert_no_errors(result);
}

// ===================================================================
// Send 动态派发
// ===================================================================

#[test]
fn test_pregel_send_target_valid() {
    // send("process", { task: t }) 目标节点存在
    let agents = vec![
        OrchestrateAgent {
            name: "split".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
        OrchestrateAgent {
            name: "process".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let sends = vec![("process".into(), 5)];
    let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &[], &sends);
    assert_no_errors(result);
}

#[test]
fn test_pregel_send_target_invalid() {
    let agents = vec![
        OrchestrateAgent {
            name: "split".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let sends = vec![("nonexistent".into(), 5)];
    let result = check_orchestrate_pregel(&agents, &[], &[], &None, &[], &[], &sends);
    assert_error_count(&result, 1);
    assert_has_error(&result, "Send target references unknown node");
}

// ===================================================================
// Interrupt + Resume
// ===================================================================

#[test]
fn test_pregel_interrupt_resume() {
    // 对应 test matrix: test_pregel_interrupt_resume
    // interrupt before handler_urgent + resume 恢复
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
    ];
    let interrupts = vec![
        InterruptPoint {
            node_name: "handler_urgent".into(),
            when: InterruptWhen::Before,
        },
        InterruptPoint {
            node_name: "handler_urgent".into(),
            when: InterruptWhen::After,
        },
    ];
    let result = check_orchestrate_pregel(&agents, &[], &[], &None, &interrupts, &[], &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_interrupt_unknown_node() {
    let agents = vec![
        OrchestrateAgent {
            name: "classifier".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let interrupts = vec![InterruptPoint {
        node_name: "nonexistent".into(),
        when: InterruptWhen::Before,
    }];
    let result = check_orchestrate_pregel(&agents, &[], &[], &None, &interrupts, &[], &[]);
    assert_error_count(&result, 1);
    assert_has_error(&result, "Interrupt point references unknown node");
}

// ===================================================================
// Rewind / Checkpoint 语义
// ===================================================================

#[test]
fn test_pregel_rewind_schema_valid() {
    // 对应 test matrix: test_pregel_rewind
    // rewind 依赖于 checkpoint 的 schema 一致性：所有 channel 必须可序列化
    // 类型检查层面：确保 state_schema 类型 hint 非空且合法
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
    ];
    let cp = Some(CheckpointConfig {
        saver: "sqlite".into(),
        thread_id: Some(NodeId(0)),
    });
    let result = check_orchestrate_pregel(&[], &[], &state, &cp, &[], &[], &[]);
    assert_no_errors(result);
}

// ===================================================================
// 压力测试
// ===================================================================

#[test]
fn test_pregel_stress_100_channels() {
    // 对应 test matrix: test_pregel_stress_100_steps
    // 100 个 channel 的 schema 检查：不 panic，性能可接受
    let mut state = Vec::with_capacity(100);
    for i in 0..100 {
        state.push(StateChannel {
            name: format!("ch_{}", i),
            type_hint: Some("number".into()),
            reducer: ReducerKind::Add,
        });
    }
    let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_stress_100_agents_with_edges() {
    // 100 个 agent 构成的链式图
    let mut agents = Vec::with_capacity(100);
    let mut edges = Vec::with_capacity(99);
    for i in 0..100 {
        agents.push(OrchestrateAgent {
            name: format!("node_{}", i),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        });
        if i > 0 {
            edges.push(OrchestrateEdge {
                from: format!("node_{}", i - 1),
                to: format!("node_{}", i),
                condition: None,
                dynamic: None,
            });
        }
    }
    // 添加 @start -> node_0
    edges.push(OrchestrateEdge {
        from: "@start".into(),
        to: "node_0".into(),
        condition: None,
        dynamic: None,
    });
    let result = check_orchestrate_pregel(&agents, &edges, &[], &None, &[], &[], &[]);
    assert_no_errors(result);
}

// ===================================================================
// 完整图示例（LangGraph 风格）
// ===================================================================

#[test]
fn test_pregel_full_graph() {
    // 一个接近真实场景的完整图：
    // @start -> classifier -> (urgent / normal) -> merge -> @exit
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
        OrchestrateAgent {
            name: "merge".into(),
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
        OrchestrateEdge {
            from: "handler_urgent".into(),
            to: "merge".into(),
            condition: None,
            dynamic: Some(DynamicKind::FanIn),
        },
        OrchestrateEdge {
            from: "handler_normal".into(),
            to: "merge".into(),
            condition: None,
            dynamic: Some(DynamicKind::FanIn),
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
            name: "context".into(),
            type_hint: Some("Context".into()),
            reducer: ReducerKind::Merge(NodeId(0)),
        },
    ];
    let cp = Some(CheckpointConfig {
        saver: "memory".into(),
        thread_id: Some(NodeId(0)),
    });
    let interrupts = vec![
        InterruptPoint {
            node_name: "handler_urgent".into(),
            when: InterruptWhen::Before,
        },
        InterruptPoint {
            node_name: "merge".into(),
            when: InterruptWhen::After,
        },
    ];
    let gotos = vec![("handler_urgent".into(), 10)];
    let sends = vec![("handler_normal".into(), 20)];

    let result = check_orchestrate_pregel(
        &agents, &edges, &state, &cp, &interrupts, &gotos, &sends,
    );
    assert_no_errors(result);
}

// ===================================================================
// 多错误累积（类型检查核心设计原则）
// ===================================================================

#[test]
fn test_pregel_type_error_accumulation() {
    // 验证 typeck 的多错误收集模式：所有问题一次报告，不中途退出
    let agents = vec![
        OrchestrateAgent {
            name: "A".into(),
            with_config: None,
            task_expr: NodeId(0),
            verify_expr: None,
        },
    ];
    let edges = vec![
        // 错误 1: to 未知
        OrchestrateEdge {
            from: "A".into(),
            to: "B".into(),
            condition: None,
            dynamic: None,
        },
        // 错误 2: from 未知
        OrchestrateEdge {
            from: "C".into(),
            to: "A".into(),
            condition: None,
            dynamic: None,
        },
    ];
    let state = vec![
        // 错误 3: @append 非列表类型
        StateChannel {
            name: "messages".into(),
            type_hint: Some("string".into()),
            reducer: ReducerKind::Append,
        },
        // 错误 4: @add 非数值类型
        StateChannel {
            name: "total".into(),
            type_hint: Some("bool".into()),
            reducer: ReducerKind::Add,
        },
    ];
    let cp = Some(CheckpointConfig {
        saver: "unknown_db".into(),
        thread_id: None,
    });
    let interrupts = vec![InterruptPoint {
        node_name: "D".into(),
        when: InterruptWhen::Before,
    }];
    let gotos = vec![("E".into(), 1)];
    let sends = vec![("F".into(), 2)];

    let result = check_orchestrate_pregel(
        &agents, &edges, &state, &cp, &interrupts, &gotos, &sends,
    );

    // 期望收集到的错误：
    // 1. Edge to B unknown
    // 2. Edge from C unknown
    // 3. @append 非 list
    // 4. @add 非 number
    // 5. Unknown saver
    // 6. Interrupt D unknown
    // 7. Command goto E unknown
    // 8. Send target F unknown
    assert!(
        result.len() >= 7,
        "expected at least 7 accumulated errors, got {}: {:?}",
        result.len(),
        result
    );
    assert_has_error(&result, "unknown node");
    assert_has_error(&result, "requires list type");
    assert_has_error(&result, "requires number type");
    assert_has_error(&result, "Unknown checkpoint saver");
    assert_has_error(&result, "Interrupt point references unknown node");
    assert_has_error(&result, "Command goto references unknown node");
    assert_has_error(&result, "Send target references unknown node");
}

// ===================================================================
// Reducer 与类型兼容性边界
// ===================================================================

#[test]
fn test_reducer_append_with_list_type_variants() {
    // 验证多种列表类型表示法都被接受
    for hint in ["list<string>", "[string]", "List<int>", "[Message]"] {
        let state = vec![StateChannel {
            name: "items".into(),
            type_hint: Some(hint.into()),
            reducer: ReducerKind::Append,
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        assert_no_errors(result);
    }
}

#[test]
fn test_reducer_add_with_number_type_variants() {
    // 验证多种数值类型表示法都被接受
    for hint in ["number", "int", "float", "Int", "Float"] {
        let state = vec![StateChannel {
            name: "counter".into(),
            type_hint: Some(hint.into()),
            reducer: ReducerKind::Add,
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        assert_no_errors(result);
    }
}

#[test]
fn test_reducer_last_accepts_any_type() {
    for hint in ["CustomType", "string", "bool", "nil", "SomeComplex<T>"] {
        let state = vec![StateChannel {
            name: "x".into(),
            type_hint: Some(hint.into()),
            reducer: ReducerKind::Last,
        }];
        let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
        assert_no_errors(result);
    }
}

#[test]
fn test_reducer_merge_non_empty_fn() {
    let state = vec![StateChannel {
        name: "ctx".into(),
        type_hint: Some("Context".into()),
        reducer: ReducerKind::Merge(NodeId(1)),
    }];
    let result = check_orchestrate_pregel(&[], &[], &state, &None, &[], &[], &[]);
    assert_no_errors(result);
}

// ===================================================================
// 边界：空/最小输入
// ===================================================================

#[test]
fn test_empty_pregel_no_errors() {
    let result = check_orchestrate_pregel(&[], &[], &[], &None, &[], &[], &[]);
    assert_no_errors(result);
}

#[test]
fn test_pregel_start_to_exit_only() {
    let edges = vec![OrchestrateEdge {
        from: "@start".into(),
        to: "@exit".into(),
        condition: None,
        dynamic: None,
    }];
    let result = check_orchestrate_pregel(&[], &edges, &[], &None, &[], &[], &[]);
    assert_no_errors(result);
}

// ===================================================================
// 注释：未来运行时测试扩展点
// ===================================================================
// 当 Worker 1/2/3/4 完成后，以下端到端测试应补充到本文件：
//
// 1. `test_pregel_runtime_basic` — 构造 AST 并执行 PregelEngine::run
// 2. `test_pregel_runtime_parallel_append` — 验证两个并行节点写入后
//    messages channel 包含 2 个元素
// 3. `test_pregel_runtime_checkpoint_save_load` — 验证 CheckpointSaver
//    保存后 load 状态一致
// 4. `test_pregel_runtime_command_goto` — 节点返回 Command 后路由正确
// 5. `test_pregel_runtime_command_update` — Command 更新后状态正确
// 6. `test_pregel_runtime_dynamic_map` — send() 生成 N 个任务并执行
// 7. `test_pregel_runtime_dynamic_reduce` — reduce 正确汇总 N 个结果
// 8. `test_pregel_runtime_interrupt_resume` — 暂停后 resume 正确恢复
// 9. `test_pregel_runtime_rewind` — rewind 到指定步骤后状态一致
// 10. `test_pregel_runtime_stress_100_steps` — 100 步图执行不 panic
