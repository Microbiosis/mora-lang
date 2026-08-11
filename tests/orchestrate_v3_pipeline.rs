//! Tier 2.5: Orchestrate V3 Pipeline integration tests
//!
//! 验证 orchestrate 在 V3 MirExpr 管线中完整流通：
//! ParserV3 → MirExprKind::Orchestrate → lower_mir_exprs → MirInst::Orchestrate
//! → run_mir → PregelEngine 执行 → 正确结果

use mora::interpreter::Interpreter;
use mora::lexer::Lexer;
use mora::mir::MirInst;
use mora::mir::expr::{MirExprKind, MirOrchestrateKind};
use mora::mir::lower::lower_mir_exprs;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;

fn parse_v3(source: &str) -> Vec<mora::mir::expr::MirExpr> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.scan_tokens();
    let parser = ParserV3::new(tokens);
    parser.parse().unwrap_or_default()
}

// ===================================================================
// 1. 语法解析测试 — ParserV3 正确构建 Orchestrate MirExpr
// ===================================================================

#[test]
fn v3_parse_orchestrate_sequential() {
    let exprs = parse_v3(
        r#"
orchestrate sequential input -> result
  agent a => "hello"
end
"#,
    );
    assert_eq!(exprs.len(), 1);
    match &exprs[0].kind {
        MirExprKind::Orchestrate {
            input_var,
            result_var,
            kind,
        } => {
            assert_eq!(input_var, "input");
            assert_eq!(result_var, "result");
            match kind.as_ref() {
                MirOrchestrateKind::Sequential { agents } => {
                    assert_eq!(agents.len(), 1);
                    assert_eq!(agents[0].name, "a");
                    // v0.75.32: task_body 在 parse 阶段即被 lower 填充（此前恒空，
                    // pregel 报 "lowering missing"）。语义与旧 ignore 测试的
                    // 「lower 后非空」断言一致，仅时间点提前。
                    assert!(
                        !agents[0].task_body.body.is_empty(),
                        "task_body should be lowered at parse time"
                    );
                }
                _ => panic!("expected Sequential orchestrate"),
            }
        }
        other => panic!("expected Orchestrate, got {:?}", other),
    }
}

#[test]
fn v3_parse_orchestrate_pregel() {
    let exprs = parse_v3(
        r#"
orchestrate pregel input -> result
  agent a => "hello"
  @start -> a
end
"#,
    );
    assert_eq!(exprs.len(), 1);
    match &exprs[0].kind {
        MirExprKind::Orchestrate {
            input_var,
            result_var,
            kind,
        } => {
            assert_eq!(input_var, "input");
            assert_eq!(result_var, "result");
            match kind.as_ref() {
                MirOrchestrateKind::Pregel { agents, edges, .. } => {
                    assert_eq!(agents.len(), 1);
                    assert_eq!(agents[0].name, "a");
                    assert_eq!(edges.len(), 1);
                    assert_eq!(edges[0].from, "@start");
                    assert_eq!(edges[0].to, "a");
                }
                _ => panic!("expected Pregel orchestrate"),
            }
        }
        other => panic!("expected Orchestrate, got {:?}", other),
    }
}

// ===================================================================
// 2. Lowering 测试 — MirExpr → MIR 正确传递 orchestrate 结构
// ===================================================================

#[test]
fn v3_lower_orchestrate_sequential_preserves_agents() {
    let exprs = parse_v3(
        r#"
orchestrate sequential input -> result
  agent a => "hello"
  agent b => "world"
end
"#,
    );
    let func = lower_mir_exprs(&exprs).expect("lowering should succeed");

    // 应该产生一个 MirInst::Orchestrate
    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();

    assert_eq!(
        orchestrate_insts.len(),
        1,
        "expected exactly one Orchestrate inst"
    );
    if let MirInst::Orchestrate {
        input_var,
        result_var,
        kind,
    } = &orchestrate_insts[0]
    {
        assert_eq!(input_var, "input");
        assert_eq!(result_var, "result");
        match kind.as_ref() {
            MirOrchestrateKind::Sequential { agents } => {
                assert_eq!(agents.len(), 2);
                // v0.55: agent bodies are pre-lowered
                assert!(
                    !agents[0].task_body.body.is_empty(),
                    "agent 'a' task_body should be lowered"
                );
                assert!(
                    !agents[1].task_body.body.is_empty(),
                    "agent 'b' task_body should be lowered"
                );
            }
            _ => panic!("expected Sequential kind"),
        }
    }
}

#[test]
fn v3_lower_orchestrate_pregel_preserves_structure() {
    let exprs = parse_v3(
        r#"
orchestrate pregel input -> result
  agent a => "hello"
  @start -> a
end
"#,
    );
    let func = lower_mir_exprs(&exprs).expect("lowering should succeed");

    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();

    assert_eq!(orchestrate_insts.len(), 1);
    if let MirInst::Orchestrate { kind, .. } = &orchestrate_insts[0] {
        match kind.as_ref() {
            MirOrchestrateKind::Pregel { agents, edges, .. } => {
                assert_eq!(agents.len(), 1);
                assert!(
                    !agents[0].task_body.body.is_empty(),
                    "agent task_body should be lowered"
                );
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].from, "@start");
                assert_eq!(edges[0].to, "a");
            }
            _ => panic!("expected Pregel kind"),
        }
    }
}

// ===================================================================
// 3. 端到端执行测试 — V3 pipeline 完整运行 orchestrate
// ===================================================================

