//! v0.104.6 D40：`transaction` 作为 task 体的唯一语句 → **panic**（exit=101）。
//!
//! ## 现象（修前，真实 CLI `mora run` 实测）
//!
//! ```mora
//! task w()
//!   transaction
//!     1
//!   end
//! end
//! w()
//! ```
//!
//! ```text
//! thread 'mora-main' panicked at src\mir\vm\dag.rs:645:73:
//! index out of bounds: the len is 0 but the index is 0
//! exit=101
//! ```
//!
//! 程序能解析、能通过 typeck，**却把整个进程打掉**。
//!
//! ## 根因：unit 语句返回「未分配的哨兵寄存器 0」
//!
//! 寄存器文件长度是 `MirFunction.n_regs`，执行器按 `n_regs` 开数组：
//!
//! ```text
//! let mut reg_ready: Vec<bool> = vec![false; dag.n_regs];
//! ```
//!
//! `transaction` 语句本身在**父上下文**里只 emit 一条无 `dst` 的
//! `MirInst::Transaction`（body / compensation 各自在独立寄存器空间里降维），
//! **父上下文一个寄存器都没分配**。可它却把哨兵 `0` 当作「本语句的值寄存器」
//! 返回（`parser_v3/emit.rs::emit_transaction_w` 末行）。
//!
//! 当它是所在块的**末条语句**时，块级 `emit_tail_return(Some(0))` 就把 reg 0
//! 写进 `Return` —— 而 `n_regs` 仍是 0。`node_ready` 的 `reg_ready[*r]`
//! 于是索引越界。
//!
//! 触发条件是**嵌套 `MirFunction` = 独立寄存器空间**。放在 `for` 体里**不崩**
//! —— 顶层函数还有循环自身的指令分配寄存器，`n_regs ≥ 1`；放进 `task` 体
//! **就崩** —— 那个函数体除了这条 unit 语句什么都没分配，`n_regs == 0`。
//!
//! ## 这是同一个 bug 类的第二处
//!
//! v0.104.2 修过一次**完全相同**的 panic（`emit.rs` 里 `commit` / `rollback`
//! 两处的注释逐字描述了「`Some((0, _))` → n_regs=0 → `node_ready` 越界
//! panic」），但**只补了内层**（事务体自己的寄存器空间，用 `last: Option<Reg>`
//! + `emit_tail_return`），漏了 `transaction` 语句自身这一层。
//!
//! 而 `transaction commit end` 作为 task 体的唯一语句，修前同样 panic。
//!
//! 本文件因此**不只测 `transaction` 一条**，而是把所有「无值 / unit 语句」
//! 逐个放进 task 体当唯一语句 —— 这正是本类缺陷的完整触发面。
//!
//! ## 修法
//!
//! 1. `emit_transaction_w` 末行改为与 `commit` / `rollback` 同一形状：
//!    `alloc_reg()` + `Const(dst, Nil)`，返回真实分配的 `dst`。
//! 2. `run_dag_with_signal_memo` 进主循环**前**一次性校验寄存器引用是否
//!    落在 `n_regs` 内，不一致就返回干净错误。
//!
//! 第 2 条**不是**把 `node_ready` 改成软失败 —— 那更糟：越界寄存器会让该
//! 节点永不就绪，主循环空转到 `MAX_STEPS`（1e7）后**静默**返回 Nil，
//! 静默错值比 panic 难查得多。
//!
//! 前置校验对合法程序零影响：能跑通的程序不可能引用越界寄存器（越界的读会
//! 在 `node_ready` 越界、越界的写会在 `regs[d]` 越界），故它不可能拒绝任何
//! 当前可运行的程序。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::mir::{MirFunction, MirInst};
use mora::value::Value;

/// 走生产路径（`cli::compile_and_opt` = 9 层管线 + 优化 + 差分回落）执行源码。
///
/// 修前这条会 panic —— Rust panic 会带毒整个测试进程，故本文件里
/// 「不 panic」本身就是断言。
fn run(src: &str) -> Result<Value, String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
}

// ===================================================================
// 1) 精确复现
// ===================================================================

/// D40 的原始复现程序：修前 exit=101 panic。
#[test]
fn transaction_as_sole_task_statement_does_not_panic() {
    let src = "task w()\n  transaction\n    1\n  end\nend\nw()\n";
    match run(src) {
        Ok(_) => {}
        Err(e) => panic!("`transaction` 作为 task 体唯一语句应正常跑完，实得: {e}"),
    }
}

/// v0.104.2 的形状：`transaction commit end` 独占 task 体。修前同样 panic
/// —— 那次修复只覆盖了内层。
#[test]
fn transaction_commit_as_sole_task_statement_does_not_panic() {
    let src = "task w()\n  transaction commit\n  end\nend\nw()\n";
    if let Err(e) = run(src) {
        panic!("`transaction commit` 作为 task 体唯一语句应正常跑完，实得: {e}");
    }
}

