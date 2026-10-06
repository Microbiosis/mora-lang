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

use std::process::Command;

fn run(src: &str, tag: &str, opt: Option<&str>) -> (i32, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("mora_d312_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
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
        vec!["A".to_string(), "B".to_string(), "6.0".to_string()],
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
        let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
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

use mora::common::BinaryOp;
use mora::mir::{MirFunction, MirInst};
use mora::value::Value;

/// 复刻 `fcfg_lower.rs` 的 `Node::For` lowering 的**寄存器分配形状**：
/// 一个物理寄存器在**循环前**（`Const`）和**回边上**（`idx += 1`）各写一次。
///
/// ⚠ 这不是可执行程序 —— `ListLit` 的元素寄存器 5/6/7 故意不定义。
/// 本文件只用 `construct` 做 CFG/支配/phi 分析，不跑它。
fn for_loop_mir() -> MirFunction {
    let body = vec![
        MirInst::Const(0, Value::Int(0)),   // 0  __idx = 0      ← 写 reg 0
        MirInst::Const(1, Value::Int(1)),   // 1  __one = 1
        MirInst::Const(2, Value::Int(3)),   // 2  __limit = 3
        MirInst::ListLit(4, vec![5, 6, 7]), // 3  __iter
        MirInst::BinaryOp(3, 0, BinaryOp::GreaterEqual, 2), // 4  cond           ← loop_start
        MirInst::JumpIf(3, 9),              // 5  退出 → 指令 9
        MirInst::Index(8, 4, 0),            // 6  __iter[__idx]  ← 读 reg 0
        MirInst::BinaryOp(0, 0, BinaryOp::Add, 1), // 7  __idx += 1     ← 又写 reg 0
        MirInst::Jump(4),                   // 8  回边 → 指令 4
        MirInst::Const(9, Value::Nil),      // 9  循环之后
    ];
    MirFunction {
        params: vec![],
        body,
        n_regs: 10,
        ..Default::default()
    }
}

/// **守卫的必要性**：带循环携带寄存器的函数，`construct` **一定**会产生 phi。
///
/// 这条是 D312 守卫的根据。若哪天这条红了（`for` 形状不再产生 phi），
/// 说明根因之一被真正修掉了 —— 此时应当**重新评估** `opt.rs` 的守卫，
/// 而不是让它无声地变成死代码。
#[test]
fn loop_carried_register_makes_construct_emit_phis() {
    let ssa = mora::mir::ssa::construct(&for_loop_mir());
    let phis: Vec<(usize, usize)> = ssa
        .blocks
        .iter()
        .map(|b| (b.id, b.phis.len()))
        .filter(|(_, n)| *n > 0)
        .collect();
    assert!(
        !phis.is_empty(),
        "带循环携带寄存器的函数应当产生 phi —— `opt.rs` 的 D312 守卫正是以此为\
         判据（SSA 里出现 phi 就整体跳过 SSA）。\n\
         若本条失败：SSA 可能已被真正修好，请重新评估该守卫是否还需要。"
    );
}

/// **phi 的 `incoming` 恒为空** —— D312 取保守方向的**直接理由**。
///
/// `insert_phi_nodes` 以 `incoming: Vec::new()` 建 phi；`rename_variables` 收的
/// 是 `phi_map: &HashMap<…>`（**不可变引用**）且内部用 `clone()` ⇒ 无处可写。
/// `deconstruct` 的 `pred_copies` 唯一来源就是 `phi.incoming` ⇒ 前驱 copy
/// 一条都不发出 ⇒ phi 目标寄存器**没有生产者**。
///
/// 如果将来实现了教科书式的支配树 DFS + 逐前驱边记录 incoming，
/// **这条会红 —— 那是好信号**：那时应当把 D312 的保守守卫换成真正的修复。
#[test]
fn phi_incoming_is_always_empty_today() {
    let ssa = mora::mir::ssa::construct(&for_loop_mir());
    let total_phi: usize = ssa.blocks.iter().map(|b| b.phis.len()).sum();
    let total_incoming: usize = ssa
        .blocks
        .iter()
        .flat_map(|b| b.phis.iter())
        .map(|p| p.incoming.len())
        .sum();
    assert!(total_phi > 0, "前提：应当至少有一个 phi; 实得 {total_phi}");
    assert_eq!(
        total_incoming, 0,
        "**D312 现状**：`phi.incoming` 恒为空 ⇒ `deconstruct` 发不出任何前驱 copy。\n\
         实得 total_phi={total_phi} total_incoming={total_incoming}。\n\
         若本条失败（total_incoming > 0）：phi 机制已生效，请把 `opt.rs` 的 D312 \
         保守守卫换成真正的 SSA 修复，并重跑 56 个真实程序的差分。"
    );
}

/// **守卫不宽**：直线函数不产生 phi，**仍然走 SSA 优化**。
///
/// 防止有人把守卫写成「有回边就跳过」或「一律跳过」—— 那会让 D311 记录的
/// 「33 个程序恢复 SSA」被无声退回。
#[test]
fn straight_line_function_still_goes_through_ssa() {
    let func = MirFunction {
        params: vec![],
        body: vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(2)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            MirInst::Const(3, Value::Nil),
        ],
        n_regs: 4,
        ..Default::default()
    };
    let ssa = mora::mir::ssa::construct(&func);
    assert!(
        ssa.blocks.iter().all(|b| b.phis.is_empty()),
        "直线函数不应产生 phi ⇒ `opt.rs` 的 D312 守卫**不会**命中它 ⇒ \
         它仍然享受 SSA 优化。若本条失败，守卫可能已被写得过宽。"
    );
}
