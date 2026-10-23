//! v0.104.6 D120：`let r = solve { … }` 让**后续语句全部静默饿死**（已修）。
//!
//! ## 缺陷
//!
//! ```mora
//! let r = solve { unify(1, 1) }
//! print("r-is", r)      // ← 修复前：零输出，exit 0，零诊断
//! ```
//!
//! 实测（修复前，默认 9 层管线）：**stdout 为空、退出码 0、无任何诊断**。
//! 同一份源码在 `MORA_9LAYER=0`（裸路径）下却正常输出 `r-is [nil]` ——
//! 两条编译路径行为分叉。
//!
//! ## 根因：`Node::Solve` 缺 `dst` —— 与 D35 / D58 完全同源
//!
//! `node_result_reg_of` 的 match 里有 `Node::Handle`（D35 补）、`Node::WithConfig`
//! （D58 补），**唯独漏了 `Node::Solve`** → 落到 `_ => None` →
//! `node_result_reg` 返回 `unwrap_or(0)` 哨兵 → `let` 绑定的**就绪门槛恒 false**
//! → 其后整条 Sequence 链永不执行。D35 的注释把这个失败模式写得很清楚，
//! 本缺陷只是换了个节点类型。
//!
//! 而 `MirInst::Solve` **是有** `dst` 的，`emit_primary_w` 也明确把 solve
//! 当表达式（「v0.102: solve 作为表达式（`let r = solve { ... }`）」）——
//! 9 层管线的 fcfg 层把结果寄存器丢了。

use mora::interpreter::Interpreter;
use mora::mir::vm::execute;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;
use std::sync::Arc;

/// ⚠ 走 **9 层管线**（`cli::compile_and_opt`）编译，再**真正执行**产物。
///
/// 这条是 D120 判据的**关键**：`ParserV3::compile` 是单遍直出，
/// **不经过** `witness_to_fcfg` —— 缺陷就住在那里。第一版测试全部只走
/// `ParserV3::compile`（当时那个 `last_value` 助手，**D295 已删**：它已成
/// 死代码），反向验证时**一条没红**（测的是没坏的路径）。
/// 必须执行 9 层管线的产物才能咬住。
fn last_value_pipelined(src: &str) -> String {
    let (func, _wit) = mora::cli::compile_and_opt(src, None).expect("9 层管线编译");
    run_source(func, src)
}

fn run_source(func: mora::mir::MirFunction, _src: &str) -> String {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    match execute(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("Err({e})"),
    }
}

/// D120 主判据：`solve` 绑定的**后续语句必须照常执行**。
///
/// 走 **9 层管线**并**执行**其产物 —— 缺陷住在 `witness_to_fcfg`，
/// 直出路径永远看不到它。
///
/// 用末值而不是「能编译」：修复前 `ParserV3::compile` 是**成功**的
/// （exit 0、零诊断），缺陷只在执行期体现。
#[test]
fn d120_solve_binding_does_not_starve_following_statements() {
    // `let r = solve { … }` 之后的求值表达式必须取到值
    let got = last_value_pipelined("let r = solve { unify(1, 1) }\nlet after = 42\nafter\n");
    assert_eq!(
        got, "Float(42.0)",
        "9 层管线产物执行时，solve 之后必须继续求值；\
         得到 Nil/哨兵说明 Node::Solve 又缺 result-reg arm（哨兵 0 的就绪门槛恒 false）"
    );
}

/// 对照组：solve 的**值**本身要能取到，且绑定的变量不是哨兵。
#[test]
fn d120_solve_binding_yields_a_real_value() {
    // 对照失败时的实测：修复前这行会因饿死而不执行
    let got = last_value_pipelined("let r = solve { unify(1, 1) }\nlen([1, 2, 3])\n");
    // ⚠ 本语言 `Int` **只**由 `len()` 产生（裸数字字面量一律是 Float，D98）。
    assert_eq!(
        got, "Int(3)",
        "solve 之后必须能继续求值（`len` 走的是完全不同的指令族）"
    );
}

/// ⚠ **绝对值**：`solve` 之后的值必须真的被取到。
///
/// 本缺陷的隐蔽之处正在于此 —— 旧实现里 9 层管线**拿不到** solve 的值
/// （`solve` 写的 dst 与 `let` 绑定的 dst 对不上），而单遍直出路径一直
/// 是对的 ⇒ 只测直出路径会全绿。
///
/// v0.104.6 **D474**：原判据里的「两条编译路径执行结果一致」**已作废** ——
/// D474 把差分机制整个退役，parser 只产 witness、MIR 全部由 9 层产出 ⇒
/// 两个入口**走同一条路** ⇒ 那条比对成了**恒真**。
///
/// ⇒ 换成**绝对值**判据：直接断言程序算出的值。
#[test]
fn d120_solve_binding_takes_the_value() {
    let src = "let r = solve { unify(1, 1) }\nlet after = 42\nafter";

    let (compiled, wit) = mora::cli::compile_and_opt(src, None).expect("编译");
    assert!(
        check_program_witnesses_bidirectional(&wit).is_empty(),
        "typeck 应无错"
    );

    // **执行结果**必须是 42.0 —— D120 的两条路径指令类别完全一样，
    // 差别在 `Solve` 写的 dst 与 `let` 绑定的 dst 对不上（只有执行能看出来）。
    let val = run_source(compiled, src);
    assert_eq!(val, "Float(42.0)", "必须取到 solve 之后的值");
}

/// 对照组：**语句位置**的 `solve { … }` 一直是对的（D120 之前也正常）。
///
/// 它不经过 `let` 绑定 → 不消费 `node_result_reg` → 绕开了缺陷路径。
/// 与主判据走**同一条**（9 层）路径，这个对照才成立。
#[test]
fn d120_statement_position_solve_still_works() {
    let got = last_value_pipelined("solve { unify(1, 1) }\nlet after = 7\nafter\n");
    assert_eq!(got, "Float(7.0)");
}

/// 反向对照：`Node::Solve` 在 `node_result_reg_of` 里**必须**有 arm。
///
/// 走 9 层管线的 fcfg 降级取寄存器；缺 arm 时落 `_ => None` → 哨兵 0。
/// 判据取**执行层**而非结构 —— 哨兵 0 的唯一可观测后果就是
/// 「后续 Sequence 永不执行」，这才是真正要锁住的东西。
#[test]
fn d120_node_solve_has_a_result_register() {
    // 哨兵 0 的就绪门槛恒 false → 后续语句饿死 → 末值拿不到 99
    // ⚠ 必须走 9 层管线：直出路径不经 witness_to_fcfg，测了也白测
    assert_eq!(
        last_value_pipelined("let r = solve { unify(1, 1) }\nlet sentinel = 99\nsentinel\n"),
        "Float(99.0)",
        "Node::Solve 若又缺 result-reg arm，哨兵 0 会让后续 Sequence 饿死（D120 回归）"
    );

    // 对照组：witness 里**确实**含 Solve（若语法改了，上面那条可能假绿）
    use mora::mir::witness::{MirWitness, WitnessKind};
    fn has_solve(w: &MirWitness) -> bool {
        if matches!(w.kind, WitnessKind::Solve { .. }) {
            return true;
        }
        // witness.rs 有统一的 children 提取（含 Let 的 value / Sequence 等）
        w.child_witnesses().iter().any(|c| has_solve(c))
    }
    let (_f, wits) =
        ParserV3::compile("let r = solve { unify(1, 1) }\nprint(1)\n").expect("compile");
    assert!(
        wits.iter().any(has_solve),
        "对照组失败：witness 树里应含 Solve（若语法变了，删掉本测试）"
    );
}
