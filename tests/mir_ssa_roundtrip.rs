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
//! 已知问题（未修复，记录于此）：
//! - SSA construct 后寄存器引用丢失（顶层 `let x = 1+2; return x`
//!   优化后返回值变 Nil）→ 管线不接入
//! - 顶层隐式返回语义在 dag_interp 中依赖「最后产生 dst 的节点」，
//!   优化重排后不稳定（独立于 SSA 管线）

use mora::interpreter::{Interpreter, parse_code_v3};
use mora::mir::MirFunction;
use mora::mir::interp::{run_main_task, run_mir};
use mora::mir::lower::lower_mir_exprs;
use mora::mir::ssa::OptLevel;

/// 应用 SSA 优化（不 panic 即通过 — 管线正确性由等价性测试治理）。
fn optimize_without_panic(source: &str, level: OptLevel) {
    let exprs = parse_code_v3(source).expect("parse should succeed");
    let mut func: MirFunction = lower_mir_exprs(&exprs).expect("lower should succeed");
    mora::mir::opt::optimize(&mut func, level);
    // deconstruct 后仍可执行（不 panic）
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let func_arc = std::sync::Arc::new(func);
    let _ = run_mir(&func_arc, &mut interp, &mut env);
    let _ = run_main_task(&func_arc, &mut interp, &mut env);
}

/// 对 task 内显式 return 的程序，验证优化前后返回值一致。
fn assert_task_equiv(source: &str) {
    let run = |level: Option<OptLevel>| -> Result<mora::value::Value, String> {
        let exprs = parse_code_v3(source).expect("parse");
        let mut func: MirFunction = lower_mir_exprs(&exprs).expect("lower");
        if let Some(l) = level {
            mora::mir::opt::optimize(&mut func, l);
        }
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存
        let func_arc = std::sync::Arc::new(func);
        let v = run_mir(&func_arc, &mut interp, &mut env)?;
        run_main_task(&func_arc, &mut interp, &mut env)?;
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
        let exprs = parse_code_v3(source).expect("parse");
        let mut func: MirFunction = lower_mir_exprs(&exprs).expect("lower");
        if let Some(l) = level {
            mora::mir::opt::optimize(&mut func, l);
        }
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存
        run_mir(&std::sync::Arc::new(func), &mut interp, &mut env)
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
    let exprs = parse_code_v3(source).unwrap();
    let func: MirFunction = lower_mir_exprs(&exprs).unwrap();
    let mut dag = mora::mir::dag::dag_analyze(&func);
    mora::mir::optimize::dag_optimize(&mut dag); // 不得 panic（bug(a) 回归）
}

#[test]
fn deconstruct_skips_return_none() {
    // v0.75.6 bug (c)：deconstruct 曾把顶层 Return(None) 发射为
    // MirInst::Return(None)，在块首短路导致隐式返回载体不执行。
    // 修复后优化产物不应包含 Return(None)。
    let source = "let x = 42\nprint(x)\n";
    let exprs = parse_code_v3(source).unwrap();
    let mut func: MirFunction = lower_mir_exprs(&exprs).unwrap();
    mora::mir::opt::optimize(&mut func, OptLevel::Basic);
    assert!(
        !func
            .body
            .iter()
            .any(|i| matches!(i, mora::mir::MirInst::Return(None))),
        "deconstruct 不得发射 Return(None)（顶层隐式返回语义）"
    );
}
