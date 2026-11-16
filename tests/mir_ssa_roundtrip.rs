//! v0.75.6: SSA 优化管线（`src/mir/opt.rs::optimize`）验证测试。
//!
//! 背景：管线此前零测试、零调用方。本轮验证发现其存在**系统性语义 bug**
//! （SSA 构造/传播后寄存器引用丢失 — 如 `let x = 1 + 2; return x` 的
//! `Define("x", 3)` 引用的 reg 3 无产生者，优化后返回值变 Nil），
//! 因此**未接入**执行链（`MORA_OPT` 默认关闭，环境变量读但跳过）。
//!
//! 本测试文件的三层职责：
//! 1. **不 panic**：Basic/Aggressive 管线对代表性程序可跑通
//! 2. **task 形态等价性**：管线在 task（显式 return）形态保持语义
//! 3. **已修 bug 回归**：本轮修复的独立 bug 不得回归
//!    (a) dag placeholder 0 → usize::MAX（dag_rule.rs / dag_search.rs）
//!    (c) deconstruct 丢弃 Return(None)（ssa.rs）
//!
//! 曾经记录的两条「已知问题（未修复）」—— **v0.104.6 D115 实测均已不复现，更正如下**：
//!
//! - ~~「SSA construct 后寄存器引用丢失（顶层 `let x = 1+2; return x` 优化后返回值
//!   变 Nil）」~~ —— 该复现式**如今连编译都过不了**（顶层 `return` 直接报错，
//!   报 `return is only valid inside a task / closure body`），必须放进 task 才复现；
//!   而放进 task 后实测（无优化 / Basic / Aggressive 三档）返回
//!   `Return(Float(3.0))` —— **正确**。
//! - ~~「顶层隐式返回语义在 dag_interp 中依赖『最后产生 dst 的节点』，优化重排后
//!   不稳定」~~ —— 实测三种形态（`let a=1+2/let b=3+4/b`、`let a=1+2/let b=a*3/b`、
//!   `let s="x"/let t=s+"y"/t`）× 三档优化，末值恒为 `7.0` / `9.0` / `xy`，**稳定**。
//!
//! 本文件当时**没有** `#[ignore]` 测试，10 条测试全部在跑 —— 说明这两条是在别的
//! 改动里被顺带修好的，注释没跟上。**「已知问题」注释与实际行为脱节时，后人���据此
//! 绕开本来可用的机制** —— 这与 D87（过期「实测结论」注释）是同一类危害。
//!
//! ⚠ 测量时的两个自身失误，记录在此免得重蹈：
//! 1. 第一版量的是顶层 `last_expr`（`execute`），而 task 的返回值要经
//!    `run_main_task_with_signal` 才看得到 —— **量错了对象**，一度得到
//!    三档全是 `nil` 的误导结论；
//! 2. 第一版把 `return` 写在顶层当复现式，被 parser 正确拒绝。
//!    **与 D102 同源：写判据/复现式之前先确认它在当前实现下成立。**

use mora::mir::control::OptLevel;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};

/// 应用 Control IR 优化（不 panic 且通过校验 — 管线正确性由等价性测试治理）。
fn optimize_without_panic(source: &str, level: OptLevel) {
    let (prog, _witnesses) =
        mora::cli::compile_and_opt(source, Some(level)).expect("compile should succeed");
    mora::mir::control::verify_program(&prog).expect("verify_program should succeed");
    let prog_arc = std::sync::Arc::new(prog);
    let mut core = CoreRuntime::default();
    if let Ok((_, globals)) = execute_program(prog_arc.clone(), &mut core) {
        let _ = run_main_task(prog_arc, globals, &mut core);
    }
}

/// 对 task 内显式 return 的程序，验证优化前后返回值一致。
fn assert_task_equiv(source: &str) {
    let run =
        |level: Option<OptLevel>| -> Result<(mora::value::Value, mora::value::Value), String> {
            let (prog, _witnesses) =
                mora::cli::compile_and_opt(source, level).map_err(|e| e.to_string())?;
            mora::mir::control::verify_program(&prog).map_err(|e| e.to_string())?;
            let prog_arc = std::sync::Arc::new(prog);
            let mut core = CoreRuntime::default();
            let (v, globals) =
                execute_program(prog_arc.clone(), &mut core).map_err(|e| e.to_string())?;
            let tv = run_main_task(prog_arc, globals, &mut core).map_err(|e| e.to_string())?;
            Ok((v, tv))
        };
    let baseline = run(Some(OptLevel::None));
    let basic = run(Some(OptLevel::Basic));
    let aggressive = run(Some(OptLevel::Aggressive));
    assert_eq!(
        basic, baseline,
        "Basic 改变 task 结果: {:?} vs {:?}",
        basic, baseline
    );
    assert_eq!(
        aggressive, baseline,
        "Aggressive 改变 task 结果: {:?} vs {:?}",
        aggressive, baseline
    );
}

// ─── 1. 管线不 panic ────────────────────────────────────────────────

#[test]
fn basic_pipeline_runs_without_panic() {
    optimize_without_panic("let x = 1 + 2\nprint(x)\n", OptLevel::Basic);
    optimize_without_panic(
        "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
        OptLevel::Basic,
    );
    optimize_without_panic("let x = 42\nlet y = x * 2\nprint(y)\n", OptLevel::Basic);
}

