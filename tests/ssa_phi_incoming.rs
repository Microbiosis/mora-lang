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

use mora::mir::ssa;

/// 编译一个含 `for` 循环的程序（不做优化），并对它做 SSA 构造。
fn ssa_of(src: &str) -> ssa::MirSsaFunction {
    let (func, _w) = mora::cli::compile_and_opt(src, None).expect("compile should succeed");
    ssa::construct(&func)
}

const FOR_LOOP: &str = "for i in [1,2,3]\n  print(i)\nend\nprint(\"done\")\n";

/// ① **D418 已翻转**：phi 机制真正生效 —— 每条 phi 都带上了 incoming。
///
/// 翻转前本条钉的是现状（`for` 循环的 phi **全部**没有 incoming）。D418 把
/// `rename_variables` 改成支配树 DFS 并逐边记录 `φ.incoming` 后，
/// `rename_variables` 产出的每条 phi 都有非空 incoming。
///
/// 这条保留为**回归护栏**的价值在于：它把「phi 从未工作过」这个事实钉成了一个
/// 会红的断言 —— 将来谁再把 `incoming` 的填充改坏（例如把遍历退回沿 `succs`
/// 的平铺工作表），本条会立刻指出。
#[test]
fn d418_for_loop_phis_carry_incoming() {
    let s = ssa_of(FOR_LOOP);
    let phis: Vec<_> = s.blocks.iter().flat_map(|b| b.phis.iter()).collect();
    assert!(
        !phis.is_empty(),
        "前提失效：`for` 循环竟没有产生任何 phi —— 块划分可能变了，本判据需重写"
    );
    let without: Vec<_> = phis
        .iter()
        .filter(|p| p.incoming.is_empty())
        .map(|p| format!("块? var={}", p.var))
        .collect();
    assert!(
        without.is_empty(),
        "**D418**：存在没有 incoming 的 phi（{}）⇒ `deconstruct` 发不出它的前驱 \
         copy ⇒ 该寄存器**没有生产者** ⇒ 消费者读到未初始化槽（D312 的静默中止）。\n\
         本函数体没有前驱的块才会有这种 phi（不可达 ⇒ 死代码），那是允许的；\
         若这条在**有前驱**的块上触发，说明 `preds` 与 `incoming` 又对不上了。",
        without.join(", ")
    );
}

/// ② **D418 已解封**：每个 phi 的 incoming 必须覆盖该块的**全部前驱**。
///
/// 「全部前驱」而不是「至少一条」是关键：`for` 循环的循环变量 phi 有两个
/// incoming（preheader 给初值、body 回边给自增值），少一条 ⇒ 回边 copy 丢失
/// ⇒ 循环不退出。
///
/// 下面这段「验牙齿时顺带发现」的记档保留在案 —— 它记录了当初为什么把本条
/// 长期挂在 `#[ignore]` 上：**三处缺口必须一起修**，任何一处单独修都会得到
/// 一个「看起来通过了」的假绿。
#[test]
fn d254_phi_incoming_covers_every_predecessor() {
    for src in [
        FOR_LOOP,
        "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
        "let n = 3\nlet acc = 0\nwhile n > 0\n  acc = acc + n\n  n = n - 1\nend\nprint(acc)\n",
    ] {
        let s = ssa_of(src);
        for b in &s.blocks {
            // sorted Vec, not HashSet: HashSet's Debug order is nondeterministic,
            // which would make the failure message unstable between runs.
            let mut preds: Vec<_> = b.preds.clone();
            preds.sort_unstable();
            for phi in &b.phis {
                let mut from: Vec<_> = phi.incoming.iter().map(|(p, _)| *p).collect();
                from.sort_unstable();
                assert_eq!(
                    from, preds,
                    "块 {} 的 phi(dst={:?}) 的 incoming 前驱 {:?} != 该块前驱 {:?}\n\
                     程序: {:?}\n\
                     少一条 incoming ⇒ deconstruct 少插一条回边 copy ⇒ 循环不退出",
                    b.id, phi.dst, from, preds, src
                );
            }
        }
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
    let dir = std::env::temp_dir().join(format!("mora_d254_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("a.mora");
    std::fs::write(
        &file,
        "print(\"A\")\nlet acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\n\
         print(\"B\")\nprint(acc)\n",
    )
    .expect("write");
    let p = file.to_str().expect("path");

    let run = |args: &[&str]| -> Vec<String> {
        let out = std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
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
    assert_eq!(off, vec!["A", "B", "6.0"], "前提：opt=off 应完整执行");
    assert_eq!(
        on, off,
        "`--opt=1` 的输出必须与 opt=off **完全一致**（D187：当前是 [\"A\"]，\
         在 for 循环处静默中止、exit 0）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `SsaInst` 的 dst（SSA 里「这条指令定义了哪个寄存器」）。
///
/// **v0.104.6 D417 修正**：本函数此前把 `Define(_, d)` / `Assign(_, d)` 的
/// `d` 当作定义 —— 但那两类的第二字段是**源**寄存器（它们写的是 **env**），
/// `rename_variables` 对它们显式 `continue`、不重编号。把它算成定义会报出
/// **虚假撞号**（原注释里 `reg 15 … 与 block 2 的 Define("i", 15)` 那一条
/// 就是这一类），进而让本条判据长期挂着 `#[ignore]` 却**已经修好**。
/// 现在直接委托单一事实源 [`ssa::ssa_dst`]。
fn dst_of(i: &ssa::SsaInst) -> Option<usize> {
    match i {
        ssa::SsaInst::Define(_, _) | ssa::SsaInst::Assign(_, _) => None,
        other => ssa::ssa_dst(other),
    }
}

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
        let s = ssa_of(src);
        let mut owner: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
        let mut dup: Vec<String> = Vec::new();
        for b in &s.blocks {
            for phi in &b.phis {
                let who = format!("block {} 的 phi", b.id);
                if let Some(prev) = owner.insert(phi.dst, who.clone()) {
                    dup.push(format!("reg {} 被 {} 与 {} 重复定义", phi.dst, prev, who));
                }
            }
            for inst in &b.insts {
                let Some(d) = dst_of(inst) else { continue };
                let who = format!("block {} 的 {:?}", b.id, inst);
                if let Some(prev) = owner.insert(d, who.clone()) {
                    dup.push(format!("reg {} 被 {} 与 {} 重复定义", d, prev, who));
                }
            }
        }
        assert!(
            dup.is_empty(),
            "SSA 里同一个寄存器被定义了多次（撞号）：\n  - {}\n  程序: {:?}",
            dup.join("\n  - "),
            src
        );
    }
}
