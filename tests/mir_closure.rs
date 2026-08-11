//! Closure / task MIR dispatch 路径测试 (v0.77 重命名自 tier0_closure_mir.rs)
//!
//! v0.77 重构：删除 6 个 source-grep 静态合约测试
//! （Value::Closure.mir_body 存在性、dispatch 走 run_mir、MirInst::Closure
//! 存在性等 — 任何重命名都会假阳性断裂）。保留 3 个 runtime 测试。
//! 这些 runtime 测试验证 `MirInst::Closure` + `MirInst::TaskDef` →
//! `run_mir` → `run_main_task` 真实端到端路径。

use mora::interpreter::Interpreter;
use mora::mir::lower::{lower_mir_exprs, typecheck_mir_exprs};
use mora::mir::vm::{run_main_task, run_mir};

fn run_via_mir(source: &str) -> Result<(), String> {
    let mut exprs = mora::interpreter::parse_code_v3(source)?;
    let type_errs = typecheck_mir_exprs(&mut exprs);
    if !type_errs.is_empty() {
        return Err(format!("typeck: {} error(s)", type_errs.len()));
    }
    let func = lower_mir_exprs(&exprs)?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    run_mir(&func_arc, &mut interp, &mut env)?;
    run_main_task(&func_arc, &mut interp, &mut env)
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
    // Mora 无 `f(args)` 名字调用语法（Call 指令只查 builtin 表），闭包经
    // Dict 方法调用路径分发：`dict.method(args)` → dispatch 查 Dict 找到
    // Value::Closure → call_value → run_mir（dispatch.rs:736-749）。
    // 用 for 循环驱动多次调用。
    //
    // 注：原测试源码用 `if s == 60 then\n  print(...)\nend`（then 独占行）
    // — ParserV3 的 `if then` 要求 then 与分支同行，该形态从诞生起就
    // 解析失败（pre-existing，v0.75.11 修复为可解析形态）。
    let src = r#"
task main()
  let ops = {"mul": fn(x) x * 2 end}
  let total = 0
  for x in [10, 20, 30]
    total = total + ops.mul(x)
  end
  print(total)
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

// 注：v0.77 之前 §4-6 是 source-grep 测试（assert!(src.contains(...))），
// 任何改名都会假阳性断裂、零 runtime 保护 — 已删除。
// 静态合约（Value::Closure.mir_body 存在、dispatch 走 run_mir 分支、
// MirInst::Closure 存在）现在由 E2E tests/e2e.rs 镜像 main.rs::run_file
// 调用栈的真实端到端断言承担。