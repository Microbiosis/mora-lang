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
//! | ① | **现在通过** | 钉住现状（`incoming` 为空），把根因**判据化**；修好后它会红，提醒翻转 |
//!
//! 这样修 `rename_variables` 之前，全量仍然是绿的；修完之后，两条 ignored
//! 变绿、① 变红 —— 三条一起告诉审查者「该翻转断言了」。

use mora::mir::ssa;

/// 编译一个含 `for` 循环的程序（不做优化），并对它做 SSA 构造。
fn ssa_of(src: &str) -> ssa::MirSsaFunction {
    let (func, _w) = mora::cli::compile_and_opt(src, None).expect("compile should succeed");
    ssa::construct(&func)
}

const FOR_LOOP: &str = "for i in [1,2,3]\n  print(i)\nend\nprint(\"done\")\n";

/// ① **现在通过**：钉住现状 —— `for` 循环的 phi 全部没有 incoming。
///
/// 这条不是「期望正确行为」，而是「把已定位的缺陷机制固定下来」。
/// 修好 `rename_variables` 后本条会红；那时请把它翻转成 ② 的形式。
#[test]
fn d254_current_state_for_loop_phis_have_empty_incoming() {
    let s = ssa_of(FOR_LOOP);
    let phis: Vec<_> = s.blocks.iter().flat_map(|b| b.phis.iter()).collect();
    assert!(
        !phis.is_empty(),
        "前提失效：`for` 循环竟没有产生任何 phi —— 块划分可能变了，本判据需重写"
    );
    let with_incoming: Vec<_> = phis.iter().filter(|p| !p.incoming.is_empty()).collect();
    assert!(
        with_incoming.is_empty(),
        "D254 的前提已改变：有 {} 个 phi 带上了 incoming（{}）—— \
         若 `rename_variables` 已被修好，请把本条翻转成「所有 phi 都覆盖其全部前驱」",
        with_incoming.len(),
        with_incoming
            .iter()
            .map(|p| format!("{:?}", p.incoming))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// ② **修复后应通过**（当前挂起）：每个 phi 的 incoming 必须覆盖该块的
/// **全部前驱**。
///
/// 「全部前驱」而不是「至少一条」是关键：`for` 循环的循环变量 phi 有两个
/// incoming（preheader 给初值、body 回边给自增值），少一条 ⇒ 回边 copy 丢失
/// ⇒ 循环不退出。
///
/// ⚠ **验牙齿时顺带发现：修 `rename_variables` 远不止「incoming + preds」两处。**
///
/// 第一次去掉本条的 ignore 标记时，实测报错是
/// `块 1 的 phi(dst=15) 的 incoming 前驱 [] != 该块前驱 [2]` ——
/// 块 1 是循环 header，本该有**两个**前驱（preheader 块 0 + body 回边块 2），
/// 而 `BasicBlock.preds` 只记了 `[2]`。这是**第二处**缺口。
///
/// D256 进一步在 `deconstruct` 的产物上发现**第三处**：`rename_variables`
/// 对 `Define` / `Assign` 用 `continue` **跳过重编号**（因为
/// `set_dst` 把它们的第二字段当 dst，改了会错改源寄存器），于是这些指令
/// **保留原编号**；而其余指令被重编号到从 0 递增的 `reg_counter`
/// ⇒ **两个编号空间必然重叠**。实测：
///
/// ```text
///  6: Call(14, "len", [6])     写 14  ┐ 撞号
/// 14: Var(14, "i")             写 14  ┘
///  7: Const(7, Int(1))         写 7   ┐ 撞号 —— 循环增量被 print 的返回值覆盖
/// 15: Call(7, "print", [14])   写 7   ┘
/// ```
///
/// ⇒ **即使 ① ② 都修好，只要撞号还在，循环依然会坏**（增量被覆盖 ⇒
/// 递增逻辑错乱）。修的时候必须**同时**解决三件事：
/// ① `phi.incoming` 的填充、② `preds` 的完整、③ 重编号避开 Define/Assign
/// 保留的原编号（或一并重编号它们）。
#[test]
#[ignore = "D254：rename_variables 尚未实现 phi incoming 填充；修复后请去掉本标记"]
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

/// 下必须与 `--opt=off` **输出完全一致**。
///
/// 这是 D187 那五条「钉住坏行为」断言**翻转后**应有的样子：它们届时也应
/// 一并改成「与 opt=off 等价」。
#[test]
#[ignore = "D254：等 rename_variables 修好；届时 D187 的五条断言也应一并翻转"]
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
fn dst_of(i: &ssa::SsaInst) -> Option<usize> {
    use ssa::SsaInst::*;
    Some(match i {
        Const(d, _)
        | Var(d, _)
        | BinaryOp(d, _, _, _)
        | Call(d, _, _)
        | ListLit(d, _)
        | DictLit(d, _)
        | Index(d, _, _)
        | IndexAssign(d, _, _)
        | MethodCall(d, _, _, _)
        | Pipe(d, _, _)
        | Prompt(d, _)
        | Copy(d, _)
        | Define(_, d)
        | Assign(_, d)
        | Expr(d) => *d,
    })
}

/// ④ **修复后应通过**（当前 `#[ignore]`）：SSA 里**每个 dst 只能被定义一次**。
///
/// 这是 D256 发现的**第三处**缺口，也是最根本的一处：
/// `rename_variables` 对 `Define` / `Assign` 用 `continue` 跳过重编号
/// （因为 `set_dst` 把它们的第二字段当 `dst`，改了会错改**源**寄存器），
/// 于是它们**保留原编号**；而其余指令被重编号到从 0 递增的 `reg_counter`
/// ⇒ **两个编号空间必然重叠**。
///
/// 实测（去掉本条的 ignore 后，5 处重复定义）：
///
/// ```text
/// reg 5  被 block 0 的 Call(5, "len", [3])   与 block 1 的 phi
/// reg 6  被 block 0 的 Const(6, Int(1))      与 block 1 的 phi
/// reg 14 被 block 1 的 BinaryOp(...)         与 block 2 的 phi
/// reg 15 被 block 1 的 phi                   与 block 2 的 Index(15, 3, 11)
/// reg 15 被 block 2 的 Index(15, 3, 11)      与 block 2 的 Define("i", 15)
/// ```
///
/// ⇒ 三处「重编号」里**只有普通指令真的写回了**：
/// - `phi.dst`：`rename_variables` 算出了 `new_dst` 并压进 `rename_stack`，
///   **却没写回 `phi.dst` 字段** ⇒ phi 仍占原编号（上面 4 条里的前 3 条）；
/// - `Define` / `Assign`：被 `continue` 整个跳过 ⇒ 也占原编号；
/// - 其余指令：`set_dst` 正常写回 ✓
///
/// 而其余指令被重编号到从 0 递增的 `reg_counter` ⇒ 与前两者的原编号
/// **必然重叠**。后果实测：`len` 的结果被 `Var` 覆盖、**循环增量 1 被
/// `print` 的返回值覆盖** ⇒ 递增逻辑错乱。
///
/// ⇒ 即使 ②（`incoming` 覆盖 `preds`）修好，只要撞号还在，**循环依然会坏**。
///
/// 修法提示：三处要**一起**改 —— 重编号必须真正写回 `phi.dst`，并让
/// `Define` / `Assign` 也纳入统一编号空间（改之前要先修 `set_dst` 对它们的
/// 语义：第二字段是**源**寄存器，不是 dst）。
/// ⇒ 即使 ②（`incoming` 覆盖 `preds`）修好，只要撞号还在，**循环依然会坏**：
/// 增量被 `print` 的返回值覆盖 ⇒ 递增逻辑错乱。
///
/// 修法提示：重编号必须**避开** Define/Assign 保留的原编号，
/// 或把这两类指令也纳入统一重编号（此时要先修 `set_dst` 对它们的语义）。
#[test]
#[ignore = "D256：Define/Assign 保留原编号与其余重编号撞号；修复后请去掉本标记"]
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
