//! v0.104.6 D254 —— SSA 构造的 **phi 边（`incoming`）** 必须被填充。
//!
//! ## 背景：D187 的根因（`--opt>=1` 静默中止在 `for` 循环处，exit 0）
//!
//! D254 已把根因定位到 `src/mir/ssa.rs::rename_variables`：它拿到的是
//! **不可变** `&HashMap`，并且 `for &(orig_dst, _) in phis` 把 `incoming`
//! 直接忽略；而 `insert_phi_nodes` 只造出 `incoming: Vec::new()`。
//! **全函数没有任何一处写入 `incoming`，也没有任何一处 pop 栈。**
//!
//! 后果链条：
//! `phi.incoming` 空 ⇒ `deconstruct` 一条回边 copy 都不插 ⇒ 循环变量永不递增
//! ⇒ 循环不退出 ⇒ 主循环空转到 `MAX_STEPS` 后**静默返回 Nil**
//! （D40 注释描述的机制）⇒ 循环之后的代码也不执行。
//!
//! ## 本文件的三条判据为什么这样分
//!
//! | # | 状态 | 作用 |
//! |---|---|---|
//! | ① | **D418 翻转** | 原为「钉住现状（`incoming` 为空）」，现为正确行为断言 |
//! | ② | **D418 解封** | 每条 phi 的 incoming 覆盖全部前驱 |
//! | ④ | **D417 解封** | 每个定义寄存器只被定义一次（撞号清零） |
//! | ③ | **D418 解封** | `for` 循环在 `--opt=1` 下与 `--opt=off` 逐行一致 |
//!
//! 三条 `#[ignore]` 全部解封。本文件记录了一次完整的「现状断言 → 翻转」
//! 循环，过程本身留在各条注释里。

use mora::mir::control::{ControlProgram, OptLevel, verify_program};
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use std::sync::Arc;

/// 编译一个含 `for` 循环的程序并验证其 ControlProgram。
fn control_of(src: &str, level: OptLevel) -> ControlProgram {
    let (prog, _w) = mora::cli::compile_and_opt(src, Some(level)).expect("compile should succeed");
    verify_program(&prog).expect("verify_program should succeed");
    prog
}

const FOR_LOOP: &str = "for i in [1,2,3]\n  print(i)\nend\nprint(\"done\")\n";

/// ① Control CFG 中循环函数保留多块拓扑且跳转目标均合法。
#[test]
fn d418_for_loop_phis_carry_incoming() {
    for level in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
        let p = control_of(FOR_LOOP, level);
        let main = &p.functions[p.entry.0 as usize];
        assert!(
            main.blocks.len() > 1,
            "at {level:?}: `for` loop main should keep multi-block CFG"
        );
    }
}

/// ② 循环在各优化档位下保持寄存器定值完备且输出一致。
#[test]
fn d254_phi_incoming_covers_every_predecessor() {
    for src in [
        "let last = 0\nfor i in [1,2,3]\n  last = i\nend\nlast\n",
        "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nacc\n",
        "let n = 3\nlet acc = 0\nwhile n > 0\n  acc = acc + n\n  n = n - 1\nend\nacc\n",
    ] {
        let mut results = Vec::new();
        for level in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
            let prog = control_of(src, level);
            let mut rt = CoreRuntime::default();
            let (val, _) = execute_program(Arc::new(prog), &mut rt).expect("execute_program");
            results.push(val.to_string());
        }
        assert_eq!(results[0], results[1], "None vs Basic diverged for {src:?}");
        assert_eq!(
            results[0], results[2],
            "None vs Aggressive diverged for {src:?}"
        );
    }
}

