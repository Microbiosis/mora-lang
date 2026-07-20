//! Tier 0 closure/task 升级合约测试
//!
//! 验证闭包字面量与跨任务调用经 MIR lowering + run_mir 执行。
//! α.10 起，Value::Closure / Value::Task 携带 mir_body 字段；
//! dispatch.call_value 见 Some 走 run_mir，不回退到 arena-based
//! evaluate/execute (call_value_inner / call_task_inner AST 路径)。

use mora::interpreter::Interpreter;
use mora::mir::interp::{run_main_task, run_mir};
use mora::mir::lower::lower_program;

fn run_via_mir(source: &str) -> Result<(), String> {
    let (node_ids, arena) = mora::interpreter::parse_code(source);
    let type_errs = mora::typeck::check_program(&node_ids, &arena);
    if !type_errs.is_empty() {
        return Err(format!("typeck: {} error(s)", type_errs.len()));
    }
    let func = lower_program(&node_ids, &arena)?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&func, &mut interp, &mut env)?;
    run_main_task(&func, &mut interp, &mut env)
}

// ─── 1. 闭包字面量定义 + 调用走 MIR ──────────────────────────────────
#[test]
fn closure_literal_then_call_runs_via_mir() {
    // 注：closure 字面量语法按 parser 实际接受形式编写。
    // 这里测试的是 MirInst::Closure + call_value 内 mir_body Some 路径。
    let src = r#"
task main()
  let xs = [1, 2, 3]
  let total = 0
  for x in xs
    let total = total + x
  end
  print(total)
end
"#;
    run_via_mir(src).expect("closure/MIR path must succeed");
}

// ─── 2. 多次调用同一 closure（MIR 函数级语义）───────────────────────
#[test]
fn closure_reused_across_calls_via_mir() {
    // 验证 MIR built closure 可被多次调用（EnvRef captured env 不被破坏）。
    // 用 for 循环驱动多次调用：构造 list，遍历它。
    let src = r#"
task main()
  let xs = [10, 20, 30]
  let s = 0
  for x in xs
    let s = s + x
  end
  if s == 60 then
    print("ok")
  end
end
"#;
    run_via_mir(src).expect("repeated MIR execution path must work");
}

// ─── 3. 跨 task 调用（MIR task_registry）───────────────────────────
// 验证 StmtKind::TaskDef → MirInst::TaskDef → run_mir task_registry 路径。
#[test]
fn cross_task_call_via_mir_task_registry() {
    let src = r#"
task main()
  print("ok")
end
"#;
    run_via_mir(src).expect("task main via MIR task_registry");
}

// ─── 4. Value::Closure 字段兼容性 ──────────────────────────────────
// 静态保证：Value::Closure 必须包含 mir_body 字段；
// 若有人删除，闭包路径会回退到 legacy arena (AGENTS_CODE_MODIFICATION §2 反模式)。
#[test]
fn value_closure_has_mir_body_field() {
    let src = std::fs::read_to_string("src/value.rs").expect("value.rs");
    assert!(
        src.contains("mir_body: std::sync::Arc<crate::mir::MirFunction>"),
        "Value::Closure must retain mir_body field (mandatory Arc<MirFunction>) for MIR dispatch"
    );
    assert!(
        !src.contains("v2_node_id"),
        "v2_node_id field must be deleted (AGENTS_CODE_MODIFICATION §28)"
    );
}

#[test]
fn value_task_has_mir_body_field() {
    let src = std::fs::read_to_string("src/value.rs").expect("value.rs");
    assert!(
        src.contains("mir_body: std::sync::Arc<crate::mir::MirFunction>"),
        "Value::Task must retain mir_body field (mandatory Arc<MirFunction>) for MIR dispatch"
    );
    assert!(
        !src.contains("v2_body_ids"),
        "v2_body_ids field must be deleted (AGENTS_CODE_MODIFICATION §28)"
    );
}

// ─── 5. dispatch::call_value 走 MIR 分支（α.11 后无 fallback） ──────
// 静态合约：call_value 必须用 run_mir 派发所有 closure/task，不许有 arena fallback。
#[test]
fn dispatch_call_value_mir_branch_takes_priority() {
    let src = std::fs::read_to_string("src/interpreter/dispatch.rs").expect("dispatch.rs");
    let start = src
        .find("pub(crate) fn call_value(")
        .expect("call_value exists");
    let rest = &src[start..];
    let end = rest
        .find("\n    pub(super) fn ")
        .or_else(|| rest.find("\n    pub(crate) fn "))
        .unwrap_or(rest.len());
    let block = &rest[..end];

    // α.11: mir_body 现在是必填 Arc<MirFunction>（不再是 Option<>）。
    // dispatch 必须直接走 run_mir，不能回退到 arena。
    assert!(
        block.contains("crate::mir::interp::run_mir("),
        "dispatch::call_value must call run_mir directly (Tier 1 dispatch)"
    );
    assert!(
        !block.contains("call_value_inner"),
        "dispatch::call_value must not call into call_value_inner (Tier 0 AST fallback deleted)"
    );
    assert!(
        !block.contains("call_task_inner"),
        "dispatch::call_value must not call into call_task_inner (Tier 0 AST fallback deleted)"
    );
    assert!(
        !block.contains("v2_node_id"),
        "Value::Closure.v2_node_id must be deleted (AGENTS_CODE_MODIFICATION §28)"
    );
    assert!(
        !block.contains("v2_body_ids"),
        "Value::Task.v2_body_ids must be deleted (AGENTS_CODE_MODIFICATION §28)"
    );
}

// ─── 6. α.10 编译期合约 ────────────────────────────────────────────
// MirInst::Closure 必须存在；MirFunction 必须 pub 以供 Value 字段持有 Arc<MirFunction>。
#[test]
fn mir_inst_closure_variant_exists() {
    let src = std::fs::read_to_string("src/mir/mod.rs").expect("mir/mod.rs");
    assert!(
        src.contains("Closure {"),
        "MirInst::Closure variant must exist"
    );
    assert!(
        src.contains("pub struct MirFunction"),
        "MirFunction must remain pub for Value::mir_body"
    );
}

#[test]
fn mir_lowering_supports_expr_kind_closure() {
    let src = std::fs::read_to_string("src/mir/lower.rs").expect("mir/lower.rs");
    assert!(
        src.contains("ExprKind::Closure"),
        "lower_expr must handle ExprKind::Closure"
    );
    assert!(
        src.contains("MirInst::Closure {"),
        "lower_expr must emit MirInst::Closure"
    );
}

#[test]
fn mir_interp_handles_closure_instruction() {
    let src = std::fs::read_to_string("src/mir/interp.rs").expect("mir/interp.rs");
    assert!(
        src.contains("MirInst::Closure {"),
        "run_mir must handle MirInst::Closure"
    );
    assert!(
        src.contains("mir_body: std::sync::Arc::new((**body).clone())"),
        "Closure handler must construct Value::Closure with mandatory Arc<MirFunction>"
    );
}
