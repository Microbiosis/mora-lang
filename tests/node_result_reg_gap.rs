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
//! `emit_with_w` 的 `Const(dst, Nil)` **确实在**，但 `run_mir` 的末值不取它。
//!
//! **影响面极小**：`last_expr` 只有**库调用方**可见；语言层观测不到
//! （`with` 是语句、`with` 块的 `let` 绑定不外泄 —— D91 实测）。
//! 且 D58 修的是 `Block::result`（FCFG 内部，`dst` 是否被写），
//! 与这里的末值选取是**两件事**，故不构成 D58 回归。仅记录。

use mora::interpreter::Interpreter;
use mora::mir::MirFunction;
use mora::mir::ssa::OptLevel;
use mora::mir::vm::{run_main_task, run_mir};
use mora::mir::witness_to_fcfg::witness_to_fcfg;
use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn finish(mut f: MirFunction, level: OptLevel) -> MirFunction {
    mora::mir::optimize::apply_rules(&mut f);
    if level.enabled() {
        mora::mir::opt::optimize(&mut f, level);
    }
    f
}

fn execute(func: MirFunction) -> Result<Value, String> {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = Arc::new(func);
    let last = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )?;
    run_main_task(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )?;
    Ok(last)
}

/// 每个用例都让该节点成为**整个程序的最后一个语句**，并在其**前面**放一个
/// 特征值（`let marker = 7`），使「`unwrap_or(0)` 撞对一个恰好是 Nil 的寄存器」
/// 这种**碰巧通过**变得不可能。
///
/// 为什么是这个形态：块在 Mora 语法里是**语句**不是表达式，
/// `let r = observe … end` **解析就失败**（实测），所以「块作 `let` 右值」
/// 那条消费路径结构上不可达。真正会消费它的是 `run_mir` 的
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
        let (func, witnesses) = match ParserV3::compile(source) {
            Ok(v) => v,
            Err(e) => {
                report.push(format!("{name}: COMPILE_ERR {e}"));
                continue;
            }
        };
        let fcfg = witness_to_fcfg(&witnesses);
        let (body, n_regs) = mora::mir::fcfg_lower::lower_fcfg(&fcfg);
        let pipeline_func = MirFunction {
            params: vec![],
            body,
            n_regs,
            effects: func.effects.clone(),
        };
        let level = OptLevel::default();
        let a = execute(finish(func.clone(), level));
        let b = execute(finish(pipeline_func.clone(), level));
        let same = match (&a, &b) {
            (Ok(x), Ok(y)) => format!("{x}") == format!("{y}"),
            (Err(_), Err(_)) => true,
            _ => false,
        };
        if !same {
            divergent.push(*name);
        }
        report.push({
            let show = |r: &Result<Value, String>| match r {
                Ok(v) => v.to_string(),
                Err(e) => format!("Err({})", e),
            };
            format!(
                "{}: {}  original={:?} pipeline={:?}\n      orig_mir={:?}",
                name,
                if same { "一致" } else { "**不一致**" },
                show(&a),
                show(&b),
                func.body
                    .iter()
                    .map(|i| format!("{:?}", i)
                        .split(['(', ' '])
                        .next()
                        .unwrap_or("?")
                        .to_string())
                    .collect::<Vec<_>>()
            )
        });
    }
    eprintln!("D96 结果寄存器消费对拍：\n  {}", report.join("\n  "));

    assert_eq!(
        report.len(),
        CASES.len(),
        "有用例没跑出结论：\n  {}",
        report.join("\n  ")
    );
    assert!(
        divergent.is_empty(),
        "管线在 {} 个构造上与原路径**不一致** —— `node_result_reg` 缺 arm 已经是**活缺陷**，\
         不再是潜在隐患。\n  {}",
        divergent.len(),
        report.join("\n  ")
    );
}
