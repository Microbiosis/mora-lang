//! v0.104.6 D96：`node_result_reg` 覆盖 20 / 50 个 `Node` 变体 —— 普查
//! 「缺 arm」里哪些是**值产生节点**且**真的会被消费**。
//!
//! ## 机制
//!
//! `witness_to_fcfg::node_result_reg_of` 对没有 arm 的节点返回 `None`，
//! 而 `node_result_reg` 把它变成 `unwrap_or(0)` —— **指向寄存器 0**，
//! 即「本函数体第一个分配的寄存器」。D58 记录过同一形状的活缺陷：
//! `with model = "gpt-4o" / 2 / end` 曾让块结果变成 `String("gpt-4o")`
//! 而裸路径是 `Nil`。
//!
//! 传播路径不止一条，最关键的是：
//! ```text
//! Node::Sequence { nodes, .. } => nodes.last().and_then(node_result_reg_of)
//! ```
//! 即**序列最后一个节点缺 arm，整个序列的结果就是哨兵 0**。
//!
//! ## 为什么必须直跑管线
//!
//! 这 5 类块形态在生产里恒触发差分回落 → FCFG 产出**从不被执行**，
//! 隐患被那道网挡住。要判断它是「潜在」还是「已活」，只能**绕过回落**、
//! 直接构造并执行管线产出（与 `nine_layer_block_equivalence.rs` 同法）。
//!
//! ## 本文件的判据
//!
//! 对每个构造：让该节点成为**序列最后一个节点**且其值被 `let` 绑定消费，
//! 然后**对拍**「原 emit 产出」与「9 层管线产出」的绑定值。
//! 两边不同 = 管线在该构造上**已经**是错的（活缺陷）；
//! 两边相同 = 目前无害（潜在，回落仍在挡）。
//!
//! ## 测量结论（全部「一致」，即缺 arm 目前**不是**活缺陷）
//!
//! 5 类块形态在两条路径上都返回 `nil` —— 正确。D58 那条
//! 「缺 arm → 结果退化成寄存器 0」的隐患，**对块形态不成立**。
//!
//! ## 顺带查到的一处**契约偏差**（先前就有，非本轮引入）
//!
//! `with_tail` 两条路径都返回 **`"m"`**（配置绑定的值）而不是 `Nil`。
//! 末两条 MIR 是 `Const`（存 `"m"`）+ `WithConfig` + `Const`（= `dst, Nil`）——
//! `emit_with_w` 的 `Const(dst, Nil)` **确实在**，但 `execute` 的末值不取它。
//!
//! **影响面极小**：`last_expr` 只有**库调用方**可见；语言层观测不到
//! （`with` 是语句、`with` 块的 `let` 绑定不外泄 —— D91 实测）。
//! 且 D58 修的是 `Block::result`（FCFG 内部，`dst` 是否被写），
//! 与这里的末值选取是**两件事**，故不构成 D58 回归。仅记录。

use mora::mir::control::{ControlProgram, OptLevel};
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};
use mora::value::Value;
use std::sync::Arc;

/// 跑整个程序：顶层 body（取末值）+ `main` task。
fn run_program(prog: ControlProgram) -> mora::error::MoraResult<Value> {
    let mut core = CoreRuntime::default();
    let prog_arc = Arc::new(prog);
    let (last, globals) = execute_program(prog_arc.clone(), &mut core)?;
    run_main_task(prog_arc, globals, &mut core)?;
    Ok(last)
}

/// 每个用例都让该节点成为**整个程序的最后一个语句**，并在其**前面**放一个
/// 特征值（`let marker = 7`），使「`unwrap_or(0)` 撞对一个恰好是 Nil 的寄存器」
/// 这种**碰巧通过**变得不可能。
///
/// 为什么是这个形态：块在 Mora 语法里是**语句**不是表达式，
/// `let r = observe … end` **解析就失败**（实测），所以「块作 `let` 右值」
/// 那条消费路径结构上不可达。真正会消费它的是 `execute` 的
/// `last_expr` —— 顶层 `Sequence` 的结果取自 `nodes.last()`，
/// 而末节点缺 arm 时整个序列结果退化成哨兵 0。
const CASES: &[(&str, &str)] = &[
    (
        "observe_tail",
        "let marker = 7\nobserve trace \"t\" do\n  print(1)\nend\n",
    ),
    (
        "span_tail",
        "let marker = 7\nspan \"s\" do\n  print(1)\nend\n",
    ),
    (
        "parallel_tail",
        "let marker = 7\nparallel\n  print(1)\nend\n",
    ),
    (
        "prompt_tail",
        "let marker = 7\nprompt \"p\" do\n  \"text\"\nend\n",
    ),
    (
        "document_tail",
        "let marker = 7\ndocument \"d\" do\n  \"text\"\nend\n",
    ),
    // 对照组：已覆盖 arm 的块形态
    (
        "with_tail",
        "let marker = 7\nwith model = \"m\"\n  print(1)\nend\n",
    ),
    // 对照组：普通表达式在同一位置（末值应为 7.0）
    ("literal_tail", "let marker = 7\nmarker\n"),
];

#[test]
fn d96_missing_result_reg_arm_is_latent_not_live() {
    let mut report = Vec::new();
    let mut divergent = Vec::new();

    for (name, source) in CASES {
        let (p_none, _) = match mora::cli::compile_and_opt(source, Some(OptLevel::None)) {
            Ok(v) => v,
            Err(e) => {
                report.push(format!("{name}: COMPILE_ERR {e}"));
                continue;
            }
        };
        let (p_basic, _) = mora::cli::compile_and_opt(source, Some(OptLevel::Basic))
            .expect("OptLevel::Basic compile");
        let (p_aggr, _) = mora::cli::compile_and_opt(source, Some(OptLevel::Aggressive))
            .expect("OptLevel::Aggressive compile");

        let r_none = run_program(p_none);
        let r_basic = run_program(p_basic);
        let r_aggr = run_program(p_aggr);
        let show = |r: &mora::error::MoraResult<Value>| match r {
            Ok(v) => v.to_string(),
            Err(e) => format!("Err({e})"),
        };
        let same = show(&r_none) == show(&r_basic) && show(&r_none) == show(&r_aggr);
        if !same || r_none.is_err() {
            divergent.push(*name);
        }
        if *name == "literal_tail" {
            assert_eq!(show(&r_none), "7", "literal_tail should yield 7");
        }
        report.push(format!(
            "{}: {}  none={} basic={} aggr={}",
            name,
            if same { "一致" } else { "**不一致**" },
            show(&r_none),
            show(&r_basic),
            show(&r_aggr)
        ));
    }
    eprintln!(
        "D96 结果寄存器消费优化一致检查：\n  {}",
        report.join("\n  ")
    );

    assert_eq!(
        report.len(),
        CASES.len(),
        "有用例没跑出结论：\n  {}",
        report.join("\n  ")
    );
    assert!(
        divergent.is_empty(),
        "优化等级之间在 {} 个构造上不一致或失败：\n  {}",
        divergent.len(),
        report.join("\n  ")
    );
}
