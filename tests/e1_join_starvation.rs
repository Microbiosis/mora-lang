//! v0.104.6 E1 回归护栏：**控制流汇合点之后的语句不得消失**。
//!
//! ## E1 是什么
//!
//! `if/else` 之后跟任何语句时，那条语句**整条不执行** —— 无报错、退出码 0。
//! 根因不在执行器，在 `dag_analyze` 的分块（详见 `src/mir/dag.rs`
//! `partition_blocks` 的注释）：裸 pc 跳转目标没被当块首，汇合点被并进
//! else 臂那一块，块内 Sequence 链与 `last_effect` 扇出因此越过汇合点。
//!
//! ## 为什么这个文件断言的是**返回值**而不是「不报错」
//!
//! E1 的症状是「语句静默消失」。若尾部写成 `print(8)`，它的返回值恒为
//! `Nil` —— 此时哪怕整条尾部从未执行，断言 `is_ok()` 也照样通过。
//! **那正是 E1 长期隐身的原因**：`tier0_replacement::
//! semantics_control_flow_runs_via_mir` 的 `print("sum=" + str(total))` 挂在
//! 汇合点之后，而该测试只检查 `run_via_mir(src).is_ok()`，于是「从未执行」
//! 被判成「通过」，还顺手掩盖了 `str()` 缺运行期实现这个独立缺陷。
//!
//! 所以本文件**所有用例的尾部都是「有值的表达式」而非 `print`**，并逐条
//! 断言返回值。这样「尾部没跑」就会表现为返回值不对，而不是悄悄通过。
//!
//! ## 形态覆盖
//!
//! 6 种形态 × **正反两条路径**。原始 E1 只在 then 侧饿死、else 侧本就好
//! —— 两条路径**不对称**，只测一条会得到「没问题」的错误结论。

use mora::interpreter::Interpreter;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

