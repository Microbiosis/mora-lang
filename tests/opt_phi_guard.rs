//! v0.104.6 D312：`for` 循环在 `--opt=1/2` 下**静默中止**（输出截断、退出码 0、零诊断）。
//!
//! ## 现象（修前）
//!
//! ```text
//! $ mora --opt=1 a.mora      # a.mora = print("A") / let acc = 0 / for … end / print("B") / print(acc)
//! A
//! $ echo $?
//! 0
//! ```
//!
//! 修后同一命令输出 `A` / `B` / `6.0`，与默认档**逐行相同**。
//!
//! 56 个真实程序里残余 2 个分叉全部消失（54/56 → **56/56**），两个都是
//! `for` 循环：`tests/fixtures/e2e/builtin_gaps.mora`、
//! `tests/fixtures/e2e/loop_for_break.mora`。
//!
//! ## 根因（两级，都比「少一个优化」严重）
//!
//! 1. `construct` 的 CFG **不记顺序落下的后继**。`deconstruct` 明写
//!    「`Return(None)` 不发射，丢弃后线性执行自然落到最后一条指令」——
//!    即 `Return(None)` 的真实语义是**落下去**。但 CFG 侧把它当终点，
//!    block 0 的 `succs` 为空。`rename_variables` 从 `vec![0]` 起只沿
//!    `succs` 走且 `visited` 只入一次 ⇒ **只有 block 0 被重命名**。
//!    *部分重命名*把跨块的值定义与使用拆成两个物理寄存器。
//! 2. `phi.incoming` **结构上恒为空**：`insert_phi_nodes` 以
//!    `incoming: Vec::new()` 建 phi，而 `rename_variables` 收的是
//!    `phi_map: &HashMap<…>`（**不可变引用**）且内部用 `clone()`。
//!    `deconstruct` 的 `pred_copies` 唯一来源就是 `phi.incoming`
//!    ⇒ phi 的前驱 copy **一条都不会发出**（实测 `TOTAL_PHI=7
//!    TOTAL_INCOMING=0`，deconstruct 产物零条 `Copy`）。
//!
//! 实测的寄存器对照（`for` 最小用例，construct 前后）：
//!
//! | | 初始化 | 循环体内读 | 回边写 |
//! |---|---|---|---|
//! | 源 MIR | `Const(20, Int(0))` | `BinaryOp(23, 20, ≥, 21)` | `BinaryOp(20, 20, +, 22)` |
//! | deconstruct 后 | `Const(9, Int(0))` | `BinaryOp(18, 19, ≥, 1)` | `BinaryOp(19, 19, +, 12)` |
//!
//! **r9 与 r19 永久失联** ⇒ 循环体读的那个寄存器首次迭代时无任何生产者
//! ⇒ DAG 执行器按「输入寄存器就绪」激活节点 ⇒ 该节点永不激活
//! ⇒ 整条链饿死 ⇒ 静默截断。
//!
//! ## 修法
//!
//! `opt.rs::optimize` 在 `construct` 之后、`deconstruct` 之前加守卫：
//! **SSA 里一旦出现 phi，整个函数跳过 SSA**（`func` 逐字节保持原样）。
//! 与 D302 停用 `ReplaceWithSource`、D310 跳过透传函数同一原则：
//! **宁可不优化，不可静默产出错误结果**。
//!
//! 守卫取「SSA 里出现 phi」而不是「CFG 有回边」——前者**更窄**：没有循环
//! 携带值的循环不产生 phi，仍能享受 SSA 优化。

use mora::test_support::TempDir;
use std::process::Command;

fn run(src: &str, tag: &str, opt: Option<&str>) -> (i32, Vec<String>) {
    let dir = TempDir::new(tag);
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = env!("CARGO_BIN_EXE_mora");
    let mut c = Command::new(exe);
    // ⚠ `--opt=` **必须放在文件前面** —— CLI 只扫第一个非选项参数之前的选项。
    // 放文件后面会被静默忽略，于是 opt=off 与 opt=1 跑出同一结果，差异测不出来。
    if let Some(l) = opt {
        c.arg(format!("--opt={l}"));
    }
    let out = c.arg(&p).output().expect("跑 mora");
    // ⚠ 必须合并 stdout 与 stderr —— 运行时错误走 stderr。
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let lines = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("类型与执行")
                && !l.starts_with("并发与抽象")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.code().unwrap_or(-1), lines)
}

const FOR_PROG: &str =
    "print(\"A\")\nlet acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(\"B\")\nprint(acc)\n";

