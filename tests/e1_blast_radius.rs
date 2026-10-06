//! E1 修复的**影响面**护栏 —— 26 种控制流形态的结果指纹。
//!
//! ## 为什么需要这个文件
//!
//! E1 的修复改的是 `partition_blocks` 的**分块**，而分块决定：循环回边与
//! 汇合点的块归属 → `dag_analyze` 的块内 Sequence 链与 `last_effect` 扇出
//! → `dag_optimize::CseDagRule` 的「同基本块」判据。也就是说，它影响**每一个
//! 带跳转的程序**，远不止触发 bug 的 if/else。
//!
//! 「全量套件全绿」在这里**不够**：套件对 `if/else` 之外的形态覆盖稀疏，
//! 而一次分块改动悄悄改变 CSE 的同块判定是完全可能的（且不会报错）。
//!
//! ## 这批期望值是怎么来的
//!
//! 2026-09-28 对 E1 的两个 hunk（裸 pc 跳转目标入 `starts`、块边界
//! fall-through 改发 `Control`）做了开关式 A/B，同一批 26 个用例各跑一遍：
//!
//! ```text
//! 26 个用例，仅 2 个不同：
//!   A1.if_value_then   before: Float(5.0)  →  after: Float(6.0)
//!   A3.block_if_then   before: Float(5.0)  →  after: Float(6.0)
//! 其余 24 个（循环 / break / continue / return / 闭包 / 递归 / 列表 /
//! 循环+分支复合）逐字节相同。
//! ```
//!
//! 即：差异**恰好**落在修复目标上，且**零附带损伤**。本文件把这 26 个结果
//! 钉成基线，任何后续分块改动一旦溢出到 if/else 之外就会立刻暴露。

use mora::interpreter::Interpreter;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
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

/// `(组::用例名, 源码, 期望值)`。
///
/// 期望值 = 2026-09-28 A/B 的「after」侧（修复生效后的正确结果）。
/// 尾部一律用**有值的表达式**而非 `print` —— `print` 的返回值恒为 `Nil`，
/// 那样「尾部没执行」就观测不到（见 `e1_join_starvation.rs` 的说明）。
fn cases() -> Vec<(&'static str, String, String)> {
    vec![
        // ── A. if/else 汇合点（E1 的目标区；A1/A3 是 A/B 中仅有的两处差异）
        ("A1.if_value_then", "let c = 1\nlet x = if c == 1 then 5 else 7 end\nx + 1\n".into(), "Float(6.0)".into()),
        ("A2.if_value_else", "let c = 2\nlet x = if c == 1 then 5 else 7 end\nx + 1\n".into(), "Float(8.0)".into()),
        ("A3.block_if_then", "let c = 1\nlet x = 0\nif c == 1 then\n  x = 5\nelse\n  x = 7\nend\nx + 1\n".into(), "Float(6.0)".into()),
        ("A4.block_if_else", "let c = 2\nlet x = 0\nif c == 1 then\n  x = 5\nelse\n  x = 7\nend\nx + 1\n".into(), "Float(8.0)".into()),
        ("A5.bare_if_then", "let c = 1\nlet n = 0\nif c == 1 then\n  let n = 7\nend\nn + 1\n".into(), "Float(8.0)".into()),
        ("A6.bare_if_else", "let c = 2\nlet n = 0\nif c == 1 then\n  let n = 7\nend\nn + 1\n".into(), "Float(1.0)".into()),
        ("A7.nested_if", "let a = 1\nlet b = 2\nlet n = 0\nif a == 1 then\n  if b == 2 then\n    let n = 5\n  else\n    let n = 6\n  end\nend\nn + 1\n".into(), "Float(6.0)".into()),
        ("A8.if_chain", "let x = 5\nlet n = 0\nif x > 9 then\n  let n = 1\nelse if x > 4 then\n  let n = 2\nelse\n  let n = 3\nend\nn * 10\n".into(), "Float(20.0)".into()),

        // ── B. 纯循环（E1 未触碰，但分块改动会波及 —— 这是重点监控区）
        ("B1.for_sum", "let t = 0\nfor i in [1, 2, 3, 4]\n  let t = t + i\nend\nt\n".into(), "Float(10.0)".into()),
        ("B2.for_bind", "let s = 0\nfor v in [10, 20, 30]\n  let s = s + 1\nend\ns\n".into(), "Float(3.0)".into()),
        ("B3.while", "let i = 0\nlet s = 0\nwhile i < 5\n  let s = s + i\n  let i = i + 1\nend\ns\n".into(), "Float(10.0)".into()),
        ("B4.for_break", "let t = 0\nfor i in [1, 2, 3, 4, 5]\n  if i == 3 then\n    break\n  end\n  let t = t + i\nend\nt\n".into(), "Float(3.0)".into()),
        ("B5.for_continue", "let t = 0\nfor i in [1, 2, 3, 4, 5]\n  if i == 3 then\n    continue\n  end\n  let t = t + i\nend\nt\n".into(), "Float(12.0)".into()),
        ("B6.nested_break", "let t = 0\nfor i in [1, 2, 3]\n  for j in [1, 2, 3]\n    if j == 2 then\n      break\n    end\n    let t = t + 1\n  end\nend\nt\n".into(), "Float(3.0)".into()),
        ("B7.nested_continue", "let t = 0\nfor i in [1, 2, 3]\n  for j in [1, 2, 3]\n    if j == 2 then\n      continue\n    end\n    let t = t + 1\n  end\nend\nt\n".into(), "Float(6.0)".into()),
        ("B8.while_break_continue", "let i = 0\nlet s = 0\nwhile i < 8\n  let i = i + 1\n  if i == 3 then\n    continue\n  end\n  if i == 7 then\n    break\n  end\n  let s = s + i\nend\ns\n".into(), "Float(18.0)".into()),
        ("B9.for_then_sum", "let t = 0\nfor i in [1, 2, 3]\n  if i > 1 then\n    let t = t + i\n  end\nend\nt\n".into(), "Float(5.0)".into()),

        // ── C. 直线 + return
        //
        // v0.104.6 D42：C2 / C3 原先是**顶层** `return`，而 D42 判定
        // 「程序顶层的 return」为缺陷（静默终止整个程序、吞掉后续语句、
        // 退出码 0），已改为编译期拒绝。本组要检验的是「**控制流里的
        // return** 传值正确」—— 这与 return 是否在顶层无关，故把 return
        // 连同它所在的控制流一起放进 task：E1 的汇合点结构
        // （if/for 体内的提前 return + 之后的兜底值）原样保留。
        ("C1.linear", "let a = 1 + 2\nlet b = a * 3\nb\n".into(), "Float(9.0)".into()),
        ("C2.return_in_if", "task f()\n  let c = 1\n  if c == 1 then\n    return 42\n  end\n  7\nend\nf()\n".into(), "Float(42.0)".into()),
        ("C3.return_in_loop", "task f()\n  for i in [1, 2, 3]\n    if i == 2 then\n      return i * 10\n    end\n  end\n  0\nend\nf()\n".into(), "Float(20.0)".into()),

        // ── D. 闭包 / task / 列表
        ("D1.closure", "let base = 10\nlet f = fn(x) x + base end\nf(5)\n".into(), "Float(15.0)".into()),
        ("D2.task_call", "task double(n)\n  return n * 2\nend\ndouble(5)\n".into(), "Float(10.0)".into()),
        ("D3.list_ops", "let xs = [1, 2, 3]\nlet ys = xs.push(4)\nlen(ys)\n".into(), "Int(4)".into()),
        ("D4.loop_build_list", "let xs = []\nlet i = 0\nwhile i < 4\n  let xs = xs.push(i * 2)\n  let i = i + 1\nend\nlen(xs)\n".into(), "Int(4)".into()),

        // ── E. 循环 + 分支复合
        ("E1.loop_if_assign", "let t = 0\nfor i in [1, 2, 3, 4, 5]\n  if i % 2 == 0 then\n    let t = t + i\n  else\n    let t = t + 1\n  end\nend\nt\n".into(), "Float(9.0)".into()),
        ("E2.if_in_while_break", "let i = 0\nlet s = 0\nwhile i < 10\n  let i = i + 1\n  if i == 4 then\n    break\n  end\n  let s = s + i\nend\ns\n".into(), "Float(6.0)".into()),
    ]
}

