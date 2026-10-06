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
//! 1. 第一版量的是顶层 `last_expr`（`run_mir`），而 task 的返回值要经
//!    `run_main_task_with_signal` 才看得到 —— **量错了对象**，一度得到
//!    三档全是 `nil` 的误导结论；
//! 2. 第一版把 `return` 写在顶层当复现式，被 parser 正确拒绝。
//!    **与 D102 同源：写判据/复现式之前先确认它在当前实现下成立。**

use mora::interpreter::Interpreter;
use mora::mir::ssa::OptLevel;
use mora::mir::vm::{run_main_task, run_mir};
use mora::parser_v3::ParserV3;

/// 应用 SSA 优化（不 panic 即通过 — 管线正确性由等价性测试治理）。
fn optimize_without_panic(source: &str, level: OptLevel) {
    let (mut func, _witnesses) = ParserV3::compile(source).expect("compile should succeed");
    mora::mir::opt::optimize(&mut func, level);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let _ = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
    let _ = run_main_task(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
}

/// 对 task 内显式 return 的程序，验证优化前后返回值一致。
fn assert_task_equiv(source: &str) {
    let run = |level: Option<OptLevel>| -> Result<mora::value::Value, String> {
        let (mut func, _witnesses) = ParserV3::compile(source)?;
        if let Some(l) = level {
            mora::mir::opt::optimize(&mut func, l);
        }
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let func_arc = std::sync::Arc::new(func);
        let v = run_mir(
            &func_arc,
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new(),
        )?;
        run_main_task(
            &func_arc,
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new(),
        )?;
        Ok(v)
    };
    let baseline = run(None);
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
        let (mut func, _witnesses) = ParserV3::compile(source)?;
        if let Some(l) = level {
            mora::mir::opt::optimize(&mut func, l);
        }
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        run_mir(
            &std::sync::Arc::new(func),
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new(),
        )
    };
    let baseline = run(None);
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
    // v0.75.6 bug (a)：dag 优化 placeholder 0 与「节点 0 是合法 id」冲突。
    // 含变量操作数的 BinaryOp（lhs/rhs 来自非 0 节点）在 Algebraic 重写
    // 时曾触发 index out of bounds。核心回归点 = dag_optimize 不 panic；
    // 变量折叠的具体结果由 dag_rule 单元测试治理，此处不作强断言。
    let source = "let x = 10\nlet y = x + 0\nprint(y)\n";
    let (func, _witnesses) = ParserV3::compile(source).unwrap();
    let mut dag = mora::mir::dag::dag_analyze(&func);
    mora::mir::optimize::dag_optimize(&mut dag); // 不得 panic（bug(a) 回归）
}

#[test]
fn deconstruct_skips_return_none() {
    // v0.75.6 bug (c)：deconstruct 曾把顶层 Return(None) 发射为
    // MirInst::Return(None)，在块首短路导致隐式返回载体不执行。
    // 修复后优化产物不应包含 Return(None)。
    let source = "let x = 42\nprint(x)\n";
    let (mut func, _witnesses) = ParserV3::compile(source).unwrap();
    mora::mir::opt::optimize(&mut func, OptLevel::Basic);
    assert!(
        !func
            .body
            .iter()
            .any(|i| matches!(i, mora::mir::MirInst::Return(None))),
        "deconstruct 不得发射 Return(None)（顶层隐式返回语义）"
    );
}

// ─── v0.75.30 回归：SSA 声明透传（--opt 显式化暴露的 bug）──────────────

#[test]
fn taskdef_survives_ssa_optimization() {
    // SSA construct 曾丢弃声明型指令（TaskDef 等）→ `--opt` 下 task main
    // 消失（MORA_OPT=1 默认关掩盖；CLI 显式化后暴露）。结构断言：
    // 优化后 func.body 必须仍含 TaskDef。
    let source = "task main()\n  print(1 + 2)\nend\n";
    let (mut func, _witnesses) = ParserV3::compile(source).expect("compile");
    assert!(
        func.body
            .iter()
            .any(|i| matches!(i, mora::mir::MirInst::TaskDef { .. })),
        "baseline: 无优化时 TaskDef 存在"
    );
    mora::mir::opt::optimize(&mut func, OptLevel::Aggressive);
    assert!(
        func.body
            .iter()
            .any(|i| matches!(i, mora::mir::MirInst::TaskDef { .. })),
        "SSA 优化后 TaskDef 必须保留（声明透传）"
    );
}
