//! v0.104.6 D466：生产路径 `execute` 与 DAG 路径的**真实**等价性护栏。
//!
//! ## 背景：一条曾是假承诺的契约
//!
//! v0.59 起，`src/mir/vm.rs` 的文档写着
//! 「`execute ≡ run_dag`（`dag.add_sequential_edges` 后退化线性）」。该承诺
//! 建立在 `MirDag::add_sequential_edges` 之上 —— 一个按**相邻节点编号** i→i+1
//! 补 `Sequence` 边的方法。
//!
//! ## v0.104.6 D466 结论：那条路径是伪等价，已删除
//!
//! `add_sequential_edges` 的前提「节点按 pc 递增创建 ⇒ 相邻编号即线性序」
//! **在有控制流时不成立**。实测 `for + break` 的 DAG：边数 73 → 107（+34），
//! 新增的 7 条里有 **4 条跨越控制流终结边界**
//! （`[15]Branch→[16]`、`[21]Branch→[22]`、`[24]Jump→[25]`、`[32]Jump→[33]`）。
//! 边传播会跳过 `Branch`/`Jump` 出边（由 handler 选中的 target 激活），却会
//! 激活 `Sequence` 边 —— 伪造的 Sequence 边成了唯一绕过该保护的路径。
//!
//! 后果：`break` / `continue` 被架空（跳出线性链的控制转移在链上失效，循环体
//! 多跑一轮）、if/else 两臂被串行化。实测 3 例分歧，**错的恒是加此方法的一侧**：
//!
//! | 用例 | execute（生产） | 加 `add_sequential_edges` | 正确值 |
//! |---|---|---|---|
//! | if/else | 8.0 | `Bool(false)` | 8.0 |
//! | for + break | 2.0 | 5.0 | 2.0 |
//! | while + continue | 5.0 | 6.0 | 5.0 |
//!
//! 「哪边对」由独立判据确定（`tests/continue_semantics.rs`：带 `continue`
//! = 5.0、其 `if/else` 等价写法 = 5.0、去掉 `continue` = 6.0 ⇒ 5.0 才对）。
//!
//! 该方法在 `src/` 内**零调用**（生产建图是
//! `dag_analyze → dag_optimize → prune_sequence_edges`），且自己声明「不保语义」
//! —— 故**整体删除**，连同依赖它的 3 条 `#[ignore]` 用例。
//!
//! ## 本文件现在测什么
//!
//! A 组（`assert_optimization_preserves_semantics`）：**生产路径**在
//! `dag_optimize` 前后的语义不变 —— 直接比对 `execute` 与 `run_dag`。
//! B 组 6 条：`execute` 与 DAG 路径在真实程序上的等价。
//!
//! 这两组都是**生产链上的**真实等价，是有效的护栏。

use std::sync::Arc;

use mora::common::BinaryOp;
use mora::mir::MirInst;
use mora::value::Value;

/// 生产路径返回值。
fn via_execute(src: &str) -> String {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("compile");
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "type errors: {:?}", errs);
    let arc = Arc::new(func);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let v = mora::mir::vm::execute(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("execute");
    format!("{:?}", v)
}