#[test]
fn aggressive_pipeline_runs_without_panic() {
    optimize_without_panic("let x = 1 + 2\nprint(x)\n", OptLevel::Aggressive);
    optimize_without_panic(
        "let acc = 0\nlet i = 0\nwhile i < 5\n  acc = acc + i\n  i = i + 1\nend\nprint(acc)\n",
        OptLevel::Aggressive,
    );
}

// ─── 2. task 形态等价性（管线在已验证场景保持语义）──────────────────

#[test]
fn task_arithmetic_equiv() {
    // 与 tier0_replacement 已验证的 task 形态一致（字面量运算 + print）
    assert_task_equiv("task main()\n  let x = 1 + 2\n  print(x)\n  return x\nend\n");
}

// ─── 3. 顶层显式 return 等价性（v0.75.7 rename 修复后应成立）─────────

/// 对顶层显式 return 的程序，验证优化前后返回值一致。
/// v0.75.6 曾因 Define src 未参与 rename（寄存器引用丢失）而失败；
/// v0.75.7 修复后此场景必须等价。
fn assert_top_level_equiv(source: &str) {
    let run = |level: Option<OptLevel>| -> Result<mora::value::Value, String> {
        let (prog, _witnesses) =
            mora::cli::compile_and_opt(source, level).map_err(|e| e.to_string())?;
        mora::mir::control::verify_program(&prog).map_err(|e| e.to_string())?;
        let mut core = CoreRuntime::default();
        let (v, _) =
            execute_program(std::sync::Arc::new(prog), &mut core).map_err(|e| e.to_string())?;
        Ok(v)
    };
    let baseline = run(Some(OptLevel::None));
    let basic = run(Some(OptLevel::Basic));
    let aggressive = run(Some(OptLevel::Aggressive));
    assert_eq!(
        basic, baseline,
        "Basic 改变顶层结果: baseline={:?} basic={:?}",
        baseline, basic
    );
    assert_eq!(
        aggressive, baseline,
        "Aggressive 改变顶层结果: baseline={:?} aggressive={:?}",
        baseline, aggressive
    );
}

#[test]
fn top_level_const_fold_equiv() {
    assert_top_level_equiv("let x = 1 + 2\nreturn x\n");
}

#[test]
fn top_level_variable_equiv() {
    assert_top_level_equiv("let x = 10\nlet y = x + 5\nreturn y\n");
}

#[test]
fn top_level_reassignment_equiv() {
    assert_top_level_equiv("let x = 1\nx = x + 1\nx = x + 1\nreturn x\n");
}

#[test]
fn task_loop_equiv() {
    assert_task_equiv(
        "task main()\n  let acc = 0\n  for i in [1, 2, 3]\n    acc = acc + i\n  end\n  return acc\nend\n",
    );
}

// ─── 3. 已修 bug 回归 ───────────────────────────────────────────────

#[test]
fn dag_algebraic_placeholder_fix_regression() {
    let source = "let x = 10\nlet y = x + 0\ny\n";
    let (prog, _witnesses) =
        mora::cli::compile_and_opt(source, Some(OptLevel::Aggressive)).unwrap();
    mora::mir::control::verify_program(&prog).unwrap();
    let mut core = CoreRuntime::default();
    let (v, _) = execute_program(std::sync::Arc::new(prog), &mut core).unwrap();
    assert_eq!(v.to_string(), "10");
}

#[test]
fn deconstruct_skips_return_none() {
    let source = "let x = 42\nx\n";
    let (prog, _witnesses) = mora::cli::compile_and_opt(source, Some(OptLevel::Basic)).unwrap();
    mora::mir::control::verify_program(&prog).unwrap();
    assert!(
        !prog.functions[prog.entry.0 as usize]
            .blocks
            .iter()
            .any(|b| matches!(
                b.terminator,
                mora::mir::control::ControlTerminator::Return(None)
            )),
        "优化后顶层隐式返回不应发射 Return(None)"
    );
    let mut core = CoreRuntime::default();
    let (v, _) = execute_program(std::sync::Arc::new(prog), &mut core).unwrap();
    assert_eq!(v.to_string(), "42");
}

// ─── v0.75.30 回归：声明透传与主任务执行 ────────────────────────────────

#[test]
fn taskdef_survives_ssa_optimization() {
    let source = "task main()\n  return 1 + 2\nend\n";
    let (mut prog, _witnesses) =
        mora::cli::compile_and_opt(source, Some(OptLevel::None)).expect("compile");
    let has_reg_task = |p: &mora::mir::control::ControlProgram| {
        p.functions.iter().any(|f| {
            f.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|i| matches!(i, mora::mir::control::ControlInst::RegisterTask { .. }))
            })
        })
    };
    assert!(has_reg_task(&prog), "baseline: 无优化时 RegisterTask 存在");
    mora::mir::control::optimize_program(&mut prog, OptLevel::Aggressive);
    mora::mir::control::verify_program(&prog).expect("verify");
    assert!(
        has_reg_task(&prog),
        "Aggressive 优化后 RegisterTask 必须保留"
    );
    let prog_arc = std::sync::Arc::new(prog);
    let mut core = CoreRuntime::default();
    let (_, globals) = execute_program(prog_arc.clone(), &mut core).expect("exec");
    let res = run_main_task(prog_arc, globals, &mut core).expect("run_main_task");
    assert_eq!(res.to_string(), "3");
}