/// 一次跑完全部用例再汇总报差异 —— 避免第一个回归掩盖其余。
#[test]
fn control_flow_shapes_match_baseline() {
    let all = cases();
    let mut failures: Vec<String> = Vec::new();
    for (name, src, expected) in &all {
        let got = run(src);
        if &got != expected {
            failures.push(format!(
                "  [{name}]\n    src      = {src:?}\n    expected = {expected}\n    actual   = {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 个控制流形态与基线不符：\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

/// 同一组形态的**交叉不变量**：`if/else` 两条路径走完后，尾部必须都执行。
///
/// 单独断言这条，是为了让「两条路径不对称」这种最难自查的回归有一个
/// 专门的、读起来就能懂的守卫。
#[test]
fn if_else_both_paths_reach_the_tail() {
    for (label, src) in [
        (
            "if-as-value",
            "let c = {C}\nlet x = if c == 1 then 5 else 7 end\nx + 1\n",
        ),
        (
            "block-if",
            "let c = {C}\nlet x = 0\nif c == 1 then\n  x = 5\nelse\n  x = 7\nend\nx + 1\n",
        ),
        (
            "bare-if",
            "let c = {C}\nlet n = 0\nif c == 1 then\n  let n = 7\nend\nn + 1\n",
        ),
    ] {
        let then_run = run(&src.replace("{C}", "1"));
        let else_run = run(&src.replace("{C}", "2"));
        // 尾部一旦没跑，then 侧会停在分支值上（5.0 / 5.0 / 7.0）而非求和结果。
        for (path, got) in [("then", &then_run), ("else", &else_run)] {
            assert!(
                matches!(got.as_str(), "Float(6.0)" | "Float(8.0)" | "Float(1.0)"),
                "[{label}/{path}] 尾部未执行或结果异常: {got}  (src={src:?})"
            );
        }
    }
}
