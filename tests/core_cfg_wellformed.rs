//! v0.104.6 D479：Core 层 CFG 的**结构良构性**守卫。
//!
//! ## 缺陷背景
//!
//! Core 层的分支块号此前是**预分配**的（`next_block_id + 1 / +2 / +3`），
//! 而块号实际是**顺序分配**的（`flush_block` 单调递增）⇒ 嵌套分支时，
//! 外层预留的号与内层实际拿到的号**相撞** ⇒ 边指错、甚至成环。
//!
//! 实测（一次性探针打印 Core 块结构，`if` 里套 `if`）：
//!
//! ```text
//! block 0: Branch(cond=0, true_bb=1, false_bb=3)   ← 外层 if
//! block 1: Branch(cond=1, true_bb=2, false_bb=4)   ← 内层 if（在外层的 then 里）
//! block 2: <内层 then 体>  Jump(4)
//! block 3: []  Jump(4)                              ← 外层 else（空）
//! block 4: []  Jump(3)                              ← ⇒ 3 ↔ 4 成环
//! ```
//!
//! D479 改成「**先降维、后回填**」：降完两个分支体才回填目标与汇合块。
//!
//! ## 本守卫守什么
//!
//! 对一批**含嵌套分支**的程序构造 CoreFunction，断言：
//!
//! ① 块 id **唯一且连续**（0..n）；
//! ② 每条分支/跳转的目标块**存在**；
//! ③ 块内 `CoreInst::Branch` 与块终结指令 `CoreTerminator::Branch`
//!    的目标**一致**（Core 模型把分支信息存了两处，改一处漏一处就分叉）；
//! ④ 从入口可达的块 = 全部块（**无死块**）；
//! ⑤ 没有**未回填**的块（终结指令不是 `Unreachable` 占位）；
//! ⑥ **无环**分支程序里没有环（循环的回边合法，故只对无环程序断言）。
//!
//! 旧实现在 `nested_if` 上会同时违反 ④（出口块不可达）与 ⑥（3 ↔ 4 成环）✓。
//!
//! ## ④ 的适用范围
//!
//! 「无死块」在外层 Core CFG 上是**普遍成立**的：顶层 `return` 根本不合法
//! （只在 task / 闭包体里合法），而 task 体走 `MirInst::TaskDef`（不进 Core
//! CFG）、闭包体走子上下文（见下方已知限制）⇒ 外层 CFG 里**不会出现**
//! `Return` 终结符，也就不会留下「提前 return 之后的死续接块」。

use std::collections::BTreeSet;

use mora::mir::core::{CoreInst, CoreTerminator};

/// 构造一个程序的 CoreFunction（走完整 9 层里的 EHIR → Core 段）。
fn core_of(src: &str) -> mora::mir::core::CoreFunction {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("解析");
    let fcfg = mora::mir::witness_to_fcfg::witness_to_fcfg(&witnesses);
    let table = mora::typeck::export::export_type_table(&witnesses);
    let ehir = mora::typeck::annotate::annotate(&fcfg, &table);
    mora::mir::ehir_to_core::ehir_to_core(&ehir, vec![], func.effects.clone())
}

/// 一个块的后继（按**终结指令**算 —— 它是控制流的权威来源）。
fn successors(b: &mora::mir::core::CoreBlock) -> Vec<usize> {
    match &b.terminator {
        CoreTerminator::Branch {
            true_bb, false_bb, ..
        } => vec![*true_bb, *false_bb],
        CoreTerminator::Jump(t) => vec![*t],
        CoreTerminator::Return(_) | CoreTerminator::Unreachable => vec![],
    }
}

/// 块内 `Branch` 指令给出的目标。
fn inst_branch_targets(b: &mora::mir::core::CoreBlock) -> Option<(usize, usize)> {
    b.insts.iter().find_map(|i| match i {
        CoreInst::Branch {
            true_bb, false_bb, ..
        } => Some((*true_bb, *false_bb)),
        _ => None,
    })
}

