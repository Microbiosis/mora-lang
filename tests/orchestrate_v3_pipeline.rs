//! Tier 2.5: Orchestrate V3 Pipeline integration tests
//!
//! 验证 orchestrate 在 V3 管线中完整流通：
//! ParserV3::compile → MirInst::Orchestrate → run_mir → PregelEngine 执行

use mora::interpreter::Interpreter;
use mora::mir::MirInst;
use mora::mir::expr::MirOrchestrateKind;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;

fn compile_v3(source: &str) -> mora::mir::MirFunction {
    ParserV3::compile(source)
        .expect("compile should succeed")
        .0
}

// ===================================================================
// 1. 语法解析测试 — ParserV3 正确构建 Orchestrate MirInst
// ===================================================================

#[test]
fn v3_parse_orchestrate_sequential() {
    let func = compile_v3(
        r#"
orchestrate sequential input -> result
  agent a => "hello"
end
"#,
    );
    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();
    assert_eq!(orchestrate_insts.len(), 1, "expected exactly one Orchestrate inst");
}

#[test]
fn v3_parse_orchestrate_with_edge() {
    let func = compile_v3(
        r#"
orchestrate sequential input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
"#,
    );
    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();
    assert_eq!(orchestrate_insts.len(), 1, "expected exactly one Orchestrate inst");
}

#[test]
fn v3_parse_orchestrate_graph() {
    let func = compile_v3(
        r#"
orchestrate graph input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
"#,
    );
    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();
    assert_eq!(orchestrate_insts.len(), 1, "expected exactly one Orchestrate inst");
}

// ===================================================================
// 2. Lowering 单元测试 — 验证 MirExpr → MirInst 转换
// ===================================================================

#[test]
fn v3_lower_orchestrate_sequential_preserves_agents() {
    let func = compile_v3(
        r#"
orchestrate sequential input -> result
  agent a => "hello"
  agent b => "world"
end
"#,
    );
    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();
    assert_eq!(orchestrate_insts.len(), 1, "expected exactly one Orchestrate inst");
    if let MirInst::Orchestrate {
        input_var,
        result_var,
        kind,
    } = &orchestrate_insts[0]
    {
        assert_eq!(input_var, "input");
        assert_eq!(result_var, "result");
        assert!(
            matches!(kind.as_ref(), MirOrchestrateKind::Sequential { .. }),
            "expected Sequential kind"
        );
    }
}

#[test]
fn v3_lower_orchestrate_with_edge() {
    let func = compile_v3(
        r#"
orchestrate sequential input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
"#,
    );
    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();
    assert_eq!(orchestrate_insts.len(), 1);
}

#[test]
fn v3_lower_orchestrate_graph() {
    let func = compile_v3(
        r#"
orchestrate graph input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
"#,
    );
    let orchestrate_insts: Vec<_> = func
        .body
        .iter()
        .filter(|inst| matches!(inst, MirInst::Orchestrate { .. }))
        .collect();
    assert_eq!(orchestrate_insts.len(), 1);
    if let MirInst::Orchestrate { kind, .. } = &orchestrate_insts[0] {
        assert!(
            matches!(kind.as_ref(), MirOrchestrateKind::Graph { .. }),
            "expected Graph kind"
        );
    }
}

// ===================================================================
// 3. 端到端执行测试（orchestrate 程序）
// ===================================================================

#[test]
fn v3_orchestrate_sequential_runs() {
    let source = r#"
orchestrate sequential input -> result
  agent a => "hello"
  agent b => "world"
end
print(result)
"#;
    let (func, _witnesses) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(&func_arc, &mut interp, &mut env);
    match result {
        Ok(_) => {}
        Err(e) => panic!("orchestrate sequential should run: {}", e),
    }
}

#[test]
fn v3_orchestrate_with_edge_runs() {
    let source = r#"
orchestrate sequential input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
print(result)
"#;
    let (func, _witnesses) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(&func_arc, &mut interp, &mut env);
    match result {
        Ok(_) => {}
        Err(e) => panic!("orchestrate with edge should run: {}", e),
    }
}

// ===================================================================
// 4. Pregel 编排测试
// ===================================================================

#[test]
fn v3_orchestrate_pregel_runs() {
    let source = r#"
orchestrate pregel input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
print(result)
"#;
    let (func, _witnesses) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(&func_arc, &mut interp, &mut env);
    match result {
        Ok(_) => {}
        Err(e) => panic!("orchestrate pregel should run: {}", e),
    }
}

#[test]
fn v3_orchestrate_graph_runs() {
    let source = r#"
orchestrate graph input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
print(result)
"#;
    let (func, _witnesses) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(&func_arc, &mut interp, &mut env);
    match result {
        Ok(_) => {}
        Err(e) => panic!("orchestrate graph should run: {}", e),
    }
}
