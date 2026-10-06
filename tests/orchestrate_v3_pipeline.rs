//! Tier 2.5: Orchestrate V3 Pipeline integration tests
//!
//! 验证 orchestrate 在 V3 管线中完整流通：
//! ParserV3::compile → MirInst::Orchestrate → run_mir → PregelEngine 执行

use mora::interpreter::Interpreter;
use mora::mir::MirInst;
use mora::mir::orchestrate::MirOrchestrateKind;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;

fn compile_v3(source: &str) -> mora::mir::MirFunction {
    ParserV3::compile(source).expect("compile should succeed").0
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
    assert_eq!(
        orchestrate_insts.len(),
        1,
        "expected exactly one Orchestrate inst"
    );
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
    assert_eq!(
        orchestrate_insts.len(),
        1,
        "expected exactly one Orchestrate inst"
    );
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
    assert_eq!(
        orchestrate_insts.len(),
        1,
        "expected exactly one Orchestrate inst"
    );
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
    let result = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
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
    let result = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
    match result {
        Ok(_) => {}
        Err(e) => panic!("orchestrate with edge should run: {}", e),
    }
}

// ===================================================================
// 4. Pregel 编排测试
// ===================================================================

/// v0.104.6 D97：本测试原先**只断言「能跑通」、从不检查 `result`**，且它用的
/// 程序 `edge a -> b` **没有 `edge @start -> a`** —— 按引擎语义
/// （`active_nodes` 初始为 `vec!["@start"]`，下一跳沿 edges 从它计算）
/// 那样**没有任何 agent 会被激活**，`result` 恒为 `Nil`。
/// 即：**测试通过 ≠ 它测的那段代码真的执行过**。
///
/// 现在补上入口边并断言**链尾 agent 的值**。
#[test]
fn v3_orchestrate_pregel_runs() {
    let source = r#"
orchestrate pregel input -> result
  agent a => "hello"
  agent b => "world"
  edge @start -> a
  edge a -> b
end
result
"#;
    let (func, _witnesses) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
    let value = match result {
        Ok(v) => v,
        Err(e) => panic!("orchestrate pregel should run: {}", e),
    };
    assert_eq!(
        format!("{:?}", value),
        "String(\"world\")",
        "有入口边时 `result` 应是链尾 agent（b）的值 —— \
         若得到 Nil，说明 agent 未被激活（见 D97）"
    );
}

/// D97：去掉入口边时，引擎现在**报错**（守卫已加）。
/// 修前是「照常 exit 0、只是 `result` 为 `Nil`」—— 等于告诉用户「这张图跑过了」。
#[test]
fn v3_orchestrate_pregel_without_start_edge_is_an_error() {
    let source = r#"
orchestrate pregel input -> result
  agent a => "hello"
  agent b => "world"
  edge a -> b
end
result
"#;
    let (func, _witnesses) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
    let err = match result {
        Ok(v) => panic!("漏写入口边应**报错**；却静默返回了 {v:?} —— D97 守卫若被移除本测试会失败"),
        Err(e) => e,
    };
    assert!(
        err.contains("none was ever scheduled"),
        "错误消息应点明「无 agent 被调度」。实际：{err}"
    );
}

#[test]
fn v3_orchestrate_graph_runs() {
    // v0.104.6 D97：此形态原先也**没有入口边**，于是整张图静默空转、
    // `result` 恒为 `Nil`、exit 0 —— 而本测试只 `assert!(result.is_ok())`，
    // 于是「什么都没跑」被当成了「跑通了」。`orchestrate graph` 与
    // `orchestrate pregel` 走**同一个** `MirPregelEngine`，是同一个陷阱。
    let source = r#"
orchestrate graph input -> result
  agent a => "hello"
  agent b => "world"
  edge @start -> a
  edge a -> b
end
result
"#;
    let (func, _witnesses) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
    let value = match result {
        Ok(v) => v,
        Err(e) => panic!("orchestrate graph should run: {}", e),
    };
    assert_eq!(
        format!("{:?}", value),
        "String(\"world\")",
        "有入口边时 `result` 应是链尾 agent 的值"
    );
}