#[test]
// v0.75.34: Sequential orchestrate 执行已实现（handlers.rs h_orchestrate
// Sequential 分支 — pipeline 语义，task_body 顺序执行，前输出即后输入）。
fn v3_pipeline_orchestrate_sequential_runs() {
    let exprs = parse_v3(
        r#"
orchestrate sequential input -> result
  agent a => "hello"
end
"#,
    );
    let func = lower_mir_exprs(&exprs).expect("lowering should succeed");

    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    env.define(
        "input".to_string(),
        mora::value::Value::String("test".to_string()),
        false,
    );

    run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).expect("run_mir should succeed");

    let result = env.get("result");
    assert!(result.is_some(), "result variable should be defined");
    assert_eq!(result.unwrap().to_string(), "hello");
}

#[test]
fn v3_pipeline_orchestrate_pregel_runs() {
    let exprs = parse_v3(
        r#"
orchestrate pregel input -> result
  agent a => "hello"
  @start -> a
end
"#,
    );
    let func = lower_mir_exprs(&exprs).expect("lowering should succeed");

    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    env.define(
        "input".to_string(),
        mora::value::Value::String("test".to_string()),
        false,
    );

    run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).expect("run_mir should succeed");

    let result = env.get("result");
    assert!(result.is_some(), "result variable should be defined");
    assert_eq!(result.unwrap().to_string(), "hello");
}

// ===================================================================
// 4. 通用语句类型端到端测试 — 验证 V3 MirExpr 管线覆盖所有主要语句
// ===================================================================

/// Helper: parse V3 → lower → run → return env variable
fn v3_run_and_get(source: &str, var: &str) -> Result<String, String> {
    let exprs = parse_v3(source);
    let func = lower_mir_exprs(&exprs).map_err(|e| e.to_string())?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).map_err(|e| e.to_string())?;
    env.get(var)
        .ok_or_else(|| format!("variable '{}' not defined", var))
        .map(|v| v.to_string())
}

#[test]
fn v3_pipeline_let_binding_runs() {
    let result = v3_run_and_get(
        r#"
let x = 42
x
"#,
        "x",
    );
    assert_eq!(result.unwrap(), "42");
}

#[test]
fn v3_pipeline_if_else_runs() {
    let exprs = parse_v3(
        r#"
let flag = true
if flag then "yes" else "no" end
"#,
    );
    let func = lower_mir_exprs(&exprs).unwrap();
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let ret = run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).unwrap();
    assert_eq!(ret.to_string(), "yes");
}

// v0.75.32: for/while 循环的累加（sum = sum + i）在 MIR 执行返回 "0" —
// pre-existing 执行 bug（被旧 ignore 掩盖），与本阶段无关；独立运行
// /tmp/for.mora 亦复现。
#[test]
fn v3_pipeline_for_loop_runs() {
    let exprs = parse_v3(
        r#"
let items = [1, 2, 3]
let sum = 0
for i in items
  sum = sum + i
end
sum
"#,
    );
    let func = lower_mir_exprs(&exprs).unwrap();
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let ret = run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).unwrap();
    assert_eq!(ret.to_string(), "6");
}

// v0.75.32: 同 for 循环 — pre-existing 累加 bug，与本阶段无关。
#[test]
fn v3_pipeline_while_loop_runs() {
    let exprs = parse_v3(
        r#"
let n = 3
let acc = 0
while n > 0
  acc = acc + n
  n = n - 1
end
acc
"#,
    );
    let func = lower_mir_exprs(&exprs).unwrap();
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let ret = run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).unwrap();
    assert_eq!(ret.to_string(), "6");
}

#[test]
fn v3_pipeline_task_def_runs() {
    let exprs = parse_v3(
        r#"
task greet(name) "Hello, " + name end
greet("World")
"#,
    );
    let func = lower_mir_exprs(&exprs).unwrap();
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let ret = run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).unwrap();
    assert_eq!(ret.to_string(), "Hello, World");
}

#[test]
fn v3_pipeline_match_runs() {
    let exprs = parse_v3(
        r#"
match 42 {
  _ => "default"
}
"#,
    );
    let func = lower_mir_exprs(&exprs).unwrap();
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let ret = run_mir(&std::sync::Arc::new(func), &mut interp, &mut env).unwrap();
    assert_eq!(ret.to_string(), "default");
}

// ===================================================================
// v0.77: 从 tests/mir_orchestrate_lowering.rs 合并 — 5 个测试
// 验证 MirInst::Orchestrate 结构化 kind + superstep_fusion + optimize_pregel。
// 这些是 MirOrchestrateKind API 契约 + 超步融合优化 pass 的白盒保护。
// ===================================================================

#[test]
fn orchestrate_kind_is_structured_not_string() {
    // 验证类型：MirInst::Orchestrate 携带 Box<MirOrchestrateKind>，
    // 若有人误回退到 `kind: String`，本测试将编译失败。
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
    // 验证全部四种 orchestrate kind 都能构造并装箱
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

#[test]
fn superstep_fusion_removes_consecutive_duplicate_orchestrates() {
    // 构造两个连续的相同 Pregel orchestrate
    let kind = MirOrchestrateKind::Pregel {
        agents: vec![],
        edges: vec![],
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: std::collections::HashMap::new(),
    };

    let mut func = mora::mir::MirFunction {
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
        ..Default::default()
    };

    mora::mir::opt::superstep_fusion(&mut func);

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

    let mut func = mora::mir::MirFunction {
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
        ..Default::default()
    };

    mora::mir::opt::superstep_fusion(&mut func);

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
    // 验证 optimize_pregel 不 panic（即使 body 中没有 orchestrate）
    let mut func = mora::mir::MirFunction {
        params: vec![],
        body: vec![MirInst::Const(0, mora::value::Value::Int(1))],
        n_regs: 1,
        ..Default::default()
    };
    mora::mir::opt::optimize_pregel(&mut func);
    assert_eq!(func.body.len(), 1);
}