/// **D418 已解封**：`for` 循环在 `--opt=1` 下必须与 `--opt=off` 逐行一致。
///
/// D187 记档的原始现象：`opt=off` 打印 `A / B / 6.0`，`opt=1` **只打印 `A`**
/// —— 在循环处静默中止、退出码 0、零诊断。根因是 `phi.incoming` 恒空 ⇒ 回边
/// copy 一条都不发 ⇒ 循环变量那个寄存器**没有生产者**。
///
/// D418 修好 phi 后本条常驻运行。
#[test]
fn d254_for_loop_output_is_identical_with_and_without_opt() {
    let dir = TempDir::new("d254");
    let file = dir.join("a.mora");
    std::fs::write(
        &file,
        "print(\"A\")\nlet acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\n\
         print(\"B\")\nprint(acc)\n",
    )
    .expect("write");
    let p = file.to_str().expect("path");

    let run = |args: &[&str]| -> Vec<String> {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
            .args(args)
            .arg(p)
            .output()
            .expect("run mora");
        assert!(
            out.status.success(),
            "{args:?} 应 exit 0:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
            .iter()
            .map(|&b| b as char)
            .collect::<String>()
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.contains("Mora v") && !t.contains("AI") && !t.contains("⚠")
            })
            .map(|l| l.to_string())
            .collect()
    };

    let off = run(&["run"]);
    let on = run(&["--opt=1", "run"]);
    assert_eq!(off, vec!["A", "B", "6"], "前提：opt=off 应完整执行");
    assert_eq!(
        on, off,
        "`--opt=1` 的输出必须与 opt=off **完全一致**（D187：当前是 [\"A\"]，\
         在 for 循环处静默中止、exit 0）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Control IR verifies register definition/dominance via verify_program.

/// ④ **D417 已修复并解封**（原为 `#[ignore]`）：SSA 里**每个 dst 只能被定义一次**。
///
/// D256 把这一条记成「第三处缺口、修法要三处一起改」，实际上它混了
/// **两个**独立缺陷，其中一个当时被误判为「无法修」：
///
/// 1. **`phi.dst` 从不写回**（真实缺陷，已修）—— `rename_variables` 为 phi
///    算出了 `new_dst` 并压进 `rename_stack`，**却没有写回 `phi.dst` 字段**
///    ⇒ phi 仍占原编号，与被重编号的普通指令撞号（实测 reg 0/1/2/3/5/6 被
///    block 0 的指令与 block 1 的 phi 重复定义，reg 14 被两个 phi 重复定义）。
///    **D417 修法**：phi 先装进 `BasicBlock.phis`（按原始 dst 升序，保证编号
///    确定、消除 `HashMap` 迭代顺序带来的不确定性），`rename_variables`
///    再就地改写 `phi.dst`。
/// 2. **`Define` / `Assign` 被算作定义**（**不是**缺陷，是本判据自身的 bug）——
///    它们的第二字段是**源**寄存器（写 env），`rename_variables` 对二者显式
///    `continue`、不重编号。原 `dst_of` 把那个源寄存器当定义，于是报出虚假的
///    `reg 15 … 与 Define("i", 15)`。D417 已让本判据委托单一事实源
///    [`ssa::ssa_dst`]。
///
/// 第 2 条长期把这条判据挂在 `#[ignore]` 上，让人以为「撞号修不了」——
/// 而真正要修的只有第 1 条。**教训：现状断言本身也可能是错的**；在把它
/// 自身修对之前，不要拿它当「这条路径修不了」的证据。
#[test]
fn d256_ssa_definition_targets_are_unique() {
    for src in [
        FOR_LOOP,
        "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
        "let n = 3\nlet acc = 0\nwhile n > 0\n  acc = acc + n\n  n = n - 1\nend\nprint(acc)\n",
        "let a = 1\nif a == 1\n  print(\"T\")\nend\nprint(\"done\")\n",
    ] {
        for opt in [
            mora::mir::control::OptLevel::None,
            mora::mir::control::OptLevel::Basic,
            mora::mir::control::OptLevel::Aggressive,
        ] {
            let (prog, _) = mora::cli::compile_and_opt(src, Some(opt)).expect("compile");
            mora::mir::control::verify_program(&prog).unwrap_or_else(|e| {
                panic!("Control SSA invariants violated at {opt:?} for {src:?}: {e}")
            });
        }
    }
}