/// **未优化**路径返回值：`execute_program` 直调 `dag_analyze`，**绕过 `dag_optimize`
/// 与 DAG 缓存**（对比生产链 `dag_analyze → dag_optimize → prune_sequence_edges`）。
///
/// 它只在测试里被调用（`tests/dag_integration.rs`），因此**优化阶段对它完全
/// 不可见**：CSE 跨区域合并、追加节点可达性、死块 entry 过滤这三类缺陷都
/// 只能由它走 `dag_optimize` 才可能暴露。
///
/// 本组用例把两条路线**按值**对比 —— 优化是保语义的重写，两者必须相等。
/// 这是当前缺失的那道护栏。
fn via_execute_program_unoptimized(src: &str) -> String {
    let (func, _witnesses) = mora::parser_v3::ParserV3::compile(src).expect("compile");
    let arc = Arc::new(func);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let v = mora::mir::vm::execute_program(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("execute_program");
    format!("{:?}", v)
}

/// 优化保语义：生产路径（已优化）必须与未优化路径结果一致。
fn assert_optimization_preserves_semantics(label: &str, src: &str) {
    let opt = via_execute(src);
    let raw = via_execute_program_unoptimized(src);
    assert_eq!(
        opt, raw,
        "{}: dag_optimize 改变了语义（已优化={} 未优化={}）",
        label, opt, raw
    );
}

// ── 优化保语义：生产（已优化） ≡ execute_program（未优化）────────────────────
//
// 这组是 v0.104.6 补上的缺失护栏：优化阶段此前没有任何「与未优化结果对比」的
// 断言，CSE 跨区域合并一类**保语义性破坏**因而能一路存活到生产。

#[test]
fn optimize_preserves_if_else_value() {
    assert_optimization_preserves_semantics(
        "if/else as value",
        "let c = 2\nlet x = if c == 1 then 5 else 7 end\nx + 1\n",
    );
}

#[test]
fn optimize_preserves_nested_break() {
    assert_optimization_preserves_semantics(
        "nested for + break",
        "let o = 0\nlet i = 0\nfor a in [1, 2, 3]\n  assign o = o + 1\n  for b in [1, 2]\n    if b == 2 then\n      break\n    end\n    assign i = i + 1\n  end\nend\no * 100 + i\n",
    );
}

#[test]
fn optimize_preserves_while_continue() {
    assert_optimization_preserves_semantics(
        "while + continue",
        "let n = 0\nlet k = 0i\nwhile k < 6i\n  if k == 2i then\n    assign k = k + 1i\n    continue\n  end\n  assign n = n + 1\n  assign k = k + 1i\nend\nn\n",
    );
}

#[test]
fn optimize_preserves_handwritten_mir_chain() {
    // v0.104.6 D9 回归：**手写** `MirFunction`（非源码编译产物）的直线链。
    //
    // 缺陷：`dag_analyze` 的 `reachable` 从 pc 0 出发，但优化会改写入口 ——
    // CSE 把 `1+2` 折叠后，追加的折叠节点成为新 entry，而原入口（节点 0）被标
    // Removed。此时 `reachable` 仍说节点 0/1/2 可达，它们就作为 Sequence
    // 前驱进入就绪门槛，却**永远不会被激活**（`executed[]` 恒 false）→ 节点 3
    // 永不就绪 → 末节点不跑 → 结果停在折叠节点写的 **3**（应 6）。
    //
    // 修：`apply_rewrite` 每次 rewrite 后按**当前 entry** 重算可达集
    // （`MirDag::recompute_reachable_from_entry`），与 entry 迭代到不动点。
    // 即「不能被激活的节点不得参与就绪门槛」。
    //
    // 这条曾两次误判：先补隐式 `Return`（无效，症状与 `if/else` 那个两难不同）、
    // 后整体排除 Removed 前驱（引发循环体块内顺序丢失、D1/D3 回归）。真正的
    // 判据既不是「有无尾 Return」也不是「是否 Removed」，而是「能否被激活」。
    let body = || {
        vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(2)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            MirInst::Const(3, Value::Int(3)),
            MirInst::BinaryOp(4, 2, BinaryOp::Add, 3),
        ]
    };

    // 已优化（生产路径：经 DAG 缓存 + dag_optimize）
    let arc = Arc::new(mora::mir::MirFunction {
        params: vec![],
        body: body(),
        n_regs: 5,
        ..Default::default()
    });
    let mut i1 = mora::interpreter::Interpreter::new();
    let mut e1 = i1.take_env();
    let opt = mora::mir::vm::execute(
        &arc,
        &mut i1,
        &mut e1,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("execute");

    // 未优化
    let arc2 = Arc::new(mora::mir::MirFunction {
        params: vec![],
        body: body(),
        n_regs: 5,
        ..Default::default()
    });
    let dag = mora::mir::dag::dag_analyze(&arc2);
    let mut i2 = mora::interpreter::Interpreter::new();
    let mut e2 = i2.take_env();
    let raw = mora::mir::vm::run_dag(
        &dag,
        &arc2,
        &mut i2,
        &mut e2,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("run_dag");

    assert_eq!(opt, raw, "优化改变了手写 MIR 的语义");
    assert_eq!(format!("{:?}", opt), "Int(6)", "直线链 1+2 → +3 = 6");
}

/// 强制线性路径返回值：`dag_analyze` + `add_sequential_edges` + 同一 BSP 执行器。
///
/// 走 `mora::mir::vm::run_dag`（`vm.rs` 用 `pub use dag::*;` 把执行器整体
/// re-export，`vm::engine` 是公开模块）。
///
/// **v0.104.6 D466**：原实现在此处额外调 `dag.add_sequential_edges()`
/// （「强制线性化」）后再跑。鉴于该方法已被删除（它按相邻节点编号补
/// `Sequence` 边，跨基本块时编号≠执行序，会架空 `break`/`continue`），
/// 此处改为**生产建图**：`dag_analyze` → `dag_optimize`（与
/// `DagCache::build` 同链），再交给同一个 BSP 执行器。
///
/// 这样本组断言的是**真实**的 `execute` ≡ `run_dag`，而非「线性化 + 忽略
/// 跳转」的混合体。
fn via_dag(src: &str) -> String {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("compile");
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "type errors: {:?}", errs);
    let arc = Arc::new(func);
    let dag = mora::mir::dag::dag_analyze(&arc);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let v = mora::mir::vm::run_dag(
        &dag,
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("run_dag");
    format!("{:?}", v)
}

/// 逐例断言两条路线等价。
fn assert_equiv(label: &str, src: &str) {
    let a = via_execute(src);
    let b = via_dag(src);
    assert_eq!(a, b, "{}: execute 与 run_dag 不等价", label);
}

// ── 6 条真等价用例 ────────────────────────────────────────────────────────

#[test]
fn execute_equiv_dag_arithmetic() {
    assert_equiv("arithmetic", "let a = 1\nlet b = 2\nlet c = a + b * 3\nc\n");
}

#[test]
fn execute_equiv_dag_for_loop() {
    assert_equiv(
        "for loop",
        "let t = 0\nfor i in [1, 2, 3, 4]\n  assign t = t + i\nend\nt\n",
    );
}

#[test]
fn execute_equiv_dag_while_loop() {
    assert_equiv(
        "while loop",
        "let t = 0\nlet k = 0i\nwhile k < 5i\n  assign t = t + k\n  assign k = k + 1i\nend\nt\n",
    );
}

#[test]
fn execute_equiv_dag_nested_loops() {
    assert_equiv(
        "nested loops",
        "let t = 0\nfor a in [1, 2, 3]\n  for b in [1, 2]\n    assign t = t + a * b\n  end\nend\nt\n",
    );
}

#[test]
fn execute_equiv_dag_closure() {
    assert_equiv("closure", "let f = fn(a, b) a * b + 1 end\nf(3, 4)\n");
}

#[test]
fn execute_equiv_dag_return() {
    assert_equiv(
        "explicit return",
        // v0.104.6 D42：原先这里是**顶层** `return`（`let x = 5 / if x > 1
        // then return x * 2 end / return 0`），而 D42 判定「程序顶层的
        // return」为缺陷（静默终止整个程序、吞掉后续语句且退出码 0），已改为
        // 编译期拒绝。本用例要检验的是「**return 路径**在两条路线上等价」，
        // 与 return 是否在顶层无关 —— 故把两条 return 一起放进 task，
        // 「if 内提前 return」+「末尾兜底 return」的结构原样保留。
        "task f()\n  let x = 5\n  if x > 1 then\n    return x * 2\n  end\n  return 0\nend\nf()\n",
    );
}

// ── 控制流等价用例（v0.104.6 D466：由 `#[ignore]` 分歧记录**转为**真实护栏）──
//
// 这三个程序正是当初暴露 `add_sequential_edges` 造伪图的那三个。删除该方法、
// 改用生产建图后，它们测的已是**真实**的 `execute` ≡ `run_dag` —— 从此
// 作为控制流（分支汇合 / break / continue）的等价护栏常驻。

#[test]
fn execute_equiv_dag_if_else_value() {
    assert_equiv(
        "if/else as value",
        "let c = 2\nlet x = if c == 1 then 5 else 7 end\nx + 1\n",
    );
}

#[test]
fn execute_equiv_dag_while_continue() {
    assert_equiv(
        "while + continue",
        "let n = 0\nlet k = 0i\nwhile k < 6i\n  if k == 2i then\n    assign k = k + 1i\n    continue\n  end\n  assign n = n + 1\n  assign k = k + 1i\nend\nn\n",
    );
}

#[test]
fn execute_equiv_dag_for_break() {
    assert_equiv(
        "for + break",
        "let n = 0\nfor x in [1, 2, 3, 4, 5]\n  if x == 3 then\n    break\n  end\n  assign n = n + 1\nend\nn\n",
    );
}
