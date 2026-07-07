//! v0.55: MIR orchestrate lowering integration tests.
//!
//! Verifies that `MirInst::Orchestrate` carries structured `MirOrchestrateKind`
//! (not a plain String), enabling MIR-native orchestrate execution.

use mora::mir::{MirFunction, MirInst, expr::MirOrchestrateKind};

#[test]
fn orchestrate_kind_is_structured_not_string() {
    // This test verifies the type: MirInst::Orchestrate carries Box<MirOrchestrateKind>
    // If someone accidentally reverts to `kind: String`, this test will fail to compile.
    let kind = MirOrchestrateKind::Pregel {
        agents: vec![],
        edges: vec![],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: std::collections::HashMap::new(),
    };
    let inst = MirInst::Orchestrate {
        input_var: "input".to_string(),
        result_var: "result".to_string(),
        kind: Box::new(kind),
    };

    match inst {
        MirInst::Orchestrate { kind, .. } => {
            assert!(matches!(kind.as_ref(), MirOrchestrateKind::Pregel { .. }));
        }
        _ => panic!("expected Orchestrate variant"),
    }
}

#[test]
fn orchestrate_all_kinds_constructible() {
    // Verify all four orchestrate kinds can be constructed and boxed
    let _seq = Box::new(MirOrchestrateKind::Sequential { agents: vec![] });
    let _loop = Box::new(MirOrchestrateKind::Loop {
        agents: vec![],
        rounds: Some(10),
        exit_when: None,
    });
    let _graph = Box::new(MirOrchestrateKind::Graph {
        agents: vec![],
        edges: vec![],
    });
    let _pregel = Box::new(MirOrchestrateKind::Pregel {
        agents: vec![],
        edges: vec![],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: std::collections::HashMap::new(),
    });
}

// ===================================================================
// Phase 3: MIR optimization pass tests
// ===================================================================

#[test]
fn superstep_fusion_removes_consecutive_duplicate_orchestrates() {
    // 构造两个连续的 Pregel orchestrate（kind 完全相同）
    let kind = MirOrchestrateKind::Pregel {
        agents: vec![],
        edges: vec![],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: std::collections::HashMap::new(),
    };

    let mut func = MirFunction {
        params: vec![],
        body: vec![
            MirInst::Orchestrate {
                input_var: "input".to_string(),
                result_var: "result".to_string(),
                kind: Box::new(kind.clone()),
            },
            MirInst::Orchestrate {
                input_var: "input".to_string(),
                result_var: "result".to_string(),
                kind: Box::new(kind),
            },
            MirInst::Const(0, mora::value::Value::Int(42)), // trailing instruction
        ],
        n_regs: 1,
    };

    mora::mir::opt::superstep_fusion(&mut func);

    // 第二个冗余的 orchestrate 应该被移除
    let orchestrate_count = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .count();
    assert_eq!(
        orchestrate_count, 1,
        "expected 1 orchestrate after fusion, got {}",
        orchestrate_count
    );
    assert_eq!(
        func.body.len(),
        2,
        "expected 2 instructions after fusion, got {}",
        func.body.len()
    );
}

#[test]
fn superstep_fusion_preserves_different_orchestrates() {
    // 构造两个不同的 Pregel orchestrate（edges 不同）
    let kind1 = MirOrchestrateKind::Pregel {
        agents: vec![],
        edges: vec![],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: std::collections::HashMap::new(),
    };
    let kind2 = MirOrchestrateKind::Pregel {
        agents: vec![],
        edges: vec![mora::mir::expr::MirEdgeDef {
            from: "a".to_string(),
            to: "b".to_string(),
            condition_expr: None,
            condition_body: None,
        }],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: std::collections::HashMap::new(),
    };

    let mut func = MirFunction {
        params: vec![],
        body: vec![
            MirInst::Orchestrate {
                input_var: "input".to_string(),
                result_var: "r1".to_string(),
                kind: Box::new(kind1),
            },
            MirInst::Orchestrate {
                input_var: "input".to_string(),
                result_var: "r2".to_string(),
                kind: Box::new(kind2),
            },
        ],
        n_regs: 0,
    };

    mora::mir::opt::superstep_fusion(&mut func);

    // 两个不同 kind 的 orchestrate 都应该保留
    let orchestrate_count = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .count();
    assert_eq!(
        orchestrate_count, 2,
        "expected 2 orchestrates preserved, got {}",
        orchestrate_count
    );
}

#[test]
fn optimize_pregel_runs_all_passes() {
    // 验证 optimize_pregel 不会 panic（即使 body 中没有 orchestrate）
    let mut func = MirFunction {
        params: vec![],
        body: vec![MirInst::Const(0, mora::value::Value::Int(1))],
        n_regs: 1,
    };
    mora::mir::opt::optimize_pregel(&mut func);
    assert_eq!(func.body.len(), 1);
}