/// ① id 唯一且连续；② 目标块存在；③ 指令与终结指令的分支目标一致。
fn assert_structurally_sound(core: &mora::mir::core::CoreFunction, tag: &str) {
    let ids: Vec<usize> = core.blocks.iter().map(|b| b.id).collect();
    let expected: Vec<usize> = (0..core.blocks.len()).collect();
    assert_eq!(
        ids, expected,
        "[{tag}] 块 id 必须唯一且连续（0..n），实际 {ids:?}"
    );

    for b in &core.blocks {
        let targets = match &b.terminator {
            CoreTerminator::Branch {
                true_bb, false_bb, ..
            } => vec![*true_bb, *false_bb],
            CoreTerminator::Jump(t) => vec![*t],
            _ => vec![],
        };
        for t in &targets {
            assert!(
                *t < core.blocks.len(),
                "[{tag}] 块 {} 的目标 {t} 越界（只有 {} 个块）",
                b.id,
                core.blocks.len()
            );
        }
        // ③ 指令与终结指令的分支目标必须一致
        if let CoreTerminator::Branch {
            true_bb, false_bb, ..
        } = &b.terminator
        {
            let inst = inst_branch_targets(b).unwrap_or_else(|| {
                panic!(
                    "[{tag}] 块 {} 的终结指令是 Branch，块内却没有 Branch 指令",
                    b.id
                )
            });
            assert_eq!(
                inst,
                (*true_bb, *false_bb),
                "[{tag}] 块 {} 的 Branch 指令目标 {inst:?} 与终结指令目标 \
                 ({true_bb}, {false_bb}) 不一致 —— Core 模型把分支信息存了两处",
                b.id
            );
        }
    }
}

/// ④ 无死块（从入口可达 = 全部块）。
fn assert_no_dead_block(core: &mora::mir::core::CoreFunction, tag: &str) {
    let n = core.blocks.len();
    let mut seen = BTreeSet::new();
    let mut stack = vec![core.entry];
    while let Some(id) = stack.pop() {
        if id >= n || !seen.insert(id) {
            continue;
        }
        stack.extend(successors(&core.blocks[id]));
    }
    let dead: Vec<usize> = (0..n).filter(|i| !seen.contains(i)).collect();
    assert!(
        dead.is_empty(),
        "[{tag}] 有不可达块 {dead:?}（可达集 = {seen:?}）—— 分支目标指错或出口块没接上"
    );
}

/// ⑤ 没有未回填的块（占位终结指令 = `Unreachable`）。
fn assert_all_backfilled(core: &mora::mir::core::CoreFunction, tag: &str) {
    let pending: Vec<usize> = core
        .blocks
        .iter()
        .filter(|b| matches!(b.terminator, CoreTerminator::Unreachable))
        .map(|b| b.id)
        .collect();
    assert!(
        pending.is_empty(),
        "[{tag}] 块 {pending:?} 的终结指令仍是 `Unreachable` 占位 —— \
         分支目标/汇合块**没有回填**（D479 的回填漏了一处）"
    );
}

/// ⑥ 无环程序里没有环。
fn assert_acyclic(core: &mora::mir::core::CoreFunction, tag: &str) {
    let n = core.blocks.len();
    // 迭代式 DFS + 栈上标记，检出回边
    let mut color = vec![0u8; n]; // 0=未访问 1=在栈上 2=已完成
    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, Vec<usize>)> = vec![(start, successors(&core.blocks[start]))];
        color[start] = 1;
        while let Some((id, mut succs)) = stack.pop() {
            while let Some(s) = succs.pop() {
                if s >= n {
                    continue;
                }
                match color[s] {
                    1 => panic!(
                        "[{tag}] CFG 有环：块 {s} 仍在栈上（经块 {id} 回到它）—— \
                         分支目标指错（回边只应来自循环）"
                    ),
                    0 => {
                        color[s] = 1;
                        let next = successors(&core.blocks[s]);
                        stack.push((s, next));
                    }
                    _ => {}
                }
            }
            color[id] = 2;
        }
    }
}

