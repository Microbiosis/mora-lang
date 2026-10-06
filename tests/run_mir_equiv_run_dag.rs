//! v0.59 核心承诺的**真实检验**：`run_mir ≡ run_dag`（线性退化等价）。
//!
//! ## 这条承诺之前是空的
//!
//! `src/mir/vm.rs:7` 与 `run_mir` 的文档都写着：
//!
//! ```text
//! run_mir ≡ run_dag（dag.add_sequential_edges 后退化线性）
//! ```
//!
//! 但 `tests/e2e.rs` 里引用它的 `e2e_run_mir_deterministic` **只断言「同一个
//! fixture 跑两次都成功」** —— 从未构造 `add_sequential_edges`、从未对比两条
//! 路线。而 `MirDag::add_sequential_edges` 在 `src/` 里**零调用**。
//!
//! ## 这个文件做什么
//!
//! 同一份源码分别走：
//!   * **生产路径** `run_mir` —— DAG 缓存 + `dag_optimize` + 真实 BSP 超步；
//!   * **强制线性路径** —— `dag_analyze` 后 `add_sequential_edges()` 把整图
//!     压成线性顺序，再交给同一个 BSP 执行器。
//!
//! 两者返回值必须相等。
//!
//! ## 检验结果（v0.104.6，continue 一行于 v0.104.6 复核后更正）
//!
//! **承诺在控制流程序上是假的**：9 例中 6 例等价、**3 例发散**。三例里
//! **生产路径全部是对的**，错的都是强制线性路径：
//!
//! | 用例           | run_mir（生产） | 强制线性 | 正确值 | 对的一方 |
//! |----------------|-----------------|----------|--------|----------|
//! | arithmetic / for / while / nested / closure / return | ✅ | ✅ | — | 都对 |
//! | if/else        | 8.0             | `Bool(false)` | 8.0  | run_mir |
//! | while + continue | 5.0           | 6.0      | 5.0  | run_mir |
//! | for + break    | 2.0             | 5.0      | 2.0  | run_mir |
//!
//! ### 更正记录（v0.104.6）
//!
//! 本表原先把 `while + continue` 一行写成「强制线性 6.0 才是对的、生产路径
//! 5.0 是错的」，并把它归因为执行器缺陷 E1。**两处都错**，已按实测更正：
//!
//! 1. **正确的一侧搞反了**。用「continue 版本 vs 去掉 continue 版本 vs
//!    continue 的 if/else 等价写法」三路对照实测（见
//!    `tests/continue_semantics.rs::production_path_honours_continue`）：
//!    带 `continue` = 5.0、其 `if/else` 等价写法 = 5.0、去掉 `continue` = 6.0。
//!    三者一致说明 **5.0 才是对的**，6.0 是强制线性路径的错值。
//! 2. **归因错了**。E1（汇合点被未选中分支臂饿死）已于 v0.104.6 修复，
//!    而本用例的发散在修复后**依然存在**（5.0 vs 6.0 不变），故与 E1 无关。
//!
//! ### 真正的机制
//!
//! 两条分��的错法一致，都是**强制线性路径忽略了 `break` / `continue`**：
//! `add_sequential_edges` 把整图连成线性链，而 `break` / `continue` 是跳出
//! 线性链的控制转移，在这个模型里失效 —— 循环体的剩余部分照跑不误。
//! 故 `for + break` 的 5.0（而非 2.0）、`while + continue` 的 6.0（而非 5.0）
//! 恰好都是「跳转被忽略、循环体多跑了一轮」。
//!
//! **结论不变，理由变了**：「用 `add_sequential_edges` 把线性归入非线性」
//! 这条路依然不可行 —— 但不是因为生产路径有缺陷，而是因为这个**实验装置
//! 本身**不保真（它测的是「线性化 + 忽略跳转」的混合体，不能用来判定生产
//! 路径的对错）。下方 3 条以 `#[ignore]` 记录实测分歧，不是假装通过；用
//! `cargo test --test run_mir_equiv_run_dag -- --ignored` 可复现。

use std::sync::Arc;

use mora::common::BinaryOp;
use mora::mir::MirInst;
use mora::value::Value;

/// 生产路径返回值。
fn via_run_mir(src: &str) -> String {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("compile");
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "type errors: {:?}", errs);
    let arc = Arc::new(func);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let v = mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("run_mir");
    format!("{:?}", v)
}

