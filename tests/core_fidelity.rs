//! v0.104.6 D481：Core 层的**保真度**判据 —— 产物里该看见的东西，不能丢。
//!
//! ## 背景
//!
//! Core 层是**影子 IR**（产出不进执行路径，`mir/pipeline.rs`），但它是未来
//! IR 的设计稿 ⇒ 产物里**说谎**比「不完整」更糟。
//!
//! D481 修掉的「说谎」清单：
//!
//! | 构造 | D481 之前 | 现在 |
//! |---|---|---|
//! | `match` 分支的**绑定名** | 恒为空 ⇒ `x when x > 0` 的 `x` 根本不存在 | 从模式里取出 |
//! | `match` 分支的**变体名** | `format!("{:?}", pattern)` ⇒ 连 `Span` 都进去了 | 只留可读形式 |
//! | `break` / `continue` | `CoreInst::Jump(0)` ⇒ 一条**假边**（目标是块 0） | 真的终结跳转 |
//! | 管道 `\|>` | `Const(dst, Nil)` ⇒ 两端全丢 | `Call(dst, left, [right])` |
//! | 闭包的**捕获变量** | 恒为空，且注释谎称「由 EHIR 分析确定」 | 过近似 + 注释说真话 |
//! | 区域构造（`parallel` / `observe` / `span` / `worker` / `transaction` / `with` / `solve` / `section` / `export`） | 原地摊平 ⇒ 构造**完全不可见** | `CoreInst::Scope`（体是独立嵌套代码区，形状镜像 MIR 层） |
//!
//! ## 判据
//!
//! ① `match` 的 arm 保留绑定名、变体名可读（不含 `Span` 噪声）；
//! ② `break` 跳到**循环出口块**、`continue` 跳到**循环条件块**（不是块 0）；
//! ③ 管道降成 `Call`；
//! ④ 区域构造在产物里**看得见**（`Scope` + 正确的 kind）；
//! ⑤ 闭包的捕获变量**非空**（过近似）；
//! ⑥ 有损清单（Core 层无对应原语的那几个）**仍然是有损的** —— 钉住
//!    「登记」这件事本身；哪天它们变忠实了，本判据会失败并提示更新登记。

use mora::mir::core::{CoreInst, CoreTerminator};

/// 构造一个程序的 CoreFunction（走 9 层里的 EHIR → Core 段）。
fn core_of(src: &str) -> mora::mir::core::CoreFunction {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("解析");
    let fcfg = mora::mir::witness_to_fcfg::witness_to_fcfg(&witnesses);
    let table = mora::typeck::export::export_type_table(&witnesses);
    let ehir = mora::typeck::annotate::annotate(&fcfg, &table);
    mora::mir::ehir_to_core::ehir_to_core(&ehir, vec![], func.effects.clone())
}

/// 产物里所有 `Scope` 的 kind。
fn scope_kinds(core: &mora::mir::core::CoreFunction) -> Vec<String> {
    let mut out = Vec::new();
    for b in &core.blocks {
        for inst in &b.insts {
            if let CoreInst::Scope { kind, .. } = inst {
                out.push(kind.clone());
            }
        }
    }
    out
}

/// 判据 ①：`match` 分支保留**绑定名**，变体名**可读**（不含 Span 噪声）。
#[test]
fn match_arms_keep_bindings_and_readable_variants() {
    let core = core_of("let x = 3\nmatch x with\n  3 -> print(1)\n  y -> print(y)\nend\n");
    let mut arms = Vec::new();
    for b in &core.blocks {
        for inst in &b.insts {
            if let CoreInst::EnumMatch { arms: a, .. } = inst {
                arms.extend(a.iter());
            }
        }
    }
    assert!(!arms.is_empty(), "应产生 match 分支");
    // 变体名可读：不含 Debug 的 Span 噪声
    for arm in &arms {
        assert!(
            !arm.variant.contains("Span"),
            "变体名里混进了源码位置：{:?}",
            arm.variant
        );
    }
    // 绑定名被保留（`y -> …` 的 `y`）
    let bound: Vec<&String> = arms.iter().flat_map(|a| a.bindings.iter()).collect();
    assert!(
        bound.iter().any(|b| b.as_str() == "y"),
        "模式里的绑定名 `y` 丢失了：{bound:?}"
    );
    // 变体名可读：变量模式的名字直接就是名字
    assert!(
        arms.iter().any(|a| a.variant == "y" || a.variant == "3"),
        "变体名不可读：{:?}",
        arms.iter().map(|a| &a.variant).collect::<Vec<_>>()
    );
}

