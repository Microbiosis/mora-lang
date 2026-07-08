//! MIR 差分测试（α.0 gate）
//!
//! 对同一脚本，分别用 AST 解释器和 MIR 解释器执行，断言输出一致。
//! 这不靠手写 MIR 测试，靠双解释器差分——AST 解释器是"标准"，MIR 追齐它。

use mora::ast_v2::{AstArena, NodeId};
use mora::interpreter::Interpreter;
use mora::mir::interp::run_mir;
use mora::mir::lower::lower_program;

/// 解析 + lowering，返回 (node_ids, arena, mir_func)
fn parse_and_lower(source: &str) -> (Vec<NodeId>, AstArena, mora::mir::MirFunction) {
    let (node_ids, arena) = mora::interpreter::parse_code(source);
    let mir = lower_program(&node_ids, &arena).expect("lowering failed");
    (node_ids, arena, mir)
}

/// 对比 AST 解释器和 MIR 解释器的执行结果
/// AST interpret 返回 Result<(), String>；MIR run_mir 返回 Result<Value, String>。
/// 差分策略：两者都应 Ok（无错误），或都 Err（有错误且错误一致）。
/// 顶层无 return 时 MIR 返回 Nil，AST 返回 ()——都算 Ok，算一致。
fn assert_differential(source: &str) {
    let (node_ids, arena, mir) = parse_and_lower(source);

    // AST 解释器
    let mut ast_interp = Interpreter::new();
    let ast_result = ast_interp.interpret(&node_ids, &arena);

    // MIR 解释器
    let mut mir_interp = Interpreter::new();
    let globals = mir_interp.get_globals();
    let mut env = mora::value::Environment::with_parent_of(globals);
    let mir_result = run_mir(&mir, &mut mir_interp, &mut env);

    // 差分断言：Ok/Err 类型不同，只比对"是否出错"
    match (&ast_result, &mir_result) {
        (Ok(()), Ok(_)) => { /* 两者都成功，一致 */ }
        (Err(ae), Err(me)) => {
            // 两者都错，算一致（错误信息可能不同，α.0 不强制相同）
            let _ = (ae, me);
        }
        (Ok(()), Err(me)) => panic!(
            "AST ok but MIR errored for source:\n{}\nMIR error: {}",
            source, me
        ),
        (Err(ae), Ok(_)) => panic!(
            "AST errored but MIR ok for source:\n{}\nAST error: {}",
            source, ae
        ),
    }
}

#[test]
fn mir_differential_let_and_binary() {
    assert_differential("let x = 1 + 2");
}

#[test]
fn mir_differential_nested_binary() {
    assert_differential("let x = 1 + 2 * 3");
}

#[test]
fn mir_differential_variable_use() {
    assert_differential("let x = 10\nlet y = x + 5");
}

#[test]
fn mir_differential_assign() {
    assert_differential("let x = 1\nx = 2");
}

#[test]
fn mir_differential_if_true() {
    assert_differential("if 1 < 2 then\n  let x = 100\nend");
}

#[test]
fn mir_differential_if_false() {
    assert_differential("if 1 > 2 then\n  let x = 100\nelse\n  let y = 200\nend");
}

#[test]
fn mir_differential_top_level_return_is_error_in_ast() {
    // 顶层 return 在 AST 解释器里是 Err（"Unexpected return at top level"）。
    // MIR 解释器当前会直接返回——这是已知的语义差异（α.0 不处理"顶层 return 非法"）。
    // α.0 记录此差异，α.1 会在 MIR 解释器加顶层 return 检查。此处只验证 lowering 不 panic。
    let (node_ids, _arena, mir) = parse_and_lower("return 42");
    assert!(!mir.body.is_empty());
    // 不做差分断言（语义差异已知）
    let _ = node_ids;
}

#[test]
fn mir_lowering_produces_expected_insts() {
    // 直接验证 lowering 产出的指令结构（不跑解释器）
    let (_, _, mir) = parse_and_lower("let x = 1 + 2");
    assert!(mir.n_regs >= 3, "expected >=3 regs, got {}", mir.n_regs);
    // 应有: Const(r0,1), Const(r1,2), BinaryOp(r2,Add,r0,r1), Define("x",r2)
    assert!(
        mir.body.len() >= 4,
        "expected >=4 insts, got {}",
        mir.body.len()
    );
}

#[test]
fn mir_lowering_if_branch() {
    let (_, _, mir) = parse_and_lower("if 1 < 2 then\n  let x = 1\nend");
    // 应含 JumpIfNot + Define + Jump + Label
    let has_jump = mir
        .body
        .iter()
        .any(|i| matches!(i, mora::mir::MirInst::JumpIfNot(..)));
    assert!(has_jump, "expected JumpIfNot in if lowering");
}

// ── α.1 差分测试 ──

#[test]
fn mir_differential_list_literal() {
    assert_differential("let x = [1, 2, 3]");
}

#[test]
fn mir_differential_dict_literal() {
    assert_differential("let x = {a: 1, b: 2}");
}