/// **未优化**路径返回值：`run_mir_dag` 直调 `dag_analyze`，**绕过 `dag_optimize`
/// 与 DAG 缓存**（对比生产链 `dag_analyze → dag_optimize → prune_sequence_edges`）。
///
/// 它只在测试里被调用（`tests/dag_integration.rs`），因此**优化阶段对它完全
/// 不可见**：CSE 跨区域合并、追加节点可达性、死块 entry 过滤这三类缺陷都
/// 只能由它走 `dag_optimize` 才可能暴露。
///
/// 本组用例把两条路线**按值**对比 —— 优化是保语义的重写，两者必须相等。
/// 这是当前缺失的那道护栏。
fn via_run_mir_dag_unoptimized(src: &str) -> String {
    let (func, _witnesses) = mora::parser_v3::ParserV3::compile(src).expect("compile");
    let arc = Arc::new(func);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let v = mora::mir::vm::run_mir_dag(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("run_mir_dag");
    format!("{:?}", v)
}

/// 优化保语义：生产路径（已优化）必须与未优化路径结果一致。
fn assert_optimization_preserves_semantics(label: &str, src: &str) {
    let opt = via_run_mir(src);
    let raw = via_run_mir_dag_unoptimized(src);
    assert_eq!(
        opt, raw,
        "{}: dag_optimize 改变了语义（已优化={} 未优化={}）",
        label, opt, raw
    );
}

// ── 优化保语义：生产（已优化） ≡ run_mir_dag（未优化）────────────────────
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
    let opt = mora::mir::vm::run_mir(
        &arc,
        &mut i1,
        &mut e1,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("run_mir");

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
/// re-export，`vm::dag` 本身是私有模块）。
fn via_dag_forced_linear(src: &str) -> String {
    let (func, _witnesses) = mora::parser_v3::ParserV3::compile(src).expect("compile");
    let arc = Arc::new(func);
    let mut dag = mora::mir::dag::dag_analyze(&arc);
    dag.add_sequential_edges();
    dag.recompute_entry();
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
    let a = via_run_mir(src);
    let b = via_dag_forced_linear(src);
    assert_eq!(a, b, "{}: run_mir 与强制线性 run_dag 不等价", label);
}

// ── 6 条真等价用例 ────────────────────────────────────────────────────────

#[test]
fn run_mir_equiv_forced_linear_dag_arithmetic() {
    assert_equiv("arithmetic", "let a = 1\nlet b = 2\nlet c = a + b * 3\nc\n");
}

#[test]
fn run_mir_equiv_forced_linear_dag_for_loop() {
    assert_equiv(
        "for loop",
        "let t = 0\nfor i in [1, 2, 3, 4]\n  assign t = t + i\nend\nt\n",
    );
}

#[test]
fn run_mir_equiv_forced_linear_dag_while_loop() {
    assert_equiv(
        "while loop",
        "let t = 0\nlet k = 0i\nwhile k < 5i\n  assign t = t + k\n  assign k = k + 1i\nend\nt\n",
    );
}

#[test]
fn run_mir_equiv_forced_linear_dag_nested_loops() {
    assert_equiv(
        "nested loops",
        "let t = 0\nfor a in [1, 2, 3]\n  for b in [1, 2]\n    assign t = t + a * b\n  end\nend\nt\n",
    );
}

#[test]
fn run_mir_equiv_forced_linear_dag_closure() {
    assert_equiv("closure", "let f = fn(a, b) a * b + 1 end\nf(3, 4)\n");
}

#[test]
fn run_mir_equiv_forced_linear_dag_return() {
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

// ── 3 条已知分歧（v0.104.6 实测，未修）──────────────────────────────────

#[test]
#[ignore = "已知分歧：强制线性得 Bool(false)，生产路径 8.0 才对"]
fn run_mir_equiv_forced_linear_dag_if_else_diverges() {
    assert_equiv(
        "if/else",
        "let c = 2\nlet x = if c == 1 then 5 else 7 end\nx + 1\n",
    );
}

#[test]
#[ignore = "已知分歧：强制线性得 6.0（continue 被忽略 → 循环体多跑一轮），生产路径 5.0 才对。v0.104.6 复核：本行原先把正确的一侧写反、并误归因为已修复的 E1，均已更正，见文件头"]
fn run_mir_equiv_forced_linear_dag_continue_diverges() {
    assert_equiv(
        "while + continue",
        "let n = 0\nlet k = 0i\nwhile k < 6i\n  if k == 2i then\n    assign k = k + 1i\n    continue\n  end\n  assign n = n + 1\n  assign k = k + 1i\nend\nn\n",
    );
}

#[test]
#[ignore = "已知分歧：强制线性得 5.0（break 被忽略），生产路径 2.0 才对"]
fn run_mir_equiv_forced_linear_dag_break_diverges() {
    assert_equiv(
        "for + break",
        "let n = 0\nfor x in [1, 2, 3, 4, 5]\n  if x == 3 then\n    break\n  end\n  assign n = n + 1\nend\nn\n",
    );
}
