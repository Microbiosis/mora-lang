//! 「已优化」vs「未优化」两条执行路径的差分护栏 —— 优化必须保语义。
//!
//! ## 为什么扩大覆盖
//!
//! `tests/run_mir_equiv_run_dag.rs` 此前只有 **3 例**走这个对拍（if/else 作值、
//! 嵌套 break、while+continue）。而优化器有三条规则（`CseDagRule` /
//! `DeadNodeDagRule` / `ConstFoldingDagRule`），3 例远不足以证明「保语义」。
//!
//! 尤其 `CseDagRule` 有一条**仓库自己记录过的危害**（`tests/fixtures/e2e/
//! for_loop.mora` 的注释）：
//!
//! > 把「循环索引 init」与另一条语句的同值常量合并，再把读该索引的位点
//! > 全局重命名，但增量仍写原寄存器 → 循环条件恒假 → 死循环
//! > （`let total = 0i` 与索引 init 同为 Int(0)，**同值才碰撞**）
//!
//! 关键在「**同值才碰撞**」——那 3 例恰好都没造出同值常量，所以从未覆盖到
//! 触发条件。本文件的第一组用例专门构造它。
//!
//! ## 判据
//!
//! 两条路径跑**同一份源码**，返回值必须逐字相同：
//!   * 生产路径 `run_mir` —— DAG 缓存 + `dag_optimize` + 真实 BSP 超步；
//!   * 未优化路径 `run_mir_dag` —— 直调 `dag_analyze`，绕过优化。
//!
//! 2026-09-28 实测：16 个形态**全部一致**（`differing = 0`），含已知危害的
//! 触发形态。故此文件是**回归网**而非缺陷报告。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::{run_mir, run_mir_dag};
use mora::parser_v3::ParserV3;

fn via_optimized(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

fn via_unoptimized(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir_dag(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

/// `(分组::用例名, 源码)`。
fn cases() -> Vec<(&'static str, &'static str)> {
    vec![
        // ── CseDagRule：同值常量碰撞（仓库记录过的已知危害的触发形态）────
        (
            "cse::zero_collision_loop",
            "let total = 0i\nlet i = 0i\nwhile i < 5i\n  assign total = total + i\n  assign i = i + 1i\nend\ntotal\n",
        ),
        (
            "cse::zero_collision_nested",
            "let a = 0i\nlet b = 0i\nlet s = 0i\nfor x in [1, 2, 3]\n  assign s = s + a + b + x\nend\ns\n",
        ),
        (
            "cse::identical_consts_many",
            "let a = 1i\nlet b = 1i\nlet c = 1i\nlet d = 1i\na + b + c + d\n",
        ),
        (
            "cse::same_float_zero",
            "let t = 0.0\nlet u = 0.0\nlet v = 0.0\nlet acc = 0.0\nfor x in [1.0, 2.0, 3.0]\n  assign acc = acc + x + t + u + v\nend\nacc\n",
        ),
        (
            "cse::collision_with_empty_lists",
            "let e = []\nlet f = []\nlet s = 0\nfor i in [1, 2, 3]\n  assign s = s + len(e) + len(f) + i\nend\ns\n",
        ),
        (
            "cse::collision_nested_break",
            "let c = 1i\nlet r = 0i\nfor a in [1, 2, 3]\n  for b in [1, 2]\n    if b == c then\n      break\n    end\n    assign r = r + c\n  end\nend\nr\n",
        ),
        (
            "cse::identical_strings",
            "let a = \"x\"\nlet b = \"x\"\nlet c = \"x\"\na + b + c\n",
        ),
        // ── ConstFoldingDagRule ─────────────────────────────────────
        ("constfold::arith", "let x = 2 + 3 * 4\nx\n"),
        (
            "constfold::repeated",
            "let a = 1 + 1\nlet b = 1 + 1\nlet c = 1 + 1\na + b + c\n",
        ),
        (
            "constfold::in_loop_cond",
            "let n = 0\nwhile n < 2 + 3\n  assign n = n + 1\nend\nn\n",
        ),
        // ── DeadNodeDagRule ─────────────────────────────────────────
        (
            "dead::assign_before_use",
            "let x = 1\nlet y = 2\nlet z = 3\nz\n",
        ),
        (
            "dead::branch_arm",
            "let c = 1\nlet x = if c == 1 then 5 else 7 end\nx\n",
        ),
        // ── 容器（迁移到持久化 List 后尤其要盯）─────────────────────────
        (
            "container::list_loop",
            "let xs = [1, 2, 3, 4]\nlet s = 0\nfor x in xs\n  assign s = s + x\nend\ns\n",
        ),
        (
            "container::dict_loop",
            "let d = {a: 1, b: 2}\nlet s = 0\nfor k in d.keys()\n  assign s = s + d.get(k)\nend\ns\n",
        ),
        (
            "container::push_loop",
            "let xs = []\nlet i = 0\nwhile i < 5\n  assign xs = xs.push(i)\n  assign i = i + 1\nend\nlen(xs)\n",
        ),
        // ── 混合：同值常量 + 循环 + 分支 ──────────────────────────────
        (
            "mixed::all",
            "let t = 0i\nlet acc = 0\nfor x in [1, 2, 3, 4]\n  if x % 2 == 0 then\n    assign acc = acc + x\n  else\n    assign t = t + 1i\n  end\nend\nacc * 100 + t\n",
        ),
    ]
}

#[test]
fn optimization_preserves_semantics_across_shapes() {
    let all = cases();
    let mut failures = Vec::new();
    for (name, src) in &all {
        let opt = via_optimized(src);
        let raw = via_unoptimized(src);
        if opt != raw {
            failures.push(format!(
                "  [{name}]\n    src      = {src:?}\n    optimized   = {opt}\n    unoptimized = {raw}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 个形态上「已优化」与「未优化」结果发散 —— 优化破坏了语义：\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}