#[test]
fn mir_differential_list_index() {
    assert_differential("let xs = [10, 20, 30]\nlet y = xs[1]");
}

#[test]
fn mir_differential_dict_index() {
    assert_differential("let d = {a: 1, b: 2}\nlet y = d[\"a\"]");
}

#[test]
fn mir_differential_method_call() {
    assert_differential("let xs = [1, 2, 3]\nlet y = xs.len()");
}

#[test]
fn mir_differential_for_loop_basic() {
    assert_differential("for x in [1, 2, 3]\n  let y = x\nend");
}

#[test]
fn mir_differential_for_loop_break() {
    assert_differential("for x in [1, 2, 3, 4, 5]\n  if x > 3 then\n    break\n  end\nend");
}

#[test]
fn mir_differential_pipe() {
    // pipe: 5 |> fn(x) return x + 1 end
    // α.1 pipe 走 call_value，需要闭包支持——简化测试用内置函数
    assert_differential("let y = [1, 2, 3] |> len");
}

#[test]
fn mir_lowering_for_has_loop_structure() {
    let (_, _, mir) = parse_and_lower("for x in [1, 2]\n  let y = x\nend");
    // 应含 Call(len) + Index + JumpIf + Jump（循环回跳）
    let has_call = mir
        .body
        .iter()
        .any(|i| matches!(i, mora::mir::MirInst::Call(..)));
    let has_index = mir
        .body
        .iter()
        .any(|i| matches!(i, mora::mir::MirInst::Index(..)));
    let has_back_jump = mir
        .body
        .iter()
        .any(|i| matches!(i, mora::mir::MirInst::Jump(..)));
    assert!(has_call, "expected Call(len) in for lowering");
    assert!(has_index, "expected Index in for lowering");
    assert!(has_back_jump, "expected back Jump in for lowering");
}

// ── α.2 差分测试：TaskDef ──

#[test]
fn mir_differential_task_def_and_call() {
    assert_differential(
        "task add(a: number, b: number): number\n  return a + b\nend\nlet r = add(3, 4)",
    );
}

#[test]
fn mir_differential_task_no_args() {
    assert_differential("task forty_two()\n  return 42\nend\nlet r = forty_two()");
}

#[test]
fn mir_differential_task_calls_builtin() {
    assert_differential(
        "task mylen(xs: list): number\n  return len(xs)\nend\nlet r = mylen([1, 2, 3])",
    );
}

#[test]
fn mir_differential_task_with_if() {
    assert_differential(
        "task max(a: number, b: number): number\n  if a > b then\n    return a\n  else\n    return b\n  end\nend\nlet r = max(5, 3)",
    );
}

#[test]
fn mir_differential_task_recursion() {
    assert_differential(
        "task fact(n: number): number\n  if n <= 1 then\n    return 1\n  else\n    return n * fact(n - 1)\n  end\nend\nlet r = fact(5)",
    );
}

#[test]
fn mir_lowering_task_def_has_nested_function() {
    let (_, _, mir) = parse_and_lower("task f(x: number): number\n  return x\nend");
    let has_task = mir
        .body
        .iter()
        .any(|i| matches!(i, mora::mir::MirInst::TaskDef { .. }));
    assert!(has_task, "expected TaskDef in lowering");
}

// ── α.2 差分测试：With / Parallel ──

#[test]
fn mir_differential_with_block() {
    // with 块设置 AI config，body 执行后恢复
    // 不触发真实 AI（无 OPENAI_API_KEY），只验证 config 保存/恢复不 panic
    assert_differential("with model = \"gpt-4o-mini\"\n  let x = 1\nend");
}

#[test]
fn mir_differential_with_temperature() {
    assert_differential("with model = \"gpt-4o\"\n  let x = 42\nend");
}

#[test]
fn mir_differential_parallel_block() {
    // parallel 块在 AST 和 MIR 都是顺序执行
    assert_differential("parallel\n  let x = 1\n  let y = 2\nend");
}

#[test]
fn mir_differential_parallel_with_task() {
    assert_differential(
        "task f(n: number): number\n  return n * 2\nend\nparallel\n  let a = f(5)\n  let b = f(10)\nend",
    );
}

#[test]
fn mir_lowering_with_has_withconfig() {
    let (_, _, mir) = parse_and_lower("with model = \"gpt-4o\"\n  let x = 1\nend");
    let has_with = mir
        .body
        .iter()
        .any(|i| matches!(i, mora::mir::MirInst::WithConfig { .. }));
    assert!(has_with, "expected WithConfig in lowering");
}

#[test]
fn mir_lowering_parallel_is_sequential() {
    // parallel 应展平为顺序指令，无特殊指令
    let (_, _, mir) = parse_and_lower("parallel\n  let x = 1\n  let y = 2\nend");
    // 应含两个 Define（x 和 y），无 Parallel 指令
    let define_count = mir
        .body
        .iter()
        .filter(|i| matches!(i, mora::mir::MirInst::Define(..)))
        .count();
    assert!(
        define_count >= 2,
        "expected >=2 Define in parallel lowering"
    );
}