/// 跑一段源码，返回顶层 body 的值（格式化后断言）。
fn run(source: &str) -> String {
    let (func, _w) = ParserV3::compile(source).unwrap_or_else(|e| panic!("compile: {e}"));
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

#[track_caller]
fn assert_val(name: &str, src: &str, expected: &str) {
    let got = run(src);
    assert_eq!(got, expected, "[{name}] src={src:?}");
}

// ── 形态 1：`if` 作值 + 尾部表达式（E1 的原始复现）──────────────────────

/// 修复前：then 路径下 `x + 1` **从未执行**，返回值停在 `x` = 5.0。
#[test]
fn e1_if_as_value_tail_on_then_path() {
    assert_val(
        "A1",
        "let c = 1\nlet x = if c == 1 then 5 else 7 end\nx + 1\n",
        "Float(6.0)",
    );
}

/// else 路径修复前**本来就正常** —— 保留它是为了钉住「两条路径结果一致」。
#[test]
fn e1_if_as_value_tail_on_else_path() {
    assert_val(
        "A2",
        "let c = 2\nlet x = if c == 1 then 5 else 7 end\nx + 1\n",
        "Float(8.0)",
    );
}

// ── 形态 2：块式 if/else 赋值 + 尾部表达式 ──────────────────────────────

#[test]
fn e1_block_if_else_tail_on_then_path() {
    assert_val(
        "A3",
        "let c = 1\nlet x = 0\nif c == 1 then\n  x = 5\nelse\n  x = 7\nend\nx + 1\n",
        "Float(6.0)",
    );
}

#[test]
fn e1_block_if_else_tail_on_else_path() {
    assert_val(
        "A4",
        "let c = 2\nlet x = 0\nif c == 1 then\n  x = 5\nelse\n  x = 7\nend\nx + 1\n",
        "Float(8.0)",
    );
}

// ── 形态 3：裸 `if`（无 else，隐式 else 由 fcfg_lower 物化）─────────────

/// 无 else 的 if 走 then：汇合点只能经 `Jump` 到达。
#[test]
fn e1_bare_if_tail_on_then_path() {
    assert_val(
        "A5",
        "let c = 1\nlet n = 0\nif c == 1 then\n  let n = 7\nend\nn + 1\n",
        "Float(8.0)",
    );
}

/// 无 else 的 if 走 else（隐式空块）：汇合点只能经 fall-through 到达。
///
/// 这一条同时守住 `dag_analyze` 新补的「块边界 fall-through `Control` 边」——
/// 隐式 else 块以非终结符收尾，这条边缺失则汇合点在**任何**路径下都不会被
/// 激活（修复前的另一半）。
#[test]
fn e1_bare_if_tail_on_else_path() {
    assert_val(
        "A6",
        "let c = 2\nlet n = 0\nif c == 1 then\n  let n = 7\nend\nn + 1\n",
        "Float(1.0)",
    );
}

// ── 形态 4：嵌套分支的汇合点 ───────────────────────────────────────────

/// 外层 if 的尾部同时跨过内层 if/else 的汇合点。
#[test]
fn e1_nested_if_join_points() {
    assert_val(
        "A7",
        "let a = 1\nlet b = 2\nlet n = 0\nif a == 1 then\n  if b == 2 then\n    let n = 5\n  else\n    let n = 6\n  end\nend\nn + 1\n",
        "Float(6.0)",
    );
}

/// `else if` 链：多个汇合点串联，最容易触发「未选中分支臂挡住尾部」。
#[test]
fn e1_if_else_if_chain() {
    assert_val(
        "A8",
        "let x = 5\nlet n = 0\nif x > 9 then\n  let n = 1\nelse if x > 4 then\n  let n = 2\nelse\n  let n = 3\nend\nn * 10\n",
        "Float(20.0)",
    );
}

// ── 形态 5：循环汇合点（分块修正唯一动过循环的地方）─────────────────────

/// `Jump(header)` 让 header 成为块首，`header-1 → header` 由 `Sequence`
/// 改为 `Control` —— 这条用例守住它没退化。
#[test]
fn e1_for_loop_join_after_backedge() {
    assert_val(
        "B2",
        "let s = 0\nfor v in [10, 20, 30]\n  let s = s + 1\nend\ns\n",
        "Float(3.0)",
    );
}

/// while + continue：`continue` 是第二类跳进块内的控制转移。
#[test]
fn e1_while_continue_then_tail() {
    assert_val(
        "B8",
        "let i = 0\nlet s = 0\nwhile i < 8\n  let i = i + 1\n  if i == 3 then\n    continue\n  end\n  if i == 7 then\n    break\n  end\n  let s = s + i\nend\ns\n",
        "Float(18.0)",
    );
}

/// 循环 + 分支 + 汇合点三者叠加。
#[test]
fn e1_loop_if_assign_composite() {
    assert_val(
        "E1",
        "let t = 0\nfor i in [1, 2, 3, 4, 5]\n  if i % 2 == 0 then\n    let t = t + i\n  else\n    let t = t + 1\n  end\nend\nt\n",
        "Float(9.0)",
    );
}

// ── 形态 6：`str()` 的运行期实现 ────────────────────────────────────────

/// `str()` 在 typeck 有签名（`src/typeck/hm/builtin.rs`）却曾无运行期实现。
/// 它之所以长期隐身，正是因为唯一用到它的那行被 E1 饿死了。
/// 本用例无任何控制流，可独立复现该缺口。
///
/// 注：Mora 的字面量 `45` 是 `Float`，故 `str(45)` 是 `"45.0"` 而非 `"45"`。
#[test]
fn str_builtin_exists_at_runtime() {
    assert_val("D0", "str(45)\n", "String(\"45.0\")");
}

#[test]
fn str_builtin_concatenates_with_string() {
    assert_val("D1", "let n = 7\n\"n=\" + str(n)\n", "String(\"n=7.0\")");
}