/// transaction 不是末条语句时（前面已有寄存器分配）一直是对的 —— 钉住
/// 修法没有改坏正常路径。
#[test]
fn transaction_with_sibling_statements_still_runs() {
    let src = "task w()\n  transaction\n    1\n  end\n  transaction\n    2\n  end\nend\nw()\n";
    if let Err(e) = run(src) {
        panic!("多个 transaction 串行应正常跑完，实得: {e}");
    }
}

// ===================================================================
// 2) 类级覆盖：本类缺陷的完整触发面
// ===================================================================

/// 所有「无值 / unit 形态」的 statement，逐个作为 **task 体的唯一语句**。
///
/// 「task 体 + 唯一语句」不是随意挑的形状，而是本类缺陷的**必要条件**：
/// 只有当这个独立寄存器空间里一条寄存器都没分配时 `n_regs` 才是 0。
/// `spec_ebnf_surface.rs` 里那些顶层用例**永远测不到**这一类 —— 它们周围
/// 总有别的语句在分配寄存器。
///
/// 新增 statement 产生式时请把用例加进来：这比逐个手写复现更可靠，
/// 因为「哪些语句是无值的」本身就是会漂移的事实。
#[test]
fn valueless_statements_as_sole_task_statement_all_run() {
    let cases: &[(&str, &str)] = &[
        // (名称, 放进 task 体的语句)
        ("transaction", "transaction\n  1\nend"),
        ("transaction commit", "transaction commit\nend"),
        ("worker", "worker w\n  1\nend"),
        ("parallel", "parallel\n  1\nend"),
        ("observe", "observe tr do\n  1\nend"),
        ("model", "model M\n  count: Int\nend"),
        ("msg", "msg Inc\nend"),
    ];
    let mut failures = Vec::new();
    for (name, stmt) in cases {
        let src = format!("task w()\n  {stmt}\nend\nw()\n");
        if let Err(e) = run(&src) {
            failures.push(format!("  [{name}] {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "这些语句作为 task 体唯一语句时不得 panic / 报错（修前 `transaction` \
         与 `transaction commit` 会 panic，exit=101）：\n{}",
        failures.join("\n")
    );
}

// ===================================================================
// 3) 防御层直接测试
// ===================================================================

/// 寄存器文件与指令引用不一致时，`run_mir` 必须返回**错误**而不是 panic。
///
/// 手工构造一个说谎的 `MirFunction`：`Return(Some(0))` 引用 reg 0，
/// 而 `n_regs = 0`。这是 `transaction` 修前那条链路的**精确形状** ——
/// 把它独立出来，就不必依赖「某条源码恰好触发」来验证防御层。
#[test]
fn regfile_inconsistency_is_a_clean_error_not_a_panic() {
    let func = MirFunction {
        params: vec![],
        body: vec![MirInst::Return(Some(0))],
        // 说谎：body 引用了 reg 0，寄存器文件却是空的
        n_regs: 0,
        ..Default::default()
    };
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let res = run_mir(&arc, &mut interp, &mut env, &mut Effects::new());
    let err = res.expect_err("越界寄存器引用必须返回错误；修前这里是索引越界 panic");
    assert!(
        err.contains("references register 0") && err.contains("0 register"),
        "错误信息应点名越界的寄存器号与实际寄存器数，实得: {err}"
    );
}

/// 写越界同样要拦住 —— 执行器里 `regs[d] = v` 也是直接按下标。
#[test]
fn out_of_range_write_is_a_clean_error() {
    let func = MirFunction {
        params: vec![],
        // Const(3, …) 写 reg 3，但只有 1 个寄存器
        body: vec![MirInst::Const(3, Value::Int(1))],
        n_regs: 1,
        ..Default::default()
    };
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let res = run_mir(&arc, &mut interp, &mut env, &mut Effects::new());
    let err = res.expect_err("越界写必须返回错误；否则 `regs[3]` 索引越界 panic");
    assert!(
        err.contains("references register 3"),
        "错误信息应点名越界写的寄存器号，实得: {err}"
    );
}

/// 边界内不得误伤 —— 引用恰好等于 `n_regs - 1` 是**合法**的。
#[test]
fn in_range_highest_register_is_not_rejected() {
    let func = MirFunction {
        params: vec![],
        body: vec![MirInst::Const(0, Value::Int(7)), MirInst::Return(Some(0))],
        n_regs: 1,
        ..Default::default()
    };
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let v = run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
        .expect("引用最高位寄存器是合法的，前置校验不得误拒");
    assert!(
        matches!(v, Value::Int(7)),
        "返回值应来自 reg 0，实得: {v:?}"
    );
}
