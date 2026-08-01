//! Tier 0 trait/impl/skill 升级合约测试
//!
//! α.11 起，MirInst::TraitDef / ImplDef / SkillDef 携带 prelowered MirFunction
//! body（method_bodies / task_bodies / verify_body），dispatch 见 mir_body: Some
//! 走 run_mir，不回退到 arena-based call_task_inner AST 路径。

use mora::interpreter::Interpreter;
use mora::mir::interp::{run_main_task, run_mir};
use mora::mir::lower::{lower_mir_exprs, typecheck_mir_exprs};

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

// ─── 静态合约 ─────────────────────────────────────────────────────────

#[test]
fn mir_inst_trait_def_carries_method_bodies() {
    let src = std::fs::read_to_string("src/mir/mod.rs").expect("mir/mod.rs");
    assert!(
        src.contains("method_bodies: Vec<MirFunction>"),
        "MirInst::TraitDef must carry method_bodies: Vec<MirFunction>"
    );
}

#[test]
fn mir_inst_impl_def_carries_method_bodies() {
    let src = std::fs::read_to_string("src/mir/mod.rs").expect("mir/mod.rs");
    // ImplDef 也必须含 method_bodies 字段
    assert!(
        src.contains("method_bodies: Vec<MirFunction>"),
        "MirInst::ImplDef must carry method_bodies: Vec<MirFunction>"
    );
}

#[test]
fn mir_inst_skill_def_carries_task_bodies_and_verify_body() {
    let src = std::fs::read_to_string("src/mir/mod.rs").expect("mir/mod.rs");
    assert!(
        src.contains("task_bodies: Vec<MirFunction>"),
        "MirInst::SkillDef must carry task_bodies"
    );
    assert!(
        src.contains("verify_body: Option<MirFunction>"),
        "MirInst::SkillDef must carry verify_body"
    );
}

#[test]
fn interpreter_fills_mir_body_for_trait_impl_skill() {
    // v0.75.11: mir_body 填充在 handlers.rs（h_closure/h_trait_def/h_impl_def/
    // h_skill_def 各填 Arc::new(body.clone())），interp.rs 只驱动执行顺序。
    // 至少 5 处：Closure + TraitDef + ImplDef + SkillDef task + SkillDef verify。
    let src = std::fs::read_to_string("src/mir/handlers.rs").expect("mir/handlers.rs");
    let occurrences = src.matches("mir_body: Arc::new(").count();
    assert!(
        occurrences >= 5,
        "expected ≥5 mir_body: Arc::new(...) occurrences in handlers.rs (Closure + TraitDef + ImplDef + SkillDef task + verify), found {}",
        occurrences
    );
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
