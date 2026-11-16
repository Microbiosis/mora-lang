//! v0.104.6 D477：`or` / `and` 的**短路求值** —— rhs 只在必要时才求值。
//!
//! ## 缺陷背景
//!
//! D477 之前，生产路径（witness → FCFG → MIR）把 `or` / `and` 的 rhs 当作
//! **已算好的寄存器**（`Node::Or { rhs: Reg }`），而 `witness_to_fcfg` 把它
//! 排在 `Or` 节点**之前** ⇒ **rhs 无条件求值**：短路只作用于「取值」，
//! 不作用于「求值」。
//!
//! 实测（真实 CLI，rhs 是一句有副作用的 `task bump() : bool`）：
//!
//! ```text
//!   let a = true  or  bump()   → bump() 仍然执行（应短路）
//!   let b = false and bump()   → bump() 仍然执行（应短路）
//! ```
//!
//! 而**直连路径**（`mir::lower`）一直把 rhs 发射在 `JumpIf` **之后** ⇒ 它
//! 本就短路 ⇒ **两条路径语义不一致**（同一个程序、两条编译路径，副作用不同）。
//!
//! ## 判据（三层，缺一不可）
//!
//! ① **行为**（CLI 实跑）：短路时 rhs 的副作用**不发生**；不短路时发生。
//! ② **形状**（生产路径 MIR）：rhs 的指令在 `JumpIf` / `JumpIfNot` **之后**
//!    —— 改动前它们在之前，那正是「急切求值」的形状特征。
//! ③ **跨路径一致**：生产路径与直连路径对同一程序，rhs 的指令都排在
//!    分支指令**之后** —— 防止两条路径再次分叉（这次的缺陷就是这么来的）。

use mora::mir::control::{ControlInst, ControlProgram, ControlTerminator};
use mora::test_support::TempDir;
use std::process::Command;

/// 有副作用的 rhs：一个打印一行、返回 `true` 的 task。
const BUMP_TASK: &str = "task bump() : bool\n  print(\"BUMP\")\n  return true\nend\n";

/// 跑一个程序，返回 `(stdout, stderr, 退出码)`。
///
/// `opt` 是 `--opt=N` 的档位（`None` = 不带该参数）。⚠ 选项必须排在**子命令
/// 之前**（`mora --opt=1 run f.mora`）—— `main.rs` 只在 `args[1]` 认它。
fn run_opt(tag: &str, src: &str, opt: Option<&str>) -> (String, String, i32) {
    let dir = TempDir::new(tag);
    let path = dir.join("p.mora");
    std::fs::write(&path, src).expect("写临时程序");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mora"));
    if let Some(o) = opt {
        cmd.arg(o);
    }
    let out = cmd.arg("run").arg(&path).output().expect("跑 mora");
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code().unwrap_or(-1),
    )
}

/// 三个优化档位：关 / 基本 SSA / 激进 SSA。
///
/// ⚠ 短路求值是**控制流**性质，优化器（SSA 往返、phi copy、块重排）不得
/// 把它降级成「两边都算」—— 三个档位都必须给出同一结果。
const OPT_LEVELS: [Option<&str>; 3] = [None, Some("--opt=1"), Some("--opt=2")];

/// 判据 ①：短路时 rhs 的副作用**不发生**，不短路时**发生**。
///
/// 四条覆盖 `or` / `and` 的两个方向：
/// - `true or bump()`  → lhs 为真 ⇒ 短路 ⇒ bump 不该跑
/// - `false and bump()` → lhs 为假 ⇒ 短路 ⇒ bump 不该跑
/// - `false or bump()` → lhs 为假 ⇒ 不短路 ⇒ bump **该**跑
/// - `true and bump()` → lhs 为真 ⇒ 不短路 ⇒ bump **该**跑
#[test]
fn short_circuit_skips_rhs_evaluation_exactly_when_lhs_decides() {
    let src = format!(
        "{BUMP_TASK}\n\
         let a = true or bump()\n\
         let b = false and bump()\n\
         let c = false or bump()\n\
         let d = true and bump()\n\
         print(\"done\")\n"
    );
    for opt in OPT_LEVELS {
        let (stdout, stderr, code) = run_opt("short_circuit_behavior", &src, opt);
        let got = stdout.trim_end_matches('\n');
        assert_eq!(
            code, 0,
            "程序应正常退出（opt={opt:?}）。\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert_eq!(
            got, "BUMP\nBUMP\ndone",
            "短路语义错（opt={opt:?}）：只有**不该短路**的两条才该触发 rhs 的副作用。\n\
             实得 stdout: {stdout:?}\nstderr: {stderr}"
        );
    }
}

/// 判据 ①的补强：短路与否**不改变结果值**，只改变副作用的发生次数。
#[test]
fn short_circuit_keeps_the_value_semantics() {
    let src = "let a = true or false\nlet b = false or true\n\
               let c = true and false\nlet d = false and true\n\
               print(a)\nprint(b)\nprint(c)\nprint(d)\n";
    for opt in OPT_LEVELS {
        let (stdout, stderr, code) = run_opt("short_circuit_values", src, opt);
        assert_eq!(code, 0, "程序应正常退出（opt={opt:?}）。stderr: {stderr}");
        assert_eq!(
            stdout.trim_end_matches('\n'),
            "true\ntrue\nfalse\nfalse",
            "or/and 的取值语义被短路改造破坏了（opt={opt:?}）。\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
    }
}

/// 生产路径编译（witness → Control SSA）。
fn compile_production(src: &str) -> ControlProgram {
    mora::parser_v3::ParserV3::compile(src).expect("解析").0
}

/// 判据 ②：entry block 必须先经 `Branch` 短路决策，rhs 的 `Call(bump)` 必须在后续基本块发射（不得在 Branch 前急切求值）。
#[test]
fn rhs_call_is_emitted_after_the_branch() {
    let src = format!("{BUMP_TASK}\nlet x = true or bump()\n");
    let prog = compile_production(&src);
    let entry_fn = prog.func(prog.entry);
    let entry_bb = &entry_fn.blocks[entry_fn.entry.0 as usize];
    assert!(
        matches!(entry_bb.terminator, ControlTerminator::Branch { .. }),
        "entry 块应以 Branch 终结以实现短路，实际: {:?}",
        entry_bb.terminator
    );
    let entry_has_call = entry_bb
        .insts
        .iter()
        .any(|i| matches!(i, ControlInst::Call { .. }));
    assert!(
        !entry_has_call,
        "entry 块在 Branch 之前不应出现 rhs Call(bump)：{:#?}",
        entry_bb.insts
    );
    let later_has_call = entry_fn.blocks.iter().skip(1).any(|b| {
        b.insts
            .iter()
            .any(|i| matches!(i, ControlInst::Call { .. }))
    });
    assert!(
        later_has_call,
        "rhs Call(bump) 应位于后续条件分支基本块内: {:#?}",
        entry_fn.blocks
    );
}