/// 无环分支程序：块结构必须良构。
#[test]
fn branch_cfgs_are_well_formed() {
    let cases: [(&str, &str); 5] = [
        (
            "if_else",
            "if true then\n  print(1)\nelse\n  print(2)\nend\n",
        ),
        (
            "nested_if_in_then",
            "if true then\n  if false then\n    print(1)\n  end\nelse\n  print(2)\nend\n",
        ),
        (
            "nested_if_in_else",
            "if false then\n  print(0)\nelse\n  if true then\n    print(1)\n  end\nend\n",
        ),
        (
            "or_inside_then",
            "if true then\n  let x = true or false\n  print(x)\nelse\n  print(2)\nend\n",
        ),
        (
            "and_inside_else",
            "if false then\n  print(0)\nelse\n  let x = false and true\n  print(x)\nend\n",
        ),
    ];
    for (tag, src) in cases {
        let core = core_of(src);
        assert_structurally_sound(&core, tag);
        assert_no_dead_block(&core, tag);
        assert_all_backfilled(&core, tag);
        assert_acyclic(&core, tag);
    }
}

/// 循环程序：允许回边（环），但其余不变量仍须成立。
#[test]
fn loop_cfgs_are_well_formed_except_for_back_edges() {
    let cases: [(&str, &str); 2] = [
        ("while", "let i = 0\nwhile i < 3\n  print(i)\nend\n"),
        (
            "while_with_nested_if",
            "let i = 0\nwhile i < 3\n  if i == 1 then\n    print(i)\n  end\nend\n",
        ),
    ];
    for (tag, src) in cases {
        let core = core_of(src);
        assert_structurally_sound(&core, tag);
        assert_no_dead_block(&core, tag);
        assert_all_backfilled(&core, tag);
    }
}

/// 无分支程序：单块，且 `if` 的降维块数被钉住。
#[test]
fn simple_program_has_one_block() {
    let core = core_of("print(1)\n");
    assert_eq!(core.blocks.len(), 1, "无分支程序应只有 1 个块");
    assert_structurally_sound(&core, "simple");
    assert_no_dead_block(&core, "simple");
    assert_all_backfilled(&core, "simple");
}

/// `if` 的降维形状被钉住：分支块 + then 块 + else 块 + 汇合块 = 4 块。
#[test]
fn if_lowers_to_four_blocks() {
    let core = core_of("if true then\n  print(1)\nelse\n  print(2)\nend\n");
    assert_eq!(
        core.blocks.len(),
        4,
        "if 应降为 4 块（分支 / then / else / 汇合），实际 {} 块：{:#?}",
        core.blocks.len(),
        core.blocks
    );
}

/// **已知限制登记**（D479）：match 分支体 / 闭包体里的**嵌套分支**在 Core 层
/// 被**截断** —— 体里只保留首块，分支目标指向**并不存在**的块。
///
/// 根因是**模型**，不是降维：`CoreInst::ClosureCreate.body` 与
/// `EnumMatchArm.body` 都是**单个** `CoreBlock`，装不下多块 CFG。
/// ⇒ 修它要改 Core 模型（body → 多块 / CoreFunction），是独立的一批。
///
/// 本判据把这个缺口**钉住**：它现在**存在**。若将来 Core 模型改成能承载多块，
/// 本判据会失败 —— 那时请把它翻成正面断言（目标块存在），并同步
/// `docs/nine-layer-ir.md` 的「已知限制」登记。
#[test]
fn sub_body_nested_branches_are_truncated_known_limitation() {
    let core = core_of(
        "let x = 3\nmatch x with\n  3 -> if x > 0 then print(1) else print(2) end\n  \
         _ -> print(3)\nend\n",
    );
    // 取第一个 match 分支的体，检查它的分支目标是否落在体自身的块集合内。
    let mut dangling: Vec<(usize, usize)> = Vec::new();
    for b in &core.blocks {
        for inst in &b.insts {
            if let CoreInst::EnumMatch { arms, .. } = inst {
                for arm in arms {
                    let ids: BTreeSet<usize> = [arm.body.id].into_iter().collect();
                    for t in successors(&arm.body) {
                        if !ids.contains(&t) {
                            dangling.push((arm.body.id, t));
                        }
                    }
                }
            }
        }
    }
    assert!(
        !dangling.is_empty(),
        "**已知限制消失了**：match 分支体里的嵌套分支不再被截断（悬空目标 {dangling:?}\
         为空）—— Core 模型已能承载多块？\n\
         请把本判据翻成**正面**断言（体自身块集合内目标存在），并同步\
         `docs/nine-layer-ir.md` 的「已知限制」登记。"
    );
}