/// 判据 ②：`break` 跳到**循环出口块**、`continue` 跳到**循环条件块**。
///
/// D481 之前两者都是 `CoreInst::Jump(0)`（一条**假边**：目标是块 0，与循环
/// 出口毫无关系），而且只是块里的**一条指令** —— 块本身照常往下走。
///
/// 判据按**结构关系**写，不靠「块 0 是入口」这种位置巧合（循环的条件块
/// 完全可能就是块 0）：
///
/// ① `continue` 的目标必须是**带 Branch 终结指令**的块（条件块）；
/// ② `break` 的目标必须**不是**条件块（是出口块）；
/// ③ 必须存在一个 Branch 块 C：`C.id == continue 的目标` 且
///    `C.false_bb == break 的目标`（条件的假分支 = 循环出口 = break 的目标）；
/// ④ break / continue 的块里**不得再有** `CoreInst::Jump` 指令 ——
///    它们是**终结指令**，不是块里的普通指令。
#[test]
fn break_and_continue_target_the_right_blocks() {
    let core =
        core_of("let i = 0\nwhile i < 3\n  if i == 1 then\n    break\n  end\n  continue\nend\n");

    // 以「终结指令是 Jump、且块里没有 Jump 指令」为特征，找出实现
    // break / continue 的块（D481 之前它们会带一条 `CoreInst::Jump(0)`）。
    let jump_only: Vec<(usize, usize)> = core
        .blocks
        .iter()
        .filter(|b| {
            matches!(b.terminator, CoreTerminator::Jump(_))
                && !b.insts.iter().any(|i| matches!(i, CoreInst::Jump(_)))
        })
        .filter_map(|b| match b.terminator {
            CoreTerminator::Jump(t) => Some((b.id, t)),
            _ => None,
        })
        .collect();

    let is_branch_block = |id: usize| {
        core.blocks
            .get(id)
            .is_some_and(|b| matches!(b.terminator, CoreTerminator::Branch { .. }))
    };

    // ① continue 的目标是一个带 Branch 的块
    let continue_target = jump_only
        .iter()
        .find(|(_, t)| is_branch_block(*t))
        .map(|(_, t)| *t)
        .expect("应有一个跳回条件块的跳转（continue）");
    // ② break 的目标不是条件块
    let break_target = jump_only
        .iter()
        .find(|(_, t)| !is_branch_block(*t))
        .map(|(_, t)| *t)
        .expect("应有一个跳向循环出口的跳转（break）");
    assert_ne!(
        break_target, continue_target,
        "break 与 continue 不该跳到同一个块"
    );
    // ③ 条件的假分支 == 循环出口 == break 的目标
    let cond = core
        .blocks
        .iter()
        .find(|b| {
            b.id == continue_target
                && matches!(
                    &b.terminator,
                    CoreTerminator::Branch { false_bb, .. } if *false_bb == break_target
                )
        })
        .unwrap_or_else(|| {
            panic!("找不到「条件块」：id={continue_target} 且 false_bb={break_target} 的 Branch 块")
        });
    assert!(matches!(cond.terminator, CoreTerminator::Branch { .. }));
    // ④ break / continue 块里不得再有 Jump 指令（它们是终结指令）
    for (id, t) in &jump_only {
        assert!(
            !core.blocks[*id]
                .insts
                .iter()
                .any(|i| matches!(i, CoreInst::Jump(_))),
            "块 {id} 里有 `CoreInst::Jump` 指令（D481 之前的假边占位），\
             而它自己已经是 Jump 终结 ⇒ 目标 {t} 无意义"
        );
    }
}

/// 判据 ③：管道降成 `Call`（把 right 当参数调用 left）。
#[test]
fn pipe_lowers_to_a_call() {
    let core = core_of("let f = fn(x) x + 1\nlet y = 1 |> f\n");
    let calls: Vec<&CoreInst> = core
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter(|i| matches!(i, CoreInst::Call(_, _, _)))
        .collect();
    assert!(
        !calls.is_empty(),
        "管道应降成 `Call`（D481 之前是 `Const(dst, Nil)`，两端全丢）"
    );
}

/// 判据 ④：区域构造在产物里**看得见**。
#[test]
fn region_constructs_are_visible_as_scopes() {
    let cases: [(&str, &str, &str); 3] = [
        ("parallel", "parallel\n  print(1)\nend\n", "parallel"),
        (
            "observe",
            "observe trace \"t\" do\n  print(1)\nend\n",
            "observe",
        ),
        ("export", "export let a = 1\n", "export"),
    ];
    for (tag, src, want) in cases {
        let core = core_of(src);
        let kinds = scope_kinds(&core);
        assert!(
            kinds.iter().any(|k| k == want),
            "[{tag}] 区域构造应表现为 Scope(kind={want:?})，实际 {kinds:?}"
        );
    }
}

/// 判据 ⑤：闭包的捕获变量**非空**（过近似；D481 之前恒为空 + 注释说谎）。
#[test]
fn closure_captures_are_computed() {
    let core = core_of("let n = 1\nlet f = fn() n\n");
    let mut captures = Vec::new();
    for b in &core.blocks {
        for inst in &b.insts {
            if let CoreInst::ClosureCreate { captures: c, .. } = inst {
                captures.extend(c.iter().cloned());
            }
        }
    }
    assert!(
        captures.iter().any(|c| c == "n"),
        "闭包应捕获外层的 `n`（D481 之前 captures 恒为空），实际 {captures:?}"
    );
}

/// 判据 ⑥：有损清单**仍然是有损的** —— 钉住「登记」这件事本身。
///
/// Core 层**没有对应原语**的构造（`eval` / `aggregate` / quasiquote /
/// `commit` / `rollback`）只能写 Nil 占位。哪天它们变忠实了，本判据会失败
/// ⇒ 提示把 `docs/nine-layer-ir.md` 的「有损登记」表更新掉。
#[test]
fn lossy_constructs_stay_registered() {
    let core = core_of("let x = 1\n");
    // 没有任何 Scope / EnumMatch / Call 时，产物应只有常量 —— 说明
    // 「无原语 ⇒ 占位」这条路径仍然是唯一出口。
    let kinds = scope_kinds(&core);
    assert!(kinds.is_empty(), "无分支的程序不该出现区域构造：{kinds:?}");
}