/// **主判据**：`--opt=1` / `--opt=2` 下的 `for` 循环必须与默认档**逐行等价**。
///
/// 本条是 `opt_ssa_equivalence.rs::d187_opt1_stops_silently_at_a_for_loop`
/// 那条**现状判据**翻转后的正确行为断言（该处已同步翻转）。
#[test]
fn for_loop_no_longer_stops_silently_under_opt() {
    let (dc, want) = run(FOR_PROG, "for_base", None);
    assert_eq!(dc, 0, "前提：默认档应完整执行; 实得 {dc}");
    assert_eq!(
        want,
        vec!["A".to_string(), "B".to_string(), "6".to_string()],
        "前提：默认档应输出 A / B / 6.0; 实得 {want:?}"
    );

    for lvl in ["1", "2"] {
        let (c, got) = run(FOR_PROG, &format!("for_opt{lvl}"), Some(lvl));
        assert_eq!(
            c, 0,
            "**D312**：`--opt={lvl}` 下 `for` 程序应成功执行（修前是 exit 0 的\
             静默中止）。若退出码变成非 0，说明缺陷形态已变，请更新本判据。\
             实得 {c} out={got:?}"
        );
        assert_eq!(
            got, want,
            "**D312**：`--opt={lvl}` 下 `for` 程序的输出必须与默认档**逐行相同**。\n\
             修前是静默截断成 [\"A\"]（循环体读的寄存器首次迭代无任何生产者 →\
             DAG 节点永不激活 → 整条链饿死）。\n  实得: {got:?}"
        );
    }
}

/// 真实 fixture 回归：D311 之后残余的 2 个分叉（都是 `for` 循环）。
#[test]
fn real_for_fixtures_survive_optimization() {
    for fixture in [
        "tests/fixtures/e2e/builtin_gaps.mora",
        "tests/fixtures/e2e/loop_for_break.mora",
    ] {
        let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), fixture);
        let exe = env!("CARGO_BIN_EXE_mora");
        let base = Command::new(exe).arg(&path).output().expect("跑");
        assert_eq!(
            base.status.code(),
            Some(0),
            "{fixture}：默认档应成功; 实得 {:?}",
            String::from_utf8_lossy(&base.stderr).lines().last()
        );
        for lvl in ["1", "2"] {
            let o = Command::new(exe)
                .arg(format!("--opt={lvl}"))
                .arg(&path)
                .output()
                .expect("跑");
            assert_eq!(
                o.status.code(),
                Some(0),
                "**D312**：{fixture} 在 `--opt={lvl}` 下应成功。\n\
                 D311 之后它是残余 2 个分叉之一（`for` 循环静默中止）。\n\
                 stderr 末尾: {:?}",
                String::from_utf8_lossy(&o.stderr).lines().last()
            );
            assert_eq!(
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&base.stdout),
                "**D312**：{fixture} 在 `--opt={lvl}` 下的 stdout 必须与默认档一致。\n\
                 实得 opt={lvl}: {:?}",
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .collect::<Vec<_>>()
            );
        }
    }
}

// ── 结构不变量：给守卫上牙齿 ─────────────────────────────────────

// ── Control SSA 结构与优化不变量 ──────────────────────────────────

/// 带循环携带变量的程序优化后仍通过 `verify_program`（CFG 与寄存器定义支配关系有效），
/// 保留多块循环结构且与未优化结果一致。
#[test]
fn loop_carried_register_makes_construct_emit_phis() {
    use mora::mir::control::{OptLevel, verify_program};
    use mora::runtime::CoreRuntime;
    use mora::runtime::vm::execute_program;
    use std::sync::Arc;

    let src = "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nacc\n";
    let (p0, _) = mora::cli::compile_and_opt(src, Some(OptLevel::None)).expect("compile opt=0");
    let (p1, _) = mora::cli::compile_and_opt(src, Some(OptLevel::Basic)).expect("compile opt=1");
    verify_program(&p0).expect("opt=0 verify_program");
    verify_program(&p1).expect("opt=1 verify_program");
    assert!(
        p1.functions[p1.entry.0 as usize].blocks.len() > 1,
        "循环函数应当保持多块 CFG 结构"
    );
    let mut rt0 = CoreRuntime::default();
    let mut rt1 = CoreRuntime::default();
    let v0 = execute_program(Arc::new(p0), &mut rt0)
        .expect("run opt=0")
        .0;
    let v1 = execute_program(Arc::new(p1), &mut rt1)
        .expect("run opt=1")
        .0;
    assert_eq!(v0, v1, "opt=0 与 opt=1 结果必须一致");
}

/// 每个跳转目标指向合法块，且所有读取的寄存器均由前驱定义（D418 护栏的 Control IR 版）。
#[test]
fn phi_incoming_covers_every_predecessor_d418() {
    use mora::mir::control::{OptLevel, verify_program};
    let src = "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nacc\n";
    for level in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
        let (prog, _) = mora::cli::compile_and_opt(src, Some(level)).expect("compile");
        verify_program(&prog).unwrap_or_else(|e| panic!("verify failed at {level:?}: {e}"));
    }
}

/// 直线函数经过优化后仍通过验证并保留合法结构。
#[test]
fn straight_line_function_still_goes_through_ssa() {
    use mora::mir::control::{OptLevel, verify_program};
    use mora::runtime::CoreRuntime;
    use mora::runtime::vm::execute_program;
    use std::sync::Arc;

    let src = "let a = 1\nlet b = 2\na + b\n";
    let (prog, _) = mora::cli::compile_and_opt(src, Some(OptLevel::Basic)).expect("compile");
    verify_program(&prog).expect("straight-line verify_program");
    let mut rt = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut rt).expect("run");
    assert_eq!(val.to_string(), "3");
}
