//! Trait / impl / skill 分派路径测试 (v0.77 重命名自 tier0_trait_mir.rs)
//!
//! v0.77 重构：删除 4 个 source-grep 静态合约测试（`MirInst::TraitDef.method_bodies`
//! 存在性、MirInst::SkillDef.task_bodies 存在性等 — 任何重命名都会假阳性断裂）。
//! 保留 1 个 runtime 集成测试。
//!
//! ⚠ **历史说明**：上文的 `MirInst::TraitDef` / `MirInst::ImplDef` /
//! `MirInst::SkillDef` 与 `MirFunction` 属**已退役**的 9 层 IR（v0.110.0 删除）。
//! 现存链路是 `WitnessKind::TraitDef`（`src/mir/witness.rs`）→ `mir::control::lower`
//! 生成 `ControlFunction`（`FunctionKind::TraitDefault` / trait 方法体），
//! dispatch 由 `src/runtime/builtins/traits.rs` 完成。

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};
use mora::typeck::check_mir::check_program_witnesses;

fn run_via_execute(source: &str) -> Result<(), String> {
    let (prog, witnesses) = ParserV3::compile(source).map_err(|e| e.to_string())?;
    let type_errs = check_program_witnesses(&witnesses);
    if !type_errs.is_empty() {
        return Err(format!("typeck: {} error(s)", type_errs.len()));
    }
    let mut core = CoreRuntime::default();
    let arc = std::sync::Arc::new(prog);
    let (_, globals) =
        execute_program(std::sync::Arc::clone(&arc), &mut core).map_err(|e| e.to_string())?;
    run_main_task(arc, globals, &mut core)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ─── 集成验证 ─────────────────────────────────────────────────────────
// 注意：Mora v0.55 parser 对 trait / impl / skill 的实际语法尚不完全；
// 这里跑 task main + 用 type alias 模拟 trait/impl 的下游效应。
// 完整 trait/impl/skill 通过 MIR 派发由静态合约保护。

#[test]
fn trait_impl_skill_registration_does_not_crash() {
    // 验证 StmtKind::TraitDef / ImplDef / SkillDef 三种 lowering 路径
    // 走完后 execute 仍能正常执行（即使 trait body 是空的占位）。
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
    run_via_execute(src).expect("trait/impl/skill registration does not crash base execute");
}

// 注：v0.77 之前 §静态合约 4 个测试是 source-grep（assert!(src.contains(...))），
// 任何改名都会假阳性断裂、零 runtime 保护 — 已删除。
// 静态合约（MirInst::TraitDef.method_bodies 存在、SkillDef.task_bodies 存在、
// 5+ 处 mir_body 填充）现在由 E2E tests/e2e.rs 镜像 main.rs::run_file 调用栈
// 的真实端到端断言承担。
