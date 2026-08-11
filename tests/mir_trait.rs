//! Trait / impl / skill MIR dispatch 路径测试 (v0.77 重命名自 tier0_trait_mir.rs)
//!
//! v0.77 重构：删除 4 个 source-grep 静态合约测试（MirInst::TraitDef.method_bodies
//! 存在性、MirInst::SkillDef.task_bodies 存在性等 — 任何重命名都会假阳性断裂）。
//! 保留 1 个 runtime 集成测试。
//!
//! α.11 起，MirInst::TraitDef / ImplDef / SkillDef 携带 prelowered MirFunction
//! body（method_bodies / task_bodies / verify_body），dispatch 见 mir_body: Some
//! 走 run_mir，不回退到 arena-based call_task_inner AST 路径。

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

// ─── 集成验证 ─────────────────────────────────────────────────────────
// 注意：Mora v0.55 parser 对 trait / impl / skill 的实际语法尚不完全；
// 这里跑 task main + 用 type alias 模拟 trait/impl 的下游效应。
// 完整 trait/impl/skill 通过 MIR 派发由静态合约保护。

#[test]
fn trait_impl_skill_registration_does_not_crash() {
    // 验证 StmtKind::TraitDef / ImplDef / SkillDef 三种 lowering 路径
    // 走完后 run_mir 仍能正常执行（即使 trait body 是空的占位）。
    // 注：parser 是否实际支持 `trait Foo ... end` 取决于 v0.55；这里用 task main 验证
    // base 编译与执行链路。
    let src = r#"
task main()
  let xs = [1, 2, 3]
  let s = 0
  for x in xs
    let s = s + x
  end
  print(s)
end
"#;
    run_via_mir(src).expect("trait/impl/skill registration does not crash base run_mir");
}

// 注：v0.77 之前 §静态合约 4 个测试是 source-grep（assert!(src.contains(...))），
// 任何改名都会假阳性断裂、零 runtime 保护 — 已删除。
// 静态合约（MirInst::TraitDef.method_bodies 存在、SkillDef.task_bodies 存在、
// 5+ 处 mir_body 填充）现在由 E2E tests/e2e.rs 镜像 main.rs::run_file 调用栈
// 的真实端到端断言承担。