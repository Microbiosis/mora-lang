//! DynTrait MIR 路径测试 (v0.77 重命名自 tier0_dyntrait.rs)
//!
//! v0.77 重构：删除 4 个 source-grep 静态合约测试（MirInst::DynTrait 存在性、
//! lower_expr 处理 ExprKind::DynTrait、handlers 构造 TraitObject、lexer 支持
//! as/dyn 关键字 — 任何重命名都会假阳性断裂）。保留 2 个 runtime 测试。
//!
//! α.12 验证 DynTrait cast 表达式从 parser → lowering → interp 完整链路，
//! 构造 Value::TraitObject 包内嵌 expr。

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

// ─── 1. `expr as dyn Trait` 解析 + 构造 Value::TraitObject ──────────
#[test]
fn dyntrait_cast_parses_and_lowers() {
    let src = r#"
task main()
  let x = 42
  let obj = x as dyn Any
  print(obj)
end
"#;
    run_via_mir(src).expect("dyn Trait cast must parse and execute via MIR");
}

// ─── 2. 嵌套 dyn trait cast (chained) ─────────────────────────────
#[test]
fn dyntrait_chained_cast() {
    let src = r#"
task main()
  let n = 1
  let obj1 = n as dyn Any
  let obj2 = obj1 as dyn Any
  print(obj2)
end
"#;
    run_via_mir(src).expect("chained dyn Trait cast must work");
}

// 注：v0.77 之前 §3 静态合约 4 个测试是 source-grep（assert!(src.contains(...))），
// 任何改名都会假阳性断裂、零 runtime 保护 — 已删除。
// 静态合约（MirInst::DynTrait 存在、lower_expr 处理 ExprKind::DynTrait、
// handlers 构造 TraitObject、lexer 支持 as/dyn 关键字）现在由
// E2E tests/e2e.rs 镜像 main.rs::run_file 调用栈的真实端到端断言承担。